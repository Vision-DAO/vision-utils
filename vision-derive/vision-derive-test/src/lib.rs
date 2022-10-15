use vision_derive::with_result_message;
use vision_utils::types::Address;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct MyStruct {
	val: u8,
}

#[with_result_message]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn handle_structmsg(from: Address, arg: u8) -> Result<MyStruct, String> {
	Ok(MyStruct { val: arg })
}
