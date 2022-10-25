use itertools::Itertools;
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use syn::{
	parse, parse_quote, punctuated::Punctuated, token::Comma, Expr, ExprPath, FnArg,
	GenericArgument, Ident, ItemFn, Pat, PatIdent, Path, PathArguments, PathSegment, ReturnType,
	Type, TypePath,
};

use std::sync::RwLock;

/// For a message handler, generates:
/// - A rust binding for calling the method, and using it in a synchronous way
/// - Macros for including message handlers that pipeline messages back to the
/// client function
/// - A pipeline channel, which forwards results from the generated message
/// handlers back to the client function
/// - A wrapper implementing an actual message handler (doesn't return anything,
/// and accepts only simple copy types). This is achieved by serializing
/// non-copy parameters to memory cells (see allocator service)
#[proc_macro_attribute]
pub fn with_bindings(_args: TokenStream, input: TokenStream) -> TokenStream {
	let mut input: ItemFn = parse(input).unwrap();

	// The function must be a message handler: it must have a handle_ prefix
	let msg_name = input.sig.ident.to_string().strip_prefix("handle_")
		.expect("Must be a message handler starting with handle_, followed by the name of the message being handled.");

	// Messages have ABI bindings generated that allow easy UX:
	// - A pipeline mutex that allows bubbling results from handlers up to
	// originating callers
	// - Handler methods that
	let msg_pipeline_name = Ident::new(
		&format!("PIPELINE_{}", msg_name.to_ascii_uppercase()),
		Span::call_site(),
	);
	let msg_macro_name = Ident::new(&format!("use_{}", msg_name), Span::call_site());
	let msg_ret_handler_name = Ident::new(&format!("handle_{}", msg_name), Span::call_site());

	let msg_ident = input.sig.ident;

	// Reattach #[] attrs to the newly generated wrapper function
	let extern_attrs = TokenStream2::from_iter(
		input
			.attrs
			.drain(..input.attrs.len())
			.map(|tok| tok.to_token_stream()),
	);
	let inner_ident = Ident::new(
		&format!("inner_{}", msg_ident.to_string()),
		Span::call_site(),
	);
	input.sig.ident = inner_ident.clone();

	// Save argument names for proxying call to inner handler
	let mut args = input.sig.inputs.clone();
	let args_iter = input
		.sig
		.inputs
		.clone()
		.into_iter()
		.filter_map(|arg| match arg {
			FnArg::Typed(arg) => Some(arg),
			FnArg::Receiver(_) => panic!("Cannot use self in handler."),
		});
	let arg_names: Punctuated<Expr, Comma> = args_iter
		.clone()
		.map(|arg| arg.pat)
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

	// Use serde_json to serialize the return value of the function
	let ser_type = match input.sig.output {
		ReturnType::Default => None,
		ReturnType::Type(_, ref ty) => Some(ty),
	}
	.and_then(|ty| match &**ty {
		Type::Path(p) => Some(p),
		_ => None,
	})
	.and_then(|p| p.path.segments.last().map(|p| p.ident));

	let ret_type: TypePath;
	let ser = ser_type
		.map(|ret| {
			// If the return value is a copy type, use its native representation
			match ret.to_string().as_str() {
				"Address" | "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
					Some(quote! {
						let arg = WasmPtr::from_native((&v as *const u8) as i32);
						ret_type = ret;
					})
				}
				_ => {
					ret_type = parse_quote! {vision_utils::types::Address};
					None
				}
			}
			// Otherwise, use serde to pass in a memory cell address
			.unwrap_or(quote! {
				// Allocate a memory cell for the value
				let init_size: u32 = 0;
				let msg_kind = CString::new("allocate").expect("Internal allocator error");
				send_message(vision_utils::types::ALLOCATOR_ADDR,
							 WasmPtr::from_native(msg_kind.as_ptr() as i32),
							 WasmPtr::from_native((&init_size as *const u32) as i32));
				let res_buf = ALLOC_RESULT.write().unwrap().take().unwrap().unwrap();

				use serde_json::to_vec;
				use serde::Serialize;
				use vision_utils::actor::{send_message, address};
				use wasmer::{WasmPtr, FromToNativeWasmType};

				let v_bytes = to_vec(&v).unwrap();

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
		})
		.unwrap_or(quote! {
			let arg = v;
		});

	fn gen_der(args: impl Iterator<Item = FnArg>) -> TokenStream2 {
		// Use serde_json to deserialize the parameters of the function
		let mut der = TokenStream2::new();
		let arg_types_iter = args_iter
			.map(|arg| {
				(
					match *arg.pat {
						Pat::Ident(pat) => Some(pat.ident),
						_ => None,
					}
					.expect("Handlers may not have non-identifier arguments"),
					arg.ty,
				)
			})
			.map(|(ident, ty)| {
				(
					ident,
					match *ty {
						Type::Path(pat) => pat.path.segments.last(),
						_ => None,
					}
					.expect("Handlers may not have non-identifier argument types")
					.ident,
				)
			});

		for (i, (pat, ty)) in arg_types_iter.enumerate() {
			match ty.to_string().as_str() {
				// No work needs to be done for copy types, since they are passed in as their values
				"Address" | "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
					der = quote! {
						#der

						let #pat = #pat as #ty;
					};
				}
				_ => {
					der = quote! {
						#der

						let #pat = {
							use serde_json::to_vec;
							use beacon_dao_allocator::PIPELINE;

							// Read until a } character is encoutnered (this should be JSON)
							// or the results buffer isn't expanding
							let cell = #pat;
							let msg_kind = CString::new("read").expect("Internal allocator error");
							let msg_name = WasmPtr::from_native(msg_kind.as_ptr() as i32);
							let mut buf = Vec::new();

							for i in 0..u32::MAX {
								send_message(cell, msg_name, WasmPtr::from_native((&i as *const u32) as i32));

								if let Some(next) = PIPELINE.write().unwrap().take() {
									buff.push(next);
								} else {
									break;
								}
							}

							// This should not happen, since the wrapper method being used conforms to this practice
							serde_json::from_slice(&buf).expect("Failed to deserialize input parameters.")
						};
					};

					// Since a heap-allocated proxy was used to read the argument, accept it as an Address
					if let FnArg::Typed(ref mut typed_arg) = args[i] {
						typed_arg.ty = parse_quote! {
							vision_utils::types::Address
						};
					}
				}
			}
		}

		der
	}

	// Use the serializer to return a WASM-compatible response to consumers
	// and generate bindings that streamline sending the message, and getting a
	// response
	TokenStream::from(quote! {
		#extern_attrs
		pub fn #msg_ident(#args) {
			#der

			let v = #inner_ident(#arg_names);

			#ser

			let handler_name = CString::new(#msg_name).expect("Invalid scheduler message kind encoding");
			send_message(from, WasmPtr::from_native(handler_name.as_ptr() as i32), arg);
		}

		pub static #msg_pipeline_name: std::sync::RwLock<Option<#ser_type>>> = std::sync::RwLock::new(None);

		#[macro_export]
		macro_rules! #msg_macro_name {
			() => {
				#[wasm_bindgen::prelude::wasm_bindgen]
				pub fn #msg_ret_handler_name(from: Address, arg: #ret_type) {
					#der
					#msg_pipeline_name.write().unwrap().replace(arg);
				}
			}
		}
	})
}

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

	let extern_attrs = TokenStream2::from_iter(
		input
			.attrs
			.drain(..input.attrs.len())
			.map(|tok| tok.to_token_stream()),
	);
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
						send_message(vision_utils::types::ALLOCATOR_ADDR,
									 WasmPtr::from_native(msg_kind.as_ptr() as i32),
									 WasmPtr::from_native((&init_size as *const u32) as i32));
						let res_buf = ALLOC_RESULT.write().unwrap().take().unwrap().unwrap();

						use serde_json::to_vec;
						use serde::Serialize;
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
		#extern_attrs
		pub fn #msg_ident(#args) {
			use wasmer::{WasmPtr, Array, FromToNativeWasmType};
			use vision_utils::actor::{send_message, address};
			use std::{ffi::CString};

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
		static ALLOC_RESULT: std::sync::RwLock<Option<Result<Address, Address>>> = std::sync::RwLock::new(None);

		#[wasm_bindgen::prelude::wasm_bindgen]
		pub fn handle_allocate_ok(addr: Address) {
			ALLOC_RESULT.write().unwrap().replace(Ok(addr));
		}


		#[wasm_bindgen::prelude::wasm_bindgen]
		pub fn handle_allocate_err(addr: Address) {
			ALLOC_RESULT.write().unwrap().replace(Err(addr));
		}
	};

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
