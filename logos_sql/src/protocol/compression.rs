//! Compresses encoded inscription bodies before publication.

use std::{borrow::Cow, io::Write as _};

use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};

use super::MAX_BODY_BYTES;
use crate::Error;

// Zlib bodies include their original length so decoding allocates only what is
// needed. Both encodings are subject to the same uncompressed size limit.
const PLAIN: u8 = 0;
const ZLIB: u8 = 1;
const LENGTH_BYTES: usize = size_of::<u32>();

pub(super) fn encode(body: Vec<u8>) -> Result<(u8, Vec<u8>), Error> {
    let length = u32::try_from(body.len()).map_err(|_| Error::InscriptionTooLarge)?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&body)?;

    let compressed = encoder.finish()?;

    if compressed.len() + LENGTH_BYTES >= body.len() {
        return Ok((PLAIN, body));
    }

    let mut encoded = Vec::with_capacity(LENGTH_BYTES + compressed.len());
    encoded.extend_from_slice(&length.to_le_bytes());
    encoded.extend_from_slice(&compressed);

    Ok((ZLIB, encoded))
}

pub(super) fn decode(encoding: u8, body: &[u8]) -> Result<Cow<'_, [u8]>, Error> {
    match encoding {
        PLAIN => Ok(Cow::Borrowed(body)),
        ZLIB => decompress(body).map(Cow::Owned),
        _ => Err(Error::InvalidPayload("body encoding is not supported")),
    }
}

fn decompress(body: &[u8]) -> Result<Vec<u8>, Error> {
    let (length, compressed) = body
        .split_at_checked(LENGTH_BYTES)
        .ok_or(Error::InvalidPayload("uncompressed length is missing"))?;
    let length = u32::from_le_bytes(
        length
            .try_into()
            .map_err(|_| Error::InvalidPayload("uncompressed length is invalid"))?,
    ) as usize;

    if length > MAX_BODY_BYTES {
        return Err(Error::InvalidPayload(
            "uncompressed body exceeds the protocol limit",
        ));
    }

    let mut decoded = vec![0; length];
    let mut decoder = Decompress::new(true);
    let status = decoder
        .decompress(compressed, &mut decoded, FlushDecompress::Finish)
        .map_err(|_| Error::InvalidPayload("compressed body cannot be decoded"))?;

    if status != Status::StreamEnd
        || decoder.total_in() != compressed.len() as u64
        || decoder.total_out() != length as u64
    {
        return Err(Error::InvalidPayload(
            "compressed body has an invalid length",
        ));
    }

    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::{LENGTH_BYTES, MAX_BODY_BYTES, PLAIN, ZLIB, decode, encode};
    use crate::Error;

    #[test]
    fn keeps_plain_body_when_compression_would_add_overhead() {
        let original = vec![1, 2, 3];
        let (encoding, body) = encode(original.clone()).expect("body should encode");

        assert_eq!(encoding, PLAIN);
        assert_eq!(body, original);
    }

    #[test]
    fn compressed_body_must_be_complete_and_have_no_trailing_bytes() {
        let (_, body) = encode(vec![0; 4096]).expect("body should encode");

        for end in [LENGTH_BYTES - 1, body.len() - 1] {
            assert!(
                matches!(decode(ZLIB, &body[..end]), Err(Error::InvalidPayload(_))),
                "accepted truncation at {end}"
            );
        }

        let mut trailing = body;
        trailing.push(0);
        assert!(matches!(
            decode(ZLIB, &trailing),
            Err(Error::InvalidPayload(_))
        ));
    }

    #[test]
    fn decompression_enforces_the_declared_length_and_size_limit() {
        let (_, mut body) = encode(vec![0; 4096]).expect("body should encode");

        for length in [0, 4095, 4097, MAX_BODY_BYTES + 1] {
            let length = u32::try_from(length).expect("test length fits u32");
            body[..LENGTH_BYTES].copy_from_slice(&length.to_le_bytes());

            assert!(decode(ZLIB, &body).is_err());
        }
    }

    #[test]
    fn unknown_encoding_is_rejected() {
        assert!(decode(2, b"body").is_err());
    }
}
