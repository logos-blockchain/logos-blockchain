//! A separate bindings consumer with its own error codes and no node
//! dependency.

extern crate logos_blockchain_c_common as lb_c_common;

use std::{
    ffi::{CStr, c_char},
    sync::atomic::{AtomicUsize, Ordering},
};

use lb_c_common::{
    FfiError, FfiResult, FfiReturn, memory, return_error_if_null_pointer, unwrap_or_return_error,
};
use lb_c_macros::panic_to_error;

static RELEASED_ERRORS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
#[repr(C)]
struct ConsumerStatus {
    code: u32,
    message: *mut c_char,
}

impl ConsumerStatus {
    fn error(code: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: memory::error_message(message),
        }
    }
}

// SAFETY: Both constructors return a new message owned by this consumer.
unsafe impl FfiError for ConsumerStatus {
    fn is_ok(&self) -> bool {
        self.code == 0
    }

    fn null_pointer(name: &str) -> Self {
        Self::error(1, name)
    }

    fn from_panic(message: impl Into<String>) -> Self {
        Self::error(2, message)
    }

    unsafe fn release(self) {
        RELEASED_ERRORS.fetch_add(1, Ordering::Relaxed);

        // SAFETY: This status owns the string allocated by `error_message` above.
        unsafe { memory::free_cstring(self.message) };
    }
}

impl FfiReturn<Self> for ConsumerStatus {
    unsafe fn from_error(error: Self) -> Self {
        error
    }
}

#[panic_to_error(ConsumerStatus)]
extern "C" fn open(input: *const c_char) -> FfiResult<*mut u32, ConsumerStatus> {
    return_error_if_null_pointer!(input, ConsumerStatus);

    FfiResult::from_value(42)
}

#[panic_to_error(ConsumerStatus)]
extern "C" fn fail() -> ConsumerStatus {
    // SAFETY: This error owns the message just allocated by the consumer.
    unsafe { unwrap_or_return_error!(Err::<(), _>(ConsumerStatus::error(3, "bad\0input"))) };
    unreachable!()
}

#[panic_to_error(ConsumerStatus)]
extern "C" fn panic_with_result() -> FfiResult<*mut u32, ConsumerStatus> {
    panic!("failed");
}

#[panic_to_error(ConsumerStatus)]
extern "C" fn panic_without_result() {
    panic!("failed during cleanup");
}

#[panic_to_error(ConsumerStatus)]
extern "C" fn error_without_result() {
    let error = ConsumerStatus::error(3, "failed during cleanup");

    // SAFETY: This error owns the message just allocated by the consumer.
    unsafe { unwrap_or_return_error!(Err::<(), _>(error)) };
}

fn consume(status: ConsumerStatus) -> (u32, String) {
    let code = status.code;
    let message = if status.message.is_null() {
        String::new()
    } else {
        // SAFETY: The status was returned by this consumer and has not been freed.
        unsafe { CStr::from_ptr(status.message) }
            .to_str()
            .unwrap()
            .to_owned()
    };

    // SAFETY: Consuming the status releases its owned message, or null on success.
    unsafe { status.release() };

    (code, message)
}

#[test]
fn another_library_can_return_handles_errors_and_panics() {
    let result = open(c"database".as_ptr());
    assert!(result.is_ok());

    // SAFETY: `open` returned an owned u32 allocation, still live here.
    assert_eq!(unsafe { *result.value }, 42);
    // SAFETY: Return the handle to the same consumer that created it.
    unsafe { memory::free(result.value) };
    assert_eq!(consume(result.error), (0, String::new()));

    let result = open(std::ptr::null());
    assert!(result.is_err());
    assert!(result.value.is_null());
    assert_eq!(consume(result.error), (1, "input".into()));
    assert_eq!(consume(fail()), (3, "bad\\0input".into()));

    let result = panic_with_result();
    assert!(result.value.is_null());
    assert_eq!(consume(result.error), (2, "Internal panic: failed".into()));

    let released = RELEASED_ERRORS.load(Ordering::Relaxed);

    panic_without_result();
    error_without_result();
    assert_eq!(RELEASED_ERRORS.load(Ordering::Relaxed), released + 2);
}
