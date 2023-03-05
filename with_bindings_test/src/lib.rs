use vision_derive::with_bindings;
use vision_utils::types::{Address, Callback};

#[no_mangle]
#[with_bindings]
pub extern "C" fn handle_test(from: Address, v: String, callback: Callback<String>) {
	callback.call(String::from("hello there"));
}
