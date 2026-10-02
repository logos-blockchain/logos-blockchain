use std::ffi::c_char;

/// A type alias for a C callback function.
pub type CCallback<T> = unsafe extern "C" fn(data: T);

/// The callback the `subscribe_to_*` functions take: called with a pointer to a
/// NUL-terminated JSON event, or with null once the stream has ended.
///
/// A Rust function pointer can never be null, while C is free to pass one, so
/// this is an `Option`: it has the same representation as the bare pointer,
/// with `None` standing for null. It is spelled out rather than built from
/// [`CCallback`] because `cbindgen` only recognises the pattern in this form.
pub type EventCallback = Option<unsafe extern "C" fn(data: *const c_char)>;

/// A type alias for a boxed Rust callback function.
pub type BoxedCallback<T> = Box<dyn FnMut(T) + Send + Sync>;

/// Converts a C callback function into a boxed Rust callback that can be called
/// from Rust code.
///
/// # Safety
///
/// The caller must ensure that the C callback function is thread-safe, can be
/// safely called from Rust code, and stays valid for as long as the returned
/// callback may be called.
pub unsafe fn into_boxed_callback<T: 'static>(callback: CCallback<T>) -> BoxedCallback<T> {
    Box::new(move |block: T| unsafe { callback(block) })
}
