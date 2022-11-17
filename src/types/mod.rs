use std::{ops::Deref, sync::Arc};

/// The index of an actor in the VVM.
pub type Address = u32;

/// Addresses of important root services.
pub const PERM_ADDR: Address = 1;
pub const ALLOCATOR_ADDR: Address = 2;
pub const LOGGER_ADDR: Address = 3;

/// Supplied to message bindings created by with_bindings to capture the
/// value generated by the handling actor.
pub struct Callback<T>(Arc<Box<dyn Fn(T) + Send + Sync>>);

impl<T> From<&'static (dyn Fn(T) + Send + Sync)> for Callback<T> {
	fn from(cb: &'static (dyn Fn(T) + Send + Sync)) -> Callback<T> {
		Callback(Arc::new(Box::new(cb)))
	}
}

impl<T> AsRef<dyn Fn(T) + Send + Sync> for Callback<T> {
	fn as_ref(&self) -> &(dyn Fn(T) + Send + Sync + 'static) {
		(*self.0).as_ref()
	}
}

impl<T> Deref for Callback<T> {
	type Target = dyn Fn(T) + Send + Sync;

	fn deref(&self) -> &Self::Target {
		self.as_ref()
	}
}
