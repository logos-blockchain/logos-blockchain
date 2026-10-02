use std::{
    any::Any,
    ffi::{CStr, CString, c_char},
};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum OperationStatusCode {
    #[default]
    Ok = 0x0,
    NotFound = 0x1,
    NullPointer = 0x2,
    RelayError = 0x3,
    ChannelSendError = 0x4,
    ChannelReceiveError = 0x5,
    ServiceError = 0x6,
    RuntimeError = 0x7,
    DynError = 0x8,
    InitializationError = 0x9,
    ShutdownError = 0xA,
    ConfigurationError = 0xB,
    ValidationError = 0xC,
}

#[derive(Default)]
#[repr(C)]
pub struct OperationStatus {
    pub code: OperationStatusCode,

    /// A NUL-terminated description of the error. Null on success.
    ///
    /// The caller must free it, either by passing the whole status to
    /// [`free_operation_status`](crate::api::memory::free_operation_status)
    /// or by passing the message to
    /// [`free_cstring`](crate::api::memory::free_cstring).
    pub message: *mut c_char,
}

impl OperationStatus {
    pub const OK: Self = Self {
        code: OperationStatusCode::Ok,
        message: std::ptr::null_mut(),
    };

    pub(crate) fn error(code: OperationStatusCode, message: impl Into<String>) -> Self {
        // A C string cannot hold a NUL byte, but an error message is free to
        // carry one: many of them echo input the caller or a file supplied.
        // Escaping keeps the message readable and this function infallible.
        let message = CString::new(message.into().replace('\0', "\\0"))
            .unwrap_or_default()
            .into_raw();
        Self { code, message }
    }

    /// The status an exported function returns when its body panicked: see
    /// [`panic_to_error`](lb_c_macros::panic_to_error).
    pub(crate) fn from_panic(payload: &(dyn Any + Send)) -> Self {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no message");
        Self::error(
            OperationStatusCode::RuntimeError,
            format!("Internal panic: {message}"),
        )
    }

    #[must_use]
    #[unsafe(no_mangle)]
    pub extern "C" fn is_ok(&self) -> bool {
        self.code == OperationStatusCode::Ok
    }

    #[must_use]
    #[unsafe(no_mangle)]
    pub extern "C" fn is_error(&self) -> bool {
        !self.is_ok()
    }
}

impl std::fmt::Debug for OperationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = if self.message.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(self.message) }.to_string_lossy())
        };
        f.debug_struct("OperationStatus")
            .field("code", &self.code)
            .field("message", &message.as_deref().unwrap_or("<no message>"))
            .finish()
    }
}
