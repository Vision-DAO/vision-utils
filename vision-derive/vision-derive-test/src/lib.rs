use vision_derive::with_result_message;
use vision_utils::types::Address;

#[with_result_message]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn handle_mymessage(from: Address, arg: u8) -> Result<u8, String> {
	Ok(arg)
}
