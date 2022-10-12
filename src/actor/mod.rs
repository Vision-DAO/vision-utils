use super::types::Address;
use wasmer::{Array, WasmPtr};

extern "C" {
	#[link_name = "send_message"]
	fn send_message_raw(addr: Address, msg_name_buf: &str, msg_buf: WasmPtr<u8, Array>);

	#[link_name = "address"]
	fn address_raw() -> Address;
}

pub fn send_message(addr: Address, msg_name_buf: &str, msg_buf: WasmPtr<u8, Array>) {
	unsafe { send_message_raw(addr, msg_name_buf, msg_buf) }
}

pub fn address() -> Address {
	unsafe { address_raw() }
}
