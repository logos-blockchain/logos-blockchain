use nom::{
    Err, IResult,
    error::{Error, ErrorKind},
};
use time::OffsetDateTime;

use crate::mantle::nom::{NomDecode, NomEncode};

impl NomEncode for OffsetDateTime {
    fn encode(&self) -> Vec<u8> {
        u64::try_from(self.unix_timestamp())
            .expect("timestamp fits in u64")
            .encode()
    }
}

impl NomDecode for OffsetDateTime {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining, timestamp) = u64::decode(bytes)?;
        let i64_timestamp = i64::try_from(timestamp)
            .map_err(|_| Err::Failure(Error::new(bytes, ErrorKind::MapRes)))?;
        Ok((
            remaining,
            Self::from_unix_timestamp(i64_timestamp)
                .map_err(|_| Err::Failure(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}
