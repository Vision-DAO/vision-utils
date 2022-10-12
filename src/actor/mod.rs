use super::types::Address;
use wasmer::{Array, WasmPtr};

extern "C" {
	#[link_name = "send_message"]
	fn send_message_raw(
		addr: Address,
		msg_name_buf: WasmPtr<u8, Array>,
		msg_buf: WasmPtr<u8, Array>,
	);

	#[link_name = "address"]
	fn address_raw() -> Address;

	#[link_name = "spawn_actor"]
	fn spawn_actor_raw(addr: Address) -> Address;
}

pub fn send_message(addr: Address, msg_name_buf: WasmPtr<u8, Array>, msg_buf: WasmPtr<u8, Array>) {
	unsafe { send_message_raw(addr, msg_name_buf, msg_buf) }
}

pub fn address() -> Address {
	unsafe { address_raw() }
}

pub fn spawn_actor(addr: Address) -> Address {
	unsafe { spawn_actor_raw(addr) }
}
