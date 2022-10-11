use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{parse, Ident, ItemFn, Span};

/// Generates ok_msgname and err_msgname emitters for the function, providing
/// better Result ergonomics.
#[proc_macro_attribute]
pub fn msg_handler_result(_args: TokenStream, input: TokenStream) -> TokenStream {
	let mut input: ItemFn = parse(input).unwrap();

	let msg_name = input
		.sig
		.ident
		.split('_')
		.nth(1)
		.expect("No handle_ in handler name.");
	input.sig.ident = Ident::new(&format!("inner_{}", msg_name), Span::call_site());
	let wrapper_name = format!("handle_{}", msg_name);

	let args = input.inputs;
	let arg_names = input.inputs.

	let expanded = quote! {
		pub fn #wrapper_name(#args) {
			match #input(
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
