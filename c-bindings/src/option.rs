/// Simple wrapper around an optional value.
///
/// Value is not guaranteed.
/// You should check `is_some` before accessing the value.
#[repr(C)]
pub struct FfiOption<Value> {
    pub is_some: bool,
    pub value: Value,
}

impl<Value> From<Option<Value>> for FfiOption<Value>
where
    Value: Default,
{
    fn from(option: Option<Value>) -> Self {
        option.map_or_else(
            || Self {
                is_some: false,
                value: Value::default(),
            },
            |value| Self {
                is_some: true,
                value,
            },
        )
    }
}
