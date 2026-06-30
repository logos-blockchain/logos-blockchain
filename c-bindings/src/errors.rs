use std::ffi::{CStr, CString, c_char};

#[derive(Default, PartialEq, Eq, Debug)]
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
    StopError = 0xA,
    ConfigurationError = 0xB,
    ValidationError = 0xC,
}

#[derive(Default)]
#[repr(C)]
pub struct OperationStatus {
    pub code: OperationStatusCode,

    /// A NUL-terminated description of the error.
    ///
    /// The caller must free this with
    /// [`free_cstring`](crate::api::memory::free_cstring).
    pub message: *mut c_char,
}

impl OperationStatus {
    pub const OK: Self = Self {
        code: OperationStatusCode::Ok,
        message: std::ptr::null_mut(),
    };

    pub(crate) fn error(code: OperationStatusCode, message: impl Into<String>) -> Self {
        let message = CString::new(message.into())
            .expect("Message contained an interior NUL byte.")
            .into_raw();
        Self { code, message }
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
