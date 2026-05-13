use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Zero-sized `u8` whose only valid value on the wire is `CODE`.
///
/// Serializes as `CODE`; deserialization errors when the input is any other
/// value.
pub struct ConstU8<const CODE: u8>;

impl<const CODE: u8> Serialize for ConstU8<CODE> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(CODE)
    }
}

impl<'de, const CODE: u8> Deserialize<'de> for ConstU8<CODE> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        if value != CODE {
            return Err(serde::de::Error::custom(format!(
                "Invalid opcode {value}, expected {CODE}"
            )));
        }
        Ok(Self)
    }
}

/// Shared `{ opcode, payload }` wire shape used in both directions.
#[derive(Serialize, Deserialize)]
pub struct OpWire<const CODE: u8, Inner> {
    opcode: ConstU8<CODE>,
    payload: Inner,
}

impl<const CODE: u8, Inner> OpWire<CODE, Inner> {
    pub const fn new(payload: Inner) -> Self {
        Self {
            opcode: ConstU8,
            payload,
        }
    }

    pub fn into_op(self) -> Inner {
        self.payload
    }
}
