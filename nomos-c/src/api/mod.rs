pub mod cryptarchia;
pub mod lifecycle;
pub mod wallet;

#[repr(C)]
pub struct ReturnResult<Type, Error> {
    pub value: *mut Type,
    pub error: Error,
}

impl<Type, Error: Default> ReturnResult<Type, Error> {
    pub fn from_value(value: Type) -> Self {
        Self::from_pointer(Box::into_raw(Box::new(value)))
    }

    pub fn from_pointer(value: *mut Type) -> Self {
        Self {
            value,
            error: Error::default(),
        }
    }

    pub const fn from_error(error: Error) -> Self {
        Self {
            value: core::ptr::null_mut(),
            error,
        }
    }
}
