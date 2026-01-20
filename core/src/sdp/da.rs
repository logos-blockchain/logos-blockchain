use nom::{
    IResult, Parser as _,
    bytes::complete::take,
    number::complete::{le_u32, u8 as nom_u8},
};
use serde::{Deserialize, Serialize};

use crate::sdp::{ACTIVE_METADATA_DA_TYPE, SessionNumber, parse_session_number};

const DA_ACTIVE_METADATA_VERSION_BYTE: u8 = 0x01;
type DaMetadataLengthPrefix = u32;
const DA_MIN_METADATA_SIZE: usize = 2 // type + version byte
+ size_of::<SessionNumber>() + size_of::<DaMetadataLengthPrefix>() * 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ActivityProof {
    pub current_session: SessionNumber,
    pub previous_session_opinions: Vec<u8>,
    pub current_session_opinions: Vec<u8>,
}

impl ActivityProof {
    #[must_use]
    pub fn to_metadata_bytes(&self) -> Vec<u8> {
        let total_size = 2 // type + version byte 
            + size_of::<SessionNumber>()
            + size_of::<DaMetadataLengthPrefix>() // previous_session_opinions_length
            + self.previous_session_opinions.len()
            + size_of::<DaMetadataLengthPrefix>() // current_session_opinions_length
            + self.current_session_opinions.len();

        let mut bytes = Vec::with_capacity(total_size);
        bytes.push(ACTIVE_METADATA_DA_TYPE);
        bytes.push(DA_ACTIVE_METADATA_VERSION_BYTE);
        bytes.extend(&self.current_session.to_le_bytes());

        // Encode previous opinions with length prefix
        bytes.extend(
            &(self.previous_session_opinions.len() as DaMetadataLengthPrefix).to_le_bytes(),
        );
        bytes.extend(&self.previous_session_opinions);

        // Encode current opinions with length prefix
        bytes
            .extend(&(self.current_session_opinions.len() as DaMetadataLengthPrefix).to_le_bytes());
        bytes.extend(&self.current_session_opinions);

        bytes
    }

    /// Parse metadata bytes using nom combinators
    pub fn from_metadata_bytes(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        if bytes.is_empty() {
            return Err("empty metadata bytes".to_owned().into());
        }

        if bytes.len() < DA_MIN_METADATA_SIZE {
            return Err(format!(
                "Metadata too short: got {} bytes, expected at least {}",
                bytes.len(),
                DA_MIN_METADATA_SIZE
            )
            .into());
        }

        let (_, proof) =
            parse_activity_proof(bytes).map_err(|e| format!("Failed to parse metadata: {e}"))?;

        Ok(proof)
    }
}

fn parse_activity_proof(input: &[u8]) -> IResult<&[u8], ActivityProof> {
    let (input, metadata_type) = nom_u8(input)?;
    if metadata_type != ACTIVE_METADATA_DA_TYPE {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }

    let (input, version) = nom_u8(input)?;
    if version != DA_ACTIVE_METADATA_VERSION_BYTE {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }

    let (input, current_session) = parse_session_number(input)?;
    let (input, previous_session_opinions) = parse_length_prefixed_bytes(input)?;
    let (input, current_session_opinions) = parse_length_prefixed_bytes(input)?;

    if !input.is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Eof,
        )));
    }

    Ok((
        input,
        ActivityProof {
            current_session,
            previous_session_opinions,
            current_session_opinions,
        },
    ))
}

/// Parse length-prefixed byte vector: u32 length + data
fn parse_length_prefixed_bytes(input: &[u8]) -> IResult<&[u8], Vec<u8>> {
    let (input, len) = le_u32(input)?;
    let (input, data) = take(len as usize).parse(input)?;
    Ok((input, data.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::ActivityMetadata;

    #[test]
    fn activity_proof_roundtrip_empty_opinions() {
        let proof = ActivityProof {
            current_session: 42,
            previous_session_opinions: vec![],
            current_session_opinions: vec![],
        };

        let bytes = proof.to_metadata_bytes();
        let decoded = ActivityProof::from_metadata_bytes(&bytes).unwrap();
        assert_eq!(proof, decoded);
    }

    #[test]
    fn activity_proof_roundtrip_with_data() {
        let proof = ActivityProof {
            current_session: 123,
            previous_session_opinions: vec![0xFF, 0xAA, 0x55],
            current_session_opinions: vec![0x01, 0x02, 0x03, 0x04],
        };

        let bytes = proof.to_metadata_bytes();
        let decoded = ActivityProof::from_metadata_bytes(&bytes).unwrap();

        assert_eq!(proof, decoded);
    }

    #[test]
    fn activity_proof_byte_format() {
        let proof = ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![0xAA],
            current_session_opinions: vec![0xBB, 0xCC],
        };

        let bytes = proof.to_metadata_bytes();

        // Verify format: type(1) + version(1) + session(8) + prev_len(4) + prev_data +
        // curr_len(4) + curr_data
        assert_eq!(bytes[0], ACTIVE_METADATA_DA_TYPE); // version
        assert_eq!(bytes[1], DA_ACTIVE_METADATA_VERSION_BYTE); // version

        // Session number (little-endian u64)
        let session_bytes: [u8; 8] = bytes[2..10].try_into().unwrap();
        assert_eq!(u64::from_le_bytes(session_bytes), 1);

        // Previous opinions length
        let prev_len_bytes: [u8; 4] = bytes[10..14].try_into().unwrap();
        assert_eq!(u32::from_le_bytes(prev_len_bytes), 1);

        // Previous opinions data
        assert_eq!(bytes[14], 0xAA);

        // Current opinions length
        let curr_len_bytes: [u8; 4] = bytes[15..19].try_into().unwrap();
        assert_eq!(u32::from_le_bytes(curr_len_bytes), 2);

        // Current opinions data
        assert_eq!(bytes[19], 0xBB);
        assert_eq!(bytes[20], 0xCC);

        // Total length check
        assert_eq!(bytes.len(), 21); // 1 + 1 + 8 + 4 + 1 + 4 + 2
    }

    #[test]
    fn activity_proof_empty_metadata() {
        let proof = ActivityProof {
            current_session: 999,
            previous_session_opinions: vec![],
            current_session_opinions: vec![],
        };

        let bytes = proof.to_metadata_bytes();

        // Check that empty vectors are encoded with zero length
        let prev_len_bytes: [u8; 4] = bytes[9..13].try_into().unwrap();
        assert_eq!(u32::from_le_bytes(prev_len_bytes), 0);

        let curr_len_bytes: [u8; 4] = bytes[13..17].try_into().unwrap();
        assert_eq!(u32::from_le_bytes(curr_len_bytes), 0);

        // Total: version(1) + session(8) + prev_len(4) + curr_len(4) = 17 bytes
        assert_eq!(bytes.len(), DA_MIN_METADATA_SIZE);

        let decoded = ActivityProof::from_metadata_bytes(&bytes).unwrap();
        assert_eq!(proof, decoded);
    }

    #[test]
    fn activity_proof_large_opinions() {
        let proof = ActivityProof {
            current_session: u64::MAX,
            previous_session_opinions: vec![0xFF; 1000],
            current_session_opinions: vec![0xAA; 2000],
        };

        let bytes = proof.to_metadata_bytes();
        let decoded = ActivityProof::from_metadata_bytes(&bytes).unwrap();

        assert_eq!(proof, decoded);
        assert_eq!(decoded.previous_session_opinions.len(), 1000);
        assert_eq!(decoded.current_session_opinions.len(), 2000);
    }

    #[test]
    fn activity_proof_invalid_version() {
        let proof = ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![0xAA],
            current_session_opinions: vec![0xBB, 0xCC],
        };
        let mut bytes = proof.to_metadata_bytes();
        bytes[1] = 0x99; // Invalid version

        let result = ActivityProof::from_metadata_bytes(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn activity_proof_too_short() {
        let bytes = vec![DA_ACTIVE_METADATA_VERSION_BYTE, 0x01, 0x02]; // Only 3 bytes

        let result = ActivityProof::from_metadata_bytes(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too short"));
    }

    #[test]
    fn activity_proof_truncated_data() {
        let mut bytes = vec![DA_ACTIVE_METADATA_VERSION_BYTE];
        bytes.extend(&1u64.to_le_bytes()); // session
        bytes.extend(&5u32.to_le_bytes()); // prev len = 5
        bytes.extend(&[0xAA, 0xBB]); // Only 2 bytes instead of 5

        let result = ActivityProof::from_metadata_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn activity_proof_extra_bytes() {
        let proof = ActivityProof {
            current_session: 10,
            previous_session_opinions: vec![0x11],
            current_session_opinions: vec![0x22],
        };

        let mut bytes = proof.to_metadata_bytes();
        bytes.push(0xFF); // Extra byte

        let result = ActivityProof::from_metadata_bytes(&bytes);
        assert!(result.is_err()); // Should fail due to extra bytes
    }

    #[test]
    fn activity_metadata_roundtrip() {
        let proof = ActivityProof {
            current_session: 456,
            previous_session_opinions: vec![0x12, 0x34],
            current_session_opinions: vec![0x56, 0x78, 0x9A],
        };
        let metadata = ActivityMetadata::DataAvailability(proof.clone());

        let bytes = metadata.to_metadata_bytes();
        let decoded = ActivityMetadata::from_metadata_bytes(&bytes).unwrap();

        assert_eq!(metadata, decoded);

        let ActivityMetadata::DataAvailability(decoded_proof) = decoded else {
            panic!("Unexpected ActivityMetadata variant");
        };
        assert_eq!(proof, decoded_proof);
    }
}
