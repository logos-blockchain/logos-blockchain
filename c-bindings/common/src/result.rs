use crate::FfiError;

/// Simple wrapper around a value or an error.
///
/// Value is not guaranteed.
/// You should check the error field before accessing the value.
#[repr(C)]
pub struct FfiResult<Value, Error> {
    pub value: Value,
    pub error: Error,
}

impl<Value, Error> FfiResult<Value, Error>
where
    Value: Default,
    Error: Default,
{
    pub fn ok(value: Value) -> Self {
        Self {
            value,
            error: Error::default(),
        }
    }

    pub fn err(error: Error) -> Self {
        Self {
            value: Value::default(),
            error,
        }
    }
}

impl<Value, Error> FfiResult<Value, Error>
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

impl<Value, Error> From<Result<Value, Error>> for FfiResult<Value, Error>
where
    Value: Default,
    Error: Default,
{
    fn from(result: Result<Value, Error>) -> Self {
        match result {
            Ok(value) => Self::ok(value),
            Err(error) => Self::err(error),
        }
    }
}

impl<Value, Error> FfiResult<*mut Value, Error>
where
    Error: Default,
{
    pub fn from_value(value: Value) -> Self {
        Self {
            value: Box::into_raw(Box::new(value)),
            error: Error::default(),
        }
    }
}
