/// Simple wrapper around a value or an error.
///
/// Value is not guaranteed. You should check the error field before accessing
/// the value.
#[repr(C)]
pub struct ValueResult<Type, Error> {
    pub value: Type,
    pub error: Error,
}

impl<Type, Error> ValueResult<Type, Error>
where
    Error: Default,
{
    pub fn from_value(value: Type) -> Self {
        Self {
            value,
            error: Error::default(),
        }
    }
}

impl<Type, Error> ValueResult<Type, Error>
where
    Type: Default,
{
    pub fn from_error(error: Error) -> Self {
        Self {
            value: Type::default(),
            error,
        }
    }
}

impl<Type, Error> From<Result<Type, Error>> for ValueResult<Type, Error>
where
    Type: Default,
    Error: Default,
{
    fn from(result: Result<Type, Error>) -> Self {
        match result {
            Ok(value) => Self::from_value(value),
            Err(error) => Self::from_error(error),
        }
    }
}

/// Simple wrapper around a pointer to a value or an error.
///
/// Pointer is not guaranteed. You should check the error field before
/// dereferencing the pointer.
#[repr(C)]
pub struct PointerResult<Type, Error> {
    pub value: *mut Type,
    pub error: Error,
}

impl<Type, Error> PointerResult<Type, Error>
where
    Error: Default,
{
    pub fn from_pointer(pointer: *mut Type) -> Self {
        Self {
            value: pointer,
            error: Error::default(),
        }
    }

    pub fn from_value(value: Type) -> Self {
        Self::from_pointer(Box::into_raw(Box::new(value)))
    }

    pub const fn from_error(error: Error) -> Self {
        Self {
            value: std::ptr::null_mut(),
            error,
        }
    }
}
