use crate::mantle::{nom::NomEncode, ops::sdp::SDPActiveOp};

// #[must_use]
// pub fn encode_sdp_active(op: &SDPActiveOp) -> Vec<u8> {
//     let mut bytes = Vec::new();
//     bytes.extend(encode_hash32(&op.declaration_id.0));
//     bytes.extend(encode_uint64(op.nonce));

//     // Metadata - convert ActivityMetadata to bytes
//     let metadata_bytes = op.metadata.to_metadata_bytes();
//     assert!(
//         metadata_bytes.len() <= MAX_ENCODE_DECODE_METADATA_SIZE as usize,
//         "Fatal error in 'encode_sdp_active' - {} metadata bytes clipped to
// {}",         metadata_bytes.len(),
//         MAX_ENCODE_DECODE_METADATA_SIZE
//     );

//     bytes.extend(encode_uint32(metadata_bytes.len() as u32));
//     bytes.extend(&metadata_bytes);
//     bytes
// }

// pub(crate) fn decode_sdp_active(input: &[u8]) -> IResult<&[u8], SDPActiveOp>
// {     // SDPActive = DeclarationId Nonce Metadata
//     // Metadata = UINT32 *BYTE
//     let (input, declaration_id_bytes) = decode_hash32(input)?;
//     let declaration_id = DeclarationId(declaration_id_bytes);

//     let (input, nonce) = decode_uint64(input)?;

//     let (input, metadata_len) = decode_uint32(input)?;

//     // Validate metadata length to prevent unbounded memory allocation
//     if metadata_len > MAX_ENCODE_DECODE_METADATA_SIZE {
//         return Err(nom::Err::Error(Error::new(input, ErrorKind::TooLarge)));
//     }

//     let (input, metadata_bytes) = take(metadata_len as usize).parse(input)?;

//     let metadata = ActivityMetadata::from_metadata_bytes(metadata_bytes)
//         .map_err(|_| nom::Err::Error(Error::new(input, ErrorKind::Fail)))?;

//     Ok((
//         input,
//         SDPActiveOp {
//             declaration_id,
//             nonce,
//             metadata,
//         },
//     ))
// }
