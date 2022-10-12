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

	let msg_name = input.sig.ident;

	input.sig.ident = Ident::new(
		&format!("inner_{}", msg_name.to_string()),
		Span::call_site(),
	);

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

	let expanded = quote! {
		pub fn #msg_name(#args) {
			use $crate::serde::Serialize;
			use $crate::serde_json::to_vec;
			use $crate::wasmer::{WasmPtr, Array, FromToNativeWasmType};
			use $crate::vision_utils::actor::{send_message, address};

			let init_size: u32 = 0;

			let res_buf = send_message(1, "allocate",
									   WasmPtr::from_native((&init_size as *const u32) as i32));
		// 	let write_t_bytes = |v| {
		// 		let v_bytes = to_vec(v).unwrap();

		// 		send_message(res_buf, "grow", v_bytes.len());

		// 		for (i, b) in v_bytes.into_iter().enumerate() {
		// 			send_message(res_buf, "write", &[i, b]);
		// 		}
		// 	};

		// 	match #msg_name(#arg_names) {
		// 		Ok(v) => {
		// 			write_t_bytes(v);
		// 			//send_message(from, &format!("{}_ok", msg_name), &res_buf);
		// 		},
		// 		Err(e) => {
		// 			write_t_bytes(e);
		// 			//send_message(from, &format!("{}_err", msg_name), &res_buf);
		// 		},
		// 	};
		}

		#input
	};

	TokenStream::from(expanded)
}
