use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use sha2::{Digest, Sha256};
use syn::{
	parse, parse_macro_input, parse_quote, punctuated::Punctuated, token::Colon, token::Comma,
	AttributeArgs, Expr, ExprPath, FnArg, GenericArgument, Ident, ItemFn, Pat, PatIdent, PatType,
	Path, PathArguments, PathSegment, Type, TypePath,
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
pub fn with_bindings(args: TokenStream, input: TokenStream) -> TokenStream {
	let alloc_module: AttributeArgs = parse_macro_input!(args as AttributeArgs);
	let (alloc_module, extern_crate_pre): (Path, Path) = if alloc_module.len() > 0 {
		(
			parse_quote!(self),
			Path {
				leading_colon: None,
				segments: Punctuated::new(),
			},
		)
	} else {
		(
			parse_quote!(::vision_derive::beacon_dao_allocator),
			parse_quote!(::vision_derive),
		)
	};

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

	// Uniquely identify this version of the handler from other handlers
	let mut hasher = Sha256::new();
	hasher.update(&input.to_token_stream().to_string()[..20]);
	let handler_hash = hasher.finalize();
	let msg_ret_handler_name = Ident::new(
		&format!("handle_{:x}_{}_ret", handler_hash, msg_name),
		Span::call_site(),
	);
	let ret_name = format!("{}_{:x}_ret", msg_name, handler_hash);

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
	// Skip serializing the callback parametera
	let mut args = {
		let mut with_cb = input.sig.inputs.clone();
		with_cb.pop();

		// Ensure that previous arguments are not suffixed by a comma
		if let Some(last_arg) = with_cb.pop() {
			with_cb.push_value(last_arg.into_value());
		}

		with_cb
	};
	let args_iter = args.clone().into_iter().filter_map(|arg| match arg {
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
		alloc_module: &Path,
		extern_crate_pre: &Path,
		mut callback: Option<TokenStream2>,
	) -> TokenStream2 {
		// Use #extern_crate_pre::serde_json to deserialize the parameters of the function
		let mut der = TokenStream2::new();
		let arg_types: Vec<(usize, (Ident, Ident))> = args_iter
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
			})
			.enumerate()
			.collect();
		let clone_items = arg_types
			.iter()
			.map(|(_i, (pat, _))| quote! {let #pat = #pat.clone();})
			.collect::<Vec<TokenStream2>>();
		let clone_all = clone_items
			.clone()
			.into_iter()
			.enumerate()
			.map(|(i, item_clone)| {
				let also_clone = &clone_items[(i + 1)..];

				let mut buf = quote! {
					#item_clone
				};

				for to_clone in also_clone {
					buf = quote! {
						#buf
						#to_clone
					}
				}

				buf
			})
			.collect::<Vec<TokenStream2>>();

		for (i, (pat, ty)) in arg_types.into_iter() {
			let clone_all = &clone_all[i];

			// Nest the callback after all arguments are deserialized
			let callback = if i == 0 {
				callback
					.take()
					.map(|cb| {
						quote! {
							let #pat = #pat.clone();
							#clone_all

							#cb
						}
					})
					.unwrap_or(TokenStream2::new())
			} else {
				TokenStream2::new()
			};

			match ty.to_string().as_str() {
				// No work needs to be done for copy types, since they are passed in as their values
				"Address" | "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
					der = quote! {
						let #pat = #pat as #ty;
						let #pat = std::sync::Arc::new(std::sync::Mutex::new(Some(#pat)));

						{
							#der
							#callback
						};
					};
				}
				_ => {
					der = quote! {
						// Read until a } character is encoutnered (this should be JSON)
						// or the results buffer isn't expanding
						let cell = #pat;

						#clone_all
						#alloc_module::len(cell, Callback::new(move |len| {
							let mut buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
							let n_done = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

							for i in 0..len {
								buf.lock().unwrap().push(0);

								let buf = buf.clone();
								let n_done = n_done.clone();
								#clone_all
								#alloc_module::read(cell, i, Callback::new(move |val| {
									buf.lock().unwrap()[i as usize] = val;

									if n_done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == len - 1 {
										// This should not happen, since the wrapper method being used conforms to this practice
										let #pat = std::sync::Arc::new(std::sync::Mutex::new(Some(#extern_crate_pre::serde_json::from_slice(&buf.lock().unwrap()).expect("Failed to deserialize input parameters."))));

										#der
										#callback
									}
								}));
							}
						}));
					};

					// Since a heap-allocated proxy was used to read the argument, accept it as an Address
					if let Some(ref mut args) = args {
						if let FnArg::Typed(ref mut typed_arg) = args[i] {
							typed_arg.ty = parse_quote! {
								#extern_crate_pre::vision_utils::types::Address
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
		alloc_module: &Path,
		extern_crate_pre: &Path,
		mut callback: Option<TokenStream2>,
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
				"i64" | "u64" => 8,
				_ => 4, // Address (and anything serialied to an address), i32, u32
			}
		});

		let mut gen_buf = TokenStream2::new();

		let clone_items = args_iter
			.clone()
			.map(|(pat, _)| quote! {let #pat = #pat.clone();})
			.collect::<Vec<TokenStream2>>();
		let clone_all = clone_items
			.clone()
			.into_iter()
			.enumerate()
			.map(|(i, item_clone)| {
				let also_clone = &clone_items[(i + 1)..];

				let mut buf = quote! {
					#item_clone
				};

				for to_clone in also_clone {
					buf = quote! {
						#buf
						#to_clone
					}
				}

				buf
			})
			.collect::<Vec<TokenStream2>>();

		for (i, (id, ser_type)) in args_iter.enumerate() {
			let clone_all = &clone_all[i];

			let callback = if i == 0 {
				callback
					.take()
					.map(|cb| {
						quote! {
							let #id = #id.clone();
							#clone_all

							#cb
						}
					})
					.unwrap_or(TokenStream2::new())
			} else {
				TokenStream2::new()
			};

			let ser_type_ident = ser_type
				.path
				.segments
				.last()
				.map(|p| p.ident.clone())
				.expect("Invalid type");

			gen_buf = {
				// If the return value is a copy type, use its native representation
				match ser_type_ident.to_string().as_str() {
					"Address" | "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
						let min_equivalent = match ser_type_ident.to_string().as_str() {
							"u8" | "Address" | "u16" | "u32" => quote!{u32},
							"i8" | "i16" | "i32" => quote!{i32},
							"i64" => quote!{i64},
							"u64" => quote!{u64},
							_ => quote!{u32},
						};

						type_buf.push(Some(ser_type));
						Some(quote! {
							let mut bytes = Vec::from((#id as #min_equivalent).to_le_bytes());

							let #id = v.as_ptr() as i32 + v.len() as i32;
							drop(&#id);

							bytes.append(&mut v);
							let mut v = bytes;

							#callback
							#gen_buf
						})
					}
					_ => {
						type_buf.push(Some(
							parse_quote! {#extern_crate_pre::vision_utils::types::Address},
						));
						None
					}
				}
				// Otherwise, use serde to pass in a memory cell address
				.unwrap_or({
					quote! {
						let v_bytes = #extern_crate_pre::serde_json::to_vec(&#id).unwrap();

						let #id = v.as_ptr() as i32 + v.len() as i32;
						drop(&#id);

						// Allocate a memory cell for the value
						#alloc_module::allocate(#extern_crate_pre::vision_utils::types::ALLOCATOR_ADDR, v_bytes.len() as u32, #extern_crate_pre::vision_utils::types::Callback::new(move |res_buf: u32| {
							let mut cell_addr_bytes = Vec::from(res_buf.to_le_bytes());
							cell_addr_bytes.append(&mut v);
							let mut v = cell_addr_bytes;

							for (i, b) in v_bytes.iter().enumerate() {
								// Space for offset u32, and val u8
								#alloc_module::write(res_buf, i as u32, *b, #extern_crate_pre::vision_utils::types::Callback::new(|_| {}));

								#callback
								#gen_buf
							}
						}));
					}
				})
			};
		}

		gen_buf = quote! {
			use #extern_crate_pre::serde::Serialize;

			let mut v: Vec<u8> = Vec::with_capacity(#total_bytes as usize);
			let v_ptr = v.as_ptr() as i32;
			drop(&v_ptr);

			#gen_buf
		};

		(gen_buf, type_buf)
	}

	let ty = input
		.sig
		.inputs
		.last()
		.and_then(|cb| match cb {
			FnArg::Typed(ty) => Some(ty),
			_ => None,
		})
		.and_then(|cb_type| match &*cb_type.ty {
			Type::Path(pat) => pat.path.segments.last(),
			_ => None,
		})
		.and_then(|cb_ident| match cb_ident.ident.to_string().as_str() {
			"Callback" => Some(cb_ident),
			_ => panic!("callback must be a Callback<T>"),
		})
		.and_then(|cb_ident| match &cb_ident.arguments {
			PathArguments::AngleBracketed(cb_type) => cb_type.args.last(),
			_ => None,
		})
		.and_then(|cb_param_type| match cb_param_type {
			GenericArgument::Type(ty) => Some(ty),
			_ => None,
		})
		.expect("callback must be handled via a final `cb` argument");
	let ser_type = Some(ty.clone());
	let (ser, arg_type) = gen_ser(
		vec![
			PatType {
				attrs: Vec::new(),
				pat: parse_quote! {arg},
				colon_token: Colon::default(),
				ty: Box::new(ty.clone()),
			},
			// Identify returns as a response to a particular message call
			PatType {
				attrs: Vec::new(),
				pat: parse_quote! {msg_id},
				colon_token: Colon::default(),
				ty: parse_quote! {u32},
			},
		]
		.into_iter(),
		&alloc_module,
		&extern_crate_pre,
		Some(quote! {
			let handler_name = std::ffi::CString::new(#ret_name).expect("Invalid scheduler message kind encoding");
			send_message((*from.lock().unwrap()).unwrap(), handler_name.as_ptr() as i32, arg);
		}),
	);

	let mut ret_handler_args: Punctuated<PatType, Comma> = Punctuated::new();
	let mut ret_type: Option<TypePath> = None;
	if let Some(arg_type) = arg_type.get(0).cloned().flatten() {
		ret_type = Some(arg_type.clone());
		ret_handler_args.push_value(PatType {
			attrs: Vec::new(),
			pat: parse_quote! {arg},
			colon_token: Colon::default(),
			ty: Box::new(Type::Path(arg_type.clone())),
		});
		ret_handler_args.push_punct(Comma::default());
		ret_handler_args.push_value(PatType {
			attrs: Vec::new(),
			pat: parse_quote! {msg_id},
			colon_token: Colon::default(),
			ty: parse_quote! {u32},
		});
	}

	let msg_name_vis = msg_name.to_string();

	let client_return_deserialize_callback = quote! {
		let mut lock = #msg_pipeline_name.write().unwrap();

		let maybe_cb = lock.get_mut(msg_id.lock().unwrap().take().unwrap() as usize).unwrap().take();
		if let Some(callback) = maybe_cb {
			callback.call(arg.lock().unwrap().take().unwrap());
		}
	};

	let ret_der = gen_der(
		{
			let mut safe_args = ret_handler_args.clone();
			if let Some(mut arg) = safe_args.first_mut() {
				arg.ty = Box::new(ser_type.clone().unwrap());
			}

			safe_args.into_iter()
		},
		None,
		&alloc_module,
		&extern_crate_pre,
		Some(client_return_deserialize_callback),
	);
	let args_ptr = arg_names
		.iter()
		.nth(1)
		.cloned()
		.unwrap_or(parse_quote! {v_ptr});

	let (client_arg_ser, _) = gen_ser(
		{
			// Only generate msg_id if a response is expected
			let mut client_original_args = original_args.clone();

			client_original_args.push(PatType {
				attrs: Vec::new(),
				pat: parse_quote! {msg_id},
				colon_token: Colon::default(),
				ty: parse_quote! {u32},
			});
			client_original_args
		}
		.into_iter(),
		&alloc_module,
		&extern_crate_pre,
		Some(quote! {
			let msg_kind = std::ffi::CString::new(#msg_name_vis)
				.expect("Invalid scheduler message kind encoding");

			send_message(to,
				 msg_kind.as_ptr() as i32,
				 #args_ptr);
		}),
	);

	let further_processing = match arg_type.get(0).cloned().flatten() {
		Some(_) => quote! {
			let cb = {
				let from = from.clone();

				move |arg: #ser_type| {
					#ser
				}
			};
		},
		None => quote! {},
	};

	// Use the serializer to return a WASM-compatible response to consumers
	// and generate bindings that streamline sending the message, and getting a
	// response
	let consumed_arg_names: Punctuated<Expr, Comma> = arg_names
		.clone()
		.into_iter()
		.map(|arg: Expr| -> Expr {
			parse_quote! {#arg.lock().unwrap().take().unwrap()}
		})
		.collect();
	let deserialize_server_args_callback = quote! {
		#further_processing
		#inner_ident(#consumed_arg_names, #extern_crate_pre::vision_utils::types::Callback::new(cb));
	};

	// TODO: Ensure that this generates deserializers for arg when it is not a
	// copy type in client-side code
	let der = gen_der(
		args_iter,
		Some(&mut args),
		&alloc_module,
		&extern_crate_pre,
		Some(deserialize_server_args_callback),
	);

	let mut gen = quote! {
		#[cfg(feature = "module")]
		#extern_attrs
		pub extern "C" fn #msg_ident(#args, msg_id: u32) {
			use #extern_crate_pre::vision_utils::actor::send_message;

			#der
		}

		#[cfg(feature = "module")]
		#input
	};

	let msg_name_ident = Ident::new(msg_name, Span::call_site());

	// User arguments are prefixed by a to: Address arg
	let proper_args = {
		let mut buff = original_args.clone();
		buff.insert(
			0,
			PatType {
				attrs: Vec::new(),
				pat: parse_quote! {to},
				colon_token: Colon::default(),
				ty: parse_quote! {#extern_crate_pre::vision_utils::types::Address},
			},
		);
		buff
	};

	// Include handlers for the response value if there is one
	if let Some(ret_type) = ret_type {
		gen = quote! {
			#gen

			pub static #msg_pipeline_name: RwLock<Vec<Option<Callback<#ser_type>>>> = RwLock::new(Vec::new());

			#[cfg(not(feature = "module"))]
			#[no_mangle]
			pub extern "C" fn #msg_ret_handler_name(from: #extern_crate_pre::vision_utils::types::Address, arg: #ret_type, msg_id: u32) {
				#ret_der
			}

			pub fn #msg_name_ident(#proper_args, callback: Callback<#ser_type>) {
				use #extern_crate_pre::vision_utils::actor::send_message;

				let msg_id: u32 = {
					let mut lock = #msg_pipeline_name.write().unwrap();
					lock.push(Some(callback));
					let id = lock.len() as u32 - 1;

					id
				};

				#client_arg_ser
			}
		}
	} else {
		gen = quote! {
			#gen

			pub fn #msg_name_ident(#proper_args) {
				use #extern_crate_pre::vision_utils::actor::send_message;

				let msg_id: u32 = 0;

				#client_arg_ser
			}
		}
	}

	TokenStream::from(gen)
}
