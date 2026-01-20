use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Serialize)]
struct WireOpSer<'a, Inner>
where
    &'a Inner: Serialize,
{
    pub opcode: u8,
    pub payload: &'a Inner,
}

#[derive(Deserialize)]
struct WireOpDe<Inner> {
    // #[expect(dead_code, reason = "Op codes are just used for deserialization")]
    pub opcode: u8,
    pub payload: Inner,
}

pub fn serialize_op_variant<const CODE: u8, Op: Serialize, S: Serializer>(
    op: &Op,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    WireOpSer {
        opcode: CODE,
        payload: op,
    }
    .serialize(serializer)
}

pub fn deserialize_op_variant<'de, const CODE: u8, Op: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Op, D::Error> {
    let op = WireOpDe::<Op>::deserialize(deserializer)?;
    if op.opcode != CODE {
        return Err(serde::de::Error::custom(format!(
            "Invalid opcode {} for type {}",
            op.opcode,
            std::any::type_name::<Op>()
        )));
    }
    Ok(op.payload)
}
