use itertools::Itertools;
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
	parse, punctuated::Punctuated, token::Comma, Expr, ExprPath, FnArg, GenericArgument, Ident,
	ItemFn, Pat, PatIdent, Path, PathArguments, PathSegment, ReturnType, Type,
};

use std::sync::RwLock;

/// Whether or not the module's pipeline system has been generated already.
static MODULE_GEN: RwLock<bool> = RwLock::new(false);

/// Generates handle_msgname_ok and handle_msgname_err emitters for the function, providing
/// better Result ergonomics. Allocates ok and error values via JSON serialization.
#[proc_macro_attribute]
pub fn with_result_message(_args: TokenStream, input: TokenStream) -> TokenStream {
	let mut input: ItemFn = parse(input).unwrap();

	let msg_ident = input.sig.ident;
	let inner_ident = Ident::new(
		&format!("inner_{}", msg_ident.to_string()),
		Span::call_site(),
	);
	let msg_name = msg_ident.to_string().replace("handle_", "");

	input.sig.ident = inner_ident.clone();

	let args = input.sig.inputs.clone();
	let arg_names: Punctuated<Expr, Comma> = input
		.sig
		.inputs
		.clone()
		.into_iter()
		.filter_map(|arg| match arg {
			FnArg::Typed(arg) => Some(arg.pat),
			FnArg::Receiver(_) => panic!("Cannot use self in handler."),
		})
		.filter_map(|pat| match *pat {
			Pat::Ident(PatIdent { ident, .. }) => Some(Expr::Path(ExprPath {
				attrs: Vec::new(),
				qself: None,
				path: Path {
					leading_colon: None,
					segments: {
						let mut p = Punctuated::new();
						p.push(PathSegment {
							ident,
							arguments: PathArguments::None,
						});
						p
					},
				},
			})),
			_ => panic!("Could not parse arguments."),
		})
		.collect();

	// Use serde_json to serialize the Ok and Err values, or inline them if
	// they are native wasm values
	let (ok_ser, err_ser) = match input.sig.output {
		ReturnType::Default => None,
		ReturnType::Type(_, ref ty) => Some(ty),
	}
	.and_then(|ty| match &**ty {
		Type::Path(p) => Some(p),
		_ => None,
	})
	.and_then(|p| {
		p.path.segments.last().and_then(|s| match &s.arguments {
			PathArguments::AngleBracketed(br) => Some(br.args.clone()),
			_ => None,
		})
	})
	.and_then(|args| {
		args.iter()
			.map(
				// If a type argument is provided to the Result returned, and it is a
				// primitive accepted by FromToNativeWasm, generate inline serialization
				// for it. Otherwise, use serde
				|arg| {
					match arg {
						GenericArgument::Type(ty) => Some(ty),
						_ => None,
					}
					.and_then(|ty| match ty {
						Type::Path(pat) => pat.path.get_ident(),
						_ => None,
					})
					.and_then(|ty_ident| match ty_ident.to_string().as_str() {
						"i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
							Some(quote! {
								let arg = WasmPtr::from_native((&e as *const u8) as i32);
							})
						}
						_ => None,
					})
					.unwrap_or(quote! {
						// Allocate a memory cell for the Ok or Err value
						let init_size: u32 = 0;
						let msg_kind = CString::new("allocate").expect("Invalid scheduler message kind encoding");
						send_message(1,
									 WasmPtr::from_native(msg_kind.as_ptr() as i32),
									 WasmPtr::from_native((&init_size as *const u32) as i32));
						let res_buf = ALLOC_RESULT.write().unwrap().take().unwrap().unwrap();

						use serde_json::to_vec;
						use vision_utils::actor::{send_message, address};
						use wasmer::{WasmPtr, FromToNativeWasmType};

						let v_bytes = to_vec(&e).unwrap();

						let msg_kind = CString::new("grow").expect("Invalid scheduler message kind encoding");
						let msg_len = v_bytes.len();

						send_message(res_buf,
									 WasmPtr::from_native(msg_kind.as_ptr() as i32),
									 WasmPtr::from_native((&msg_len as *const usize) as i32));

						let msg_kind = CString::new("write").expect("Invalid scheduler message kind encoding");

						for (i, b) in v_bytes.into_iter().enumerate() {
							// Space for offset u32, and val u8
							let offset: [u8; 4] = (i as u32).to_le_bytes();
							let mut write_args: [u8; 5] = [0, 0, 0, 0, b];

							for (i, b) in offset.into_iter().enumerate() {
								write_args[i] = b;
							}

							send_message(res_buf,
										 WasmPtr::from_native(msg_kind.as_ptr() as i32),
										 WasmPtr::from_native((&write_args as *const u8) as i32));
						}

						let arg = WasmPtr::from_native((&res_buf as *const u32) as i32);
					})
				},
			)
			.collect_tuple()
	})
	.expect("Handler needs a Result<Ok, Err> return to be eligible.");

	let expanded = quote! {
		pub fn #msg_ident(#args) {
			use wasmer::{WasmPtr, Array, FromToNativeWasmType};
			use vision_utils::actor::{send_message, address};


			// Return the address of the cell to the caller
			match #inner_ident(#arg_names) {
				Ok(e) => {
					#ok_ser

					let handler_name = CString::new(format!("{}_ok", #msg_name)).expect("Invalid scheduler message kind encoding");
					send_message(from, WasmPtr::from_native(handler_name.as_ptr() as i32), arg);
				},
				Err(e) => {
					#err_ser

					let handler_name = CString::new(format!("{}_err", #msg_name)).expect("Invalid scheduler message kind encoding");
					send_message(from, WasmPtr::from_native(handler_name.as_ptr() as i32), arg);
				},
			};
		}

		#input
	};

	// Sequencing for operations dealing with Results that should only be appended to
	// a module once. TODO: Do this at the module level, without macro state.
	let pipeline = quote! {
		use std::{ffi::CString, sync::RwLock};
		use serde::Serialize;

		static ALLOC_RESULT: RwLock<Option<Result<Address, Address>>> = RwLock::new(None);

		#[wasm_bindgen::prelude::wasm_bindgen]
		pub fn handle_allocate_ok(addr: Address) {
			ALLOC_RESULT.write().unwrap().replace(Ok(addr));
		}


		#[wasm_bindgen::prelude::wasm_bindgen]
		pub fn handle_allocate_err(addr: Address) {
			ALLOC_RESULT.write().unwrap().replace(Err(addr));
		}
	};

	let mut pipeline_gen = MODULE_GEN.write().unwrap();

	TokenStream::from(if *pipeline_gen {
		expanded
	} else {
		*pipeline_gen = true;

		quote! {
			#expanded
			#pipeline
		}
	})
}
