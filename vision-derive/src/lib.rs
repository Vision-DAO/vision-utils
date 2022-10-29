use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use std::iter;
use syn::{
	parse, parse_quote, punctuated::Punctuated, token::Colon, token::Comma, Expr, ExprPath, FnArg,
	Ident, ItemFn, Pat, PatIdent, PatType, Path, PathArguments, PathSegment, ReturnType, Type,
	TypePath,
};

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
	let msg_full_name = input.sig.ident.to_string();
	let msg_name = msg_full_name.strip_prefix("handle_")
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

	// Save argument names for proxying call to inner handlers
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

	let original_args: Punctuated<PatType, Comma> =
		Punctuated::from_iter(args_iter.clone().skip(1));

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

	fn gen_der(
		args_iter: impl Iterator<Item = PatType>,
		mut args: Option<&mut Punctuated<FnArg, Comma>>,
	) -> TokenStream2 {
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
						Type::Path(pat) => pat.path.segments.last().cloned(),
						_ => None,
					}
					.expect("Handlers may not have non-identifier argument types")
					.ident
					.clone(),
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
							let msg_kind = std::ffi::CString::new("read").expect("Internal allocator error");
							let msg_name: WasmPtr<u8, wasmer::Array> = wasmer::FromToNativeWasmType::from_native(msg_kind.as_ptr() as i32);
							let mut buf = Vec::new();

							for i in 0..u32::MAX {
								send_message(cell, msg_name, wasmer::FromToNativeWasmType::from_native((&i as *const u32) as i32));

								if let Some(next) = PIPELINE.write().unwrap().take() {
									buf.push(next);
								} else {
									break;
								}
							}

							// This should not happen, since the wrapper method being used conforms to this practice
							serde_json::from_slice(&buf).expect("Failed to deserialize input parameters.")
						};
					};

					// Since a heap-allocated proxy was used to read the argument, accept it as an Address
					if let Some(ref mut args) = args {
						if let FnArg::Typed(ref mut typed_arg) = args[i] {
							typed_arg.ty = parse_quote! {
								vision_utils::types::Address
							};
						}
					}
				}
			}
		}

		der
	}

	fn gen_ser(
		args_iter: impl Iterator<Item = PatType> + Clone,
	) -> (TokenStream2, Vec<Option<TypePath>>) {
		let mut type_buf: Vec<Option<TypePath>> = Vec::new();

		let args_iter = args_iter
			.filter_map(|arg| match *arg.pat {
				Pat::Ident(id) => Some((id, arg.ty)),
				_ => panic!("Arguments must be named"),
			})
			.filter_map(|(id, pat)| match *pat {
				Type::Path(ty) => Some((id, ty)),
				_ => panic!("Arguments must be typed"),
			});

		let total_bytes = args_iter.clone().fold(0, |acc, (_, ser_type)| {
			acc + match ser_type
				.path
				.segments
				.last()
				.map(|p| p.ident.clone())
				.expect("Invalid Type")
				.to_string()
				.as_str()
			{
				"i8" | "u8" => 8,
				"i16" | "u16" => 16,
				"i64" | "u64" => 64,
				_ => 32, // Address (and anything serialied to an address), i32, u32
			}
		});

		let mut gen_buf = quote! {
			let mut v: Vec<u8> = Vec::with_capacity(#total_bytes as usize);
		};

		for (id, ser_type) in args_iter {
			let ser_type_ident = ser_type
				.path
				.segments
				.last()
				.map(|p| p.ident.clone())
				.expect("Invalid type");

			let ser = {
				// If the return value is a copy type, use its native representation
				match ser_type_ident.to_string().as_str() {
					"Address" | "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
						type_buf.push(Some(ser_type));
						Some(quote! {
							let mut bytes = Vec::from((v.as_ptr() as i32 + v.len() as i32).to_le_bytes());
							let #id: WasmPtr<u8, wasmer::Array> = wasmer::FromToNativeWasmType::from_native(v.as_ptr() as i32 + v.len() as i32);
							v.append(&mut bytes);
						})
					}
					_ => {
						type_buf.push(Some(parse_quote! {vision_utils::types::Address}));
						None
					}
				}
				// Otherwise, use serde to pass in a memory cell address
				.unwrap_or(quote! {
					// Allocate a memory cell for the value
					let init_size: u32 = 0;
					let msg_kind = std::ffi::CString::new("allocate").expect("Internal allocator error");
					send_message(vision_utils::types::ALLOCATOR_ADDR,
								 wasmer::FromToNativeWasmType::from_native(msg_kind.as_ptr() as i32),
								 wasmer::FromToNativeWasmType::from_native((&init_size as *const u32) as i32));
					let res_buf = ALLOC_RESULT.write().unwrap().take().unwrap().unwrap();

					use serde_json::to_vec;
					use serde::Serialize;
					use vision_utils::actor::{address};

					let v_bytes = to_vec(&#id).unwrap();

					let mut bytes = Vec::from((v.as_ptr() as i32 + v.len() as i32).to_le_bytes());
					let #id: WasmPtr<u8, wasmer::Array> = wasmer::FromToNativeWasmType::from_native(v.as_ptr() as i32 + v.len() as i32);
					v.append(&mut bytes);

					let msg_kind = std::ffi::CString::new("grow").expect("Invalid scheduler message kind encoding");
					let msg_len = v_bytes.len();

					send_message(res_buf,
								 wasmer::FromToNativeWasmType::from_native(msg_kind.as_ptr() as i32),
								 wasmer::FromToNativeWasmType::from_native((&msg_len as *const usize) as i32));

					let msg_kind = std::ffi::CString::new("write").expect("Invalid scheduler message kind encoding");

					for (i, b) in v_bytes.into_iter().enumerate() {
						// Space for offset u32, and val u8
						let offset: [u8; 4] = (i as u32).to_le_bytes();
						let mut write_args: [u8; 5] = [0, 0, 0, 0, b];

						for (i, b) in offset.into_iter().enumerate() {
							write_args[i] = b;
						}

						send_message(res_buf,
									 wasmer::FromToNativeWasmType::from_native(msg_kind.as_ptr() as i32),
									 wasmer::FromToNativeWasmType::from_native((&write_args as *const u8) as i32));
					}
				})
			};

			gen_buf = quote! {
				#gen_buf
				#ser
			};
		}
		(gen_buf, type_buf)
	}

	let mut ser_type = None;
	let (ser, arg_type) = match input.sig.output.clone() {
		ReturnType::Default => gen_ser(iter::empty()),
		ReturnType::Type(_, ty) => {
			ser_type = Some(*ty.clone());
			gen_ser(iter::once(PatType {
				attrs: Vec::new(),
				pat: parse_quote! {arg},
				colon_token: Colon::default(),
				ty: ty.clone(),
			}))
		}
	};

	let der = gen_der(args_iter, Some(&mut args));

	let mut ret_handler_args: Punctuated<PatType, Comma> = Punctuated::new();
	let mut ret_type: Option<TypePath> = None;
	if let Some(arg_type) = arg_type.get(0).cloned().flatten() {
		ret_type = Some(arg_type.clone());
		ret_handler_args.push_value(PatType {
			attrs: Vec::new(),
			pat: parse_quote! {arg},
			colon_token: Colon::default(),
			ty: Box::new(Type::Path(arg_type.clone())),
		})
	}

	let ret_der = gen_der(ret_handler_args.into_iter(), None);
	let (client_arg_ser, _) = gen_ser(original_args.clone().into_iter());

	let further_processing = match arg_type.get(0).cloned().flatten() {
		Some(_) => quote! {
			#ser

			let handler_name = std::ffi::CString::new(#msg_name).expect("Invalid scheduler message kind encoding");
			send_message(from, wasmer::FromToNativeWasmType::from_native(handler_name.as_ptr() as i32), arg);
		},
		None => quote! {},
	};

	// Use the serializer to return a WASM-compatible response to consumers
	// and generate bindings that streamline sending the message, and getting a
	// response
	let mut gen = quote! {
		#extern_attrs
		pub fn #msg_ident(#args) {
			use vision_utils::actor::send_message;

			#der

			let arg = #inner_ident(#arg_names);

			#further_processing
		}

		#input
	};

	let msg_name_vis = msg_name.to_string();
	let args_ptr = arg_names[1].clone();

	let msg_name_ident = Ident::new(msg_name, Span::call_site());

	// Include handlers for the response value if there is one
	if let Some(ret_type) = ret_type {
		gen = quote! {
			#gen

			pub static #msg_pipeline_name: std::sync::RwLock<Option<#ser_type>> = std::sync::RwLock::new(None);

			#[macro_export]
			macro_rules! #msg_macro_name {
				() => {
					pub fn #msg_ret_handler_name(from: vision_utils::types::Address, arg: #ret_type) {
						#ret_der
						#msg_pipeline_name.write().unwrap().replace(arg);
					}
				}
			}

			pub fn #msg_name_ident(to: vision_utils::types::Address, #original_args) -> Option<#ser_type> {
				use wasmer::{WasmPtr, FromToNativeWasmType};
				use vision_utils::actor::send_message;

				#client_arg_ser
				let msg_kind = std::ffi::CString::new(#msg_name_vis)
					.expect("Invalid scheduler message kind encoding");

				send_message(to,
							 wasmer::FromToNativeWasmType::from_native(msg_kind.as_ptr() as i32),
							 #args_ptr);

				#msg_pipeline_name.write().unwrap().take()
			}
		}
	} else {
		gen = quote! {
			#gen

			pub fn #msg_name_ident(to: vision_utils::types::Address, #original_args) {
				use wasmer::{WasmPtr, FromToNativeWasmType};
				use vision_utils::actor::send_message;

				#client_arg_ser
				let msg_kind = std::ffi::CString::new(#msg_name_vis)
					.expect("Invalid scheduler message kind encoding");

				send_message(to,
							 wasmer::FromToNativeWasmType::from_native(msg_kind.as_ptr() as i32),
							 #args_ptr);
			}
		}
	}

	TokenStream::from(gen)
}
