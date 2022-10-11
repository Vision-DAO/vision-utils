use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
	parse, punctuated::Punctuated, token::Comma, Expr, ExprPath, FnArg, Ident, ItemFn, Pat,
	PatIdent, Path, PathArguments, PathSegment,
};

/// Generates handle_msgname_ok and handle_msgname_err emitters for the function, providing
/// better Result ergonomics.
#[proc_macro_attribute]
pub fn with_result_message(_args: TokenStream, input: TokenStream) -> TokenStream {
	let mut input: ItemFn = parse(input).unwrap();

	let msg_name = input.sig.ident.to_string();

	input.sig.ident = Ident::new(&format!("inner_{}", msg_name), Span::call_site());

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
			extern "C" {
				fn send_message(addr: Address, msg_name_buf: &str, msg_buf: WasmPtr<u8, Array>);
			}

			match #input(#arg_names) {
				Ok(v) => send_message(from, &format!("{}_ok", msg_name, ));
				Err(v) => send_message(from, &format!("{}_err", msg_name));
			}
		}

		#input
	};

	TokenStream::from(expanded)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn it_works() {
		let result = add(2, 2);
		assert_eq!(result, 4);
	}
}
