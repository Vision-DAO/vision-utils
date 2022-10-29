use vision_derive::with_bindings;
use vision_utils::types::Address;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Ball {
	pub ping: u32,
	pub pong: u32,
}

#[with_bindings(self)]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn handle_ping(from: Address, ball: Ball) -> Ball {
	Ball {
		ping: ball.ping,
		pong: ball.pong + 1,
	}
}
