/// A type alias for a C callback function.
pub type CCallback<T> = unsafe extern "C" fn(data: T);

/// A type alias for a boxed Rust callback function.
pub type BoxedCallback<T> = Box<dyn FnMut(T) + Send + Sync>;

/// Converts a C callback function into a boxed Rust callback that can be called
/// from Rust code.
///
/// # Safety
///
/// The caller must ensure that the C callback function is thread-safe and can
/// be safely called from Rust code.
pub fn into_boxed_callback<T: 'static>(callback: CCallback<T>) -> BoxedCallback<T> {
    Box::new(move |block: T| unsafe { callback(block) })
}
