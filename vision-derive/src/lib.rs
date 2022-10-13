use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
	parse, punctuated::Punctuated, token::Comma, Expr, ExprPath, FnArg, Ident, ItemFn, Pat,
	PatIdent, Path, PathArguments, PathSegment,
};

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
	let serializer = quote! {
	}

	let expanded = quote! {
		pub fn #msg_ident(#args) {
			use wasmer::{WasmPtr, Array, FromToNativeWasmType};
			use vision_utils::actor::{send_message, address};

			// Allocate a memory cell for the Ok or Err value
			let init_size: u32 = 0;
			let msg_kind = CString::new("allocate").expect("Invalid scheduler message kind encoding");
			send_message(1,
						 WasmPtr::from_native(msg_kind.as_ptr() as i32),
						 WasmPtr::from_native((&init_size as *const u32) as i32));
			let res_buf = ALLOC_RESULT.write().unwrap().take().unwrap().unwrap();
			let ref_res_buf = WasmPtr::from_native((&res_buf as *const u32) as i32);

			// Return the address of the cell to the caller
			match #inner_ident(#arg_names) {
				Ok(v) => {
					let handler_name = CString::new(format!("{}_ok", #msg_name)).expect("Invalid scheduler message kind encoding");

					write_t_bytes(v, res_buf);
					send_message(from, WasmPtr::from_native(handler_name.as_ptr() as i32), ref_res_buf);
				},
				Err(e) => {
					let handler_name = CString::new(format!("{}_err", #msg_name)).expect("Invalid scheduler message kind encoding");

					write_t_bytes(e, res_buf);
					send_message(from, WasmPtr::from_native(handler_name.as_ptr() as i32), ref_res_buf);
				},
			};
		}

		use std::{ffi::CString, sync::RwLock};
		use serde::Serialize;

		static ALLOC_RESULT: RwLock<Option<Result<Address, Address>>> = RwLock::new(None);

		pub fn handle_allocate_ok(addr: Address) {
			ALLOC_RESULT.write().unwrap().replace(Ok(addr));
		}

		pub fn handle_allocate_err(addr: Address) {
			ALLOC_RESULT.write().unwrap().replace(Err(addr));
		}

		// Serialize the Ok or Err value, and write it to the cell
		fn write_t_bytes(v: impl Serialize, buf: Address) {
			use serde_json::to_vec;
			use vision_utils::actor::{send_message, address};
			use wasmer::{WasmPtr, FromToNativeWasmType};

			let v_bytes = to_vec(&v).unwrap();

			let msg_kind = CString::new("grow").expect("Invalid scheduler message kind encoding");
			let msg_len = v_bytes.len();

			send_message(buf,
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

				send_message(buf,
							 WasmPtr::from_native(msg_kind.as_ptr() as i32),
							 WasmPtr::from_native((&write_args as *const u8) as i32));
			}
		}

		#input
	};

	TokenStream::from(expanded)
}
