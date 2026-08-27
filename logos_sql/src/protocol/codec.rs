//! Binary encoding for protocol leaves backed by external types.

use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_utils::bounded::UpperBoundedVec;
use rusqlite::types::Value;

use super::{MAX_PAYLOAD_BYTES, SqlParameter, SqlText};

const NULL: u8 = 0;
const INTEGER: u8 = 1;
const REAL: u8 = 2;
const TEXT: u8 = 3;
const BLOB: u8 = 4;

type BoundedBytes = UpperBoundedVec<u8, MAX_PAYLOAD_BYTES>;

// Every variable-length field uses the same fixed-width prefix. Keep the two
// leaf encoders in lockstep with the bounded collection decoder.
const _: () = assert!(MAX_PAYLOAD_BYTES > u16::MAX as usize);
const _: () = assert!(MAX_PAYLOAD_BYTES <= u32::MAX as usize);

impl BinaryEncode for SqlText {
    fn encoded_length(&self) -> usize {
        size_of::<u32>() + self.as_str().len()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        u32::try_from(self.as_str().len())
            .expect("validated SQL length fits in u32")
            .encode_into(out);
        out.extend_from_slice(self.as_str().as_bytes());
    }
}

impl BinaryDecode for SqlText {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, sql) = <BoundedBytes as BinaryDecode>::decode(input, &())?;
        let sql = String::from_utf8(sql.into_inner())
            .map_err(|_| DecodeError::invalid_value::<Self>("statement SQL is not UTF-8"))?;
        let sql = Self::new(sql)
            .map_err(|_| DecodeError::invalid_value::<Self>("statement SQL is invalid"))?;

        Ok((input, sql))
    }
}

impl BinaryEncode for SqlParameter {
    fn encoded_length(&self) -> usize {
        1 + match &self.0 {
            Value::Null => 0,
            Value::Integer(_) | Value::Real(_) => size_of::<u64>(),
            Value::Text(value) => size_of::<u32>() + value.len(),
            Value::Blob(value) => size_of::<u32>() + value.len(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match &self.0 {
            Value::Null => NULL.encode_into(out),
            Value::Integer(value) => {
                INTEGER.encode_into(out);
                u64::from_le_bytes(value.to_le_bytes()).encode_into(out);
            }
            Value::Real(value) => {
                REAL.encode_into(out);
                value.to_bits().encode_into(out);
            }
            Value::Text(value) => {
                TEXT.encode_into(out);
                u32::try_from(value.len())
                    .expect("validated text length fits in u32")
                    .encode_into(out);
                out.extend_from_slice(value.as_bytes());
            }
            Value::Blob(value) => {
                BLOB.encode_into(out);
                u32::try_from(value.len())
                    .expect("validated blob length fits in u32")
                    .encode_into(out);
                out.extend_from_slice(value);
            }
        }
    }
}

impl BinaryDecode for SqlParameter {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, tag) = <u8 as BinaryDecode>::decode(input, &())?;

        let (input, value) = match tag {
            NULL => (input, Value::Null),
            INTEGER => {
                let (input, value) = <u64 as BinaryDecode>::decode(input, &())?;
                let value = i64::from_le_bytes(value.to_le_bytes());

                (input, Value::Integer(value))
            }
            REAL => {
                let (input, bits) = <u64 as BinaryDecode>::decode(input, &())?;

                (input, Value::Real(f64::from_bits(bits)))
            }
            TEXT => {
                let (input, value) = <BoundedBytes as BinaryDecode>::decode(input, &())?;
                let value = String::from_utf8(value.into_inner()).map_err(|_| {
                    DecodeError::invalid_value::<Self>("text parameter is not UTF-8")
                })?;

                (input, Value::Text(value))
            }
            BLOB => {
                let (input, value) = <BoundedBytes as BinaryDecode>::decode(input, &())?;

                (input, Value::Blob(value.into_inner()))
            }
            _ => return Err(DecodeError::unknown_discriminant::<Self>(u64::from(tag))),
        };

        let value = Self::try_from(value)
            .map_err(|_| DecodeError::invalid_value::<Self>("SQL parameter is invalid"))?;

        Ok((input, value))
    }
}
