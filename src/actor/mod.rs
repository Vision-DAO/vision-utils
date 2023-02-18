use super::types::Address;

extern "C" {
	#[link_name = "send_message"]
	fn send_message_raw(addr: Address, msg_name_buf: i32, msg_buf: i32);

	#[link_name = "address"]
	fn address_raw() -> Address;

	#[link_name = "spawn_actor"]
	fn spawn_actor_raw(addr: Address) -> Address;

	#[link_name = "spawn_actor_from"]
	fn spawn_actor_from_raw(addr: Address) -> Address;
}

pub fn send_message(addr: Address, msg_name_buf: i32, msg_buf: i32) {
	unsafe { send_message_raw(addr, msg_name_buf, msg_buf) }
}

pub fn address() -> Address {
	unsafe { address_raw() }
}

pub fn spawn_actor(addr: Address) -> Address {
	unsafe { spawn_actor_raw(addr) }
}

pub fn spawn_actor_from(addr: Address) -> Address {
	unsafe { spawn_actor_from_raw(addr) }
}
