use crate::FfiError;

/// Simple wrapper around a value or an error.
///
/// Value is not guaranteed.
/// You should check the error field before accessing the value.
#[repr(C)]
pub struct FfiResult<Type, Error> {
    pub value: Type,
    pub error: Error,
}

impl<Type, Error> FfiResult<Type, Error>
where
    Type: Default,
    Error: Default,
{
    pub fn ok(value: Type) -> Self {
        Self {
            value,
            error: Error::default(),
        }
    }

    pub fn err(error: Error) -> Self {
        Self {
            value: Type::default(),
            error,
        }
    }
}

impl<Type, Error> FfiResult<Type, Error>
where
    Error: FfiError,
{
    pub fn is_ok(&self) -> bool {
        self.error.is_ok()
    }

    pub fn is_err(&self) -> bool {
        !self.is_ok()
    }
}

impl<Type, Error> From<Result<Type, Error>> for FfiResult<Type, Error>
where
    Type: Default,
    Error: Default,
{
    fn from(result: Result<Type, Error>) -> Self {
        match result {
            Ok(value) => Self::ok(value),
            Err(error) => Self::err(error),
        }
    }
}

impl<Type, Error> FfiResult<*mut Type, Error>
where
    Error: Default,
{
    pub fn from_value(value: Type) -> Self {
        Self {
            value: Box::into_raw(Box::new(value)),
            error: Error::default(),
        }
    }
}
