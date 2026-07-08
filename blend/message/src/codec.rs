use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, ProofOfSelection},
};
use lb_key_management_system_keys::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature,
};

/// A content error from decoding a wire component.
///
/// Length is deliberately NOT checked by any [`WireDecode`] implementation: the
/// caller (the network-side size gate that rejects wrongly-sized peer messages
/// up front) guarantees the input holds at least the bytes the component needs.
/// These variants therefore only cover malformed *values*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireDecodeError {
    #[error("Invalid boolean encoding")]
    InvalidBool,
    #[error("Invalid Ed25519 public key encoding")]
    InvalidPublicKey,
    #[error("Invalid proof of quota encoding")]
    InvalidProofOfQuota,
    #[error("Invalid proof of selection encoding")]
    InvalidProofOfSelection,
    #[error("Unsupported message version")]
    UnsupportedVersion,
    #[error("Invalid payload type discriminant")]
    InvalidPayloadType,
}

/// Append a message component's fixed-size, prefix-free wire bytes to `out`.
///
/// Every Blend message component has a size fully determined by the (fixed,
/// network-wide) number of encapsulation layers, so nothing is length-prefixed.
/// The output buffer is allocated by the caller; implementations only append.
pub trait WireEncode {
    fn encode_into(&self, out: &mut Vec<u8>);
}

/// Decode a message component from the front of `input`, returning the value
/// and the unconsumed remainder (`(rest, value)`, as in `nom`).
///
/// Implementations do not check `input`'s length — the caller guarantees it is
/// large enough (see [`WireDecodeError`]). `Context` carries anything the
/// decoder needs that is not on the wire (e.g. the layer count); `()` for
/// self-describing fixed-size components.
pub trait WireDecode: Sized {
    type Context;

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), WireDecodeError>;
}

impl WireEncode for u8 {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(*self);
    }
}

impl WireDecode for u8 {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        Ok((&input[1..], input[0]))
    }
}

impl WireEncode for u16 {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
}

impl WireDecode for u16 {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        let (bytes, remaining) = input.split_at(size_of::<Self>());
        let value = Self::from_le_bytes(bytes.try_into().expect("split_at guarantees the length"));
        Ok((remaining, value))
    }
}

impl WireEncode for bool {
    fn encode_into(&self, out: &mut Vec<u8>) {
        u8::from(*self).encode_into(out);
    }
}

impl WireDecode for bool {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        let (remaining, value) = u8::decode(input, ())?;
        match value {
            0 => Ok((remaining, false)),
            1 => Ok((remaining, true)),
            _ => Err(WireDecodeError::InvalidBool),
        }
    }
}

impl WireEncode for Ed25519PublicKey {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }
}

impl WireDecode for Ed25519PublicKey {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        let (key_bytes, remaining) = input.split_at(ED25519_PUBLIC_KEY_SIZE);
        let key_array: [u8; ED25519_PUBLIC_KEY_SIZE] = key_bytes
            .try_into()
            .expect("split_at guarantees the length");
        let public_key =
            Self::from_bytes(&key_array).map_err(|_| WireDecodeError::InvalidPublicKey)?;
        Ok((remaining, public_key))
    }
}

impl WireEncode for ProofOfQuota {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&<[u8; PROOF_OF_QUOTA_SIZE]>::from(self));
    }
}

impl WireDecode for ProofOfQuota {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        let (proof_bytes, remaining) = input.split_at(PROOF_OF_QUOTA_SIZE);
        let proof_array: [u8; PROOF_OF_QUOTA_SIZE] = proof_bytes
            .try_into()
            .expect("split_at guarantees the length");
        let proof =
            Self::try_from(proof_array).map_err(|_| WireDecodeError::InvalidProofOfQuota)?;
        Ok((remaining, proof))
    }
}

impl WireEncode for ProofOfSelection {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&<[u8; PROOF_OF_SELECTION_SIZE]>::from(self));
    }
}

impl WireDecode for ProofOfSelection {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        let (proof_bytes, remaining) = input.split_at(PROOF_OF_SELECTION_SIZE);
        let proof_array: [u8; PROOF_OF_SELECTION_SIZE] = proof_bytes
            .try_into()
            .expect("split_at guarantees the length");
        let proof =
            Self::try_from(proof_array).map_err(|_| WireDecodeError::InvalidProofOfSelection)?;
        Ok((remaining, proof))
    }
}

impl WireEncode for Ed25519Signature {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_bytes());
    }
}

impl WireDecode for Ed25519Signature {
    type Context = ();

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), WireDecodeError> {
        let (sig_bytes, remaining) = input.split_at(ED25519_SIGNATURE_SIZE);
        let sig_array: [u8; ED25519_SIGNATURE_SIZE] = sig_bytes
            .try_into()
            .expect("split_at guarantees the length");
        // `Ed25519Signature::from_bytes` is infallible (any bytes are a valid
        // signature value; verification happens elsewhere).
        Ok((remaining, Self::from_bytes(&sig_array)))
    }
}
