pub use decoding::*;
pub use encoding::*;

mod decoding {
    use nom::{
        IResult, Parser as _,
        bytes::complete::take,
        combinator::map_res,
        error::{Error, ErrorKind},
        number::complete::le_u64,
    };
    use time::OffsetDateTime;

    pub fn decode_utf8_string(input: &[u8], len: usize) -> IResult<&[u8], String> {
        map_res(take(len), |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .map(ToOwned::to_owned)
                .map_err(|_| Error::new(bytes, ErrorKind::Fail))
        })
        .parse(input)
    }

    pub fn decode_uint64(input: &[u8]) -> IResult<&[u8], u64> {
        // UINT64 = 8BYTE
        le_u64(input)
    }

    pub fn decode_unix_timestamp(input: &[u8]) -> IResult<&[u8], OffsetDateTime> {
        // Timestamp = UINT64
        map_res(decode_uint64, |ts| {
            OffsetDateTime::from_unix_timestamp(
                ts.try_into()
                    .map_err(|_| Error::new(input, ErrorKind::Fail))?,
            )
            .map_err(|_| Error::new(input, ErrorKind::Fail))
        })
        .parse(input)
    }
}

mod encoding {
    use time::OffsetDateTime;

    pub fn encode_uint64(value: u64) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }

    pub fn encode_string(s: &String) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    pub fn encode_unix_timestamp(ts: &OffsetDateTime) -> Vec<u8> {
        encode_uint64(
            ts.unix_timestamp()
                .try_into()
                .expect("timestamp fits in u64"),
        )
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use crate::mantle::{
        codec::primitives::{
            decoding::{decode_uint64, decode_unix_timestamp, decode_utf8_string},
            encoding::{encode_string, encode_uint64, encode_unix_timestamp},
        },
        nom::{NomDecode as _, NomEncode as _},
    };

    #[test]
    fn test_encode_decode_primitives() {
        // Test UINT64
        let data = encode_uint64(42u64);
        let (remaining, value) = decode_uint64(&data).unwrap();
        assert_eq!(value, 42u64);
        assert!(remaining.is_empty());

        // Test UINT32
        let data = 123u32.encode();
        let (remaining, value) = u32::decode(&data).unwrap();
        assert_eq!(value, 123u32);
        assert!(remaining.is_empty());

        // Test Byte
        let data = 0xABu8.encode();
        let (remaining, value) = u8::decode(&data).unwrap();
        assert_eq!(value, 0xAB);
        assert!(remaining.is_empty());

        // Test Hash32
        let data = [0x42u8; 32].encode();
        let (remaining, value) = <[u8; 32]>::decode(&data).unwrap();
        assert_eq!(value, [0x42u8; 32]);
        assert!(remaining.is_empty());

        // Test UTF-8 String
        let str = "hello, world!".to_owned();
        let data = encode_string(&str);
        let (remaining, value) = decode_utf8_string(&data, data.len()).unwrap();
        assert_eq!(value, str);
        assert!(remaining.is_empty());

        // Test Unix Timestamp
        let ts = OffsetDateTime::now_utc();
        let data = encode_unix_timestamp(&ts);
        let (remaining, value) = decode_unix_timestamp(&data).unwrap();
        assert_eq!(value, ts.truncate_to_second());
        assert!(remaining.is_empty());
    }
}
