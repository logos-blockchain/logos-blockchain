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

#[cfg(test)]
mod tests {
    use super::ConstU8;

    #[test]
    fn serialize_writes_the_opcode() {
        assert_eq!(
            serde_json::to_value(ConstU8::<7>).expect("the opcode serializes"),
            serde_json::json!(7)
        );
    }

    #[test]
    fn deserialize_accepts_the_expected_opcode() {
        serde_json::from_value::<ConstU8<7>>(serde_json::json!(7))
            .map(|_| ())
            .expect("7 is the expected opcode");
    }

    #[test]
    fn deserialize_rejects_another_opcode() {
        let error = serde_json::from_value::<ConstU8<7>>(serde_json::json!(8))
            .map(|_| ())
            .expect_err("8 is not the expected opcode");

        assert!(error.to_string().contains("Invalid opcode 8, expected 7"));
    }
}
