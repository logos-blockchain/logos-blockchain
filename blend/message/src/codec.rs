use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, ProofOfSelection},
};
use lb_key_management_system_keys::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature,
};

/// Append a message component's fixed-size, prefix-free wire bytes to `out`.
pub trait WireCodec: Sized {
    type Context;

    fn encoded_length(_context: Self::Context) -> usize {
        size_of::<Self>()
    }

    fn encode_into(&self, out: &mut Vec<u8>);

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()>;
}

impl WireCodec for u8 {
    type Context = ();

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(*self);
    }

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), ()> {
        if input.is_empty() {
            return Err(());
        }
        let value = input[0];
        let remaining = &input[1..];
        Ok((remaining, value))
    }
}

impl WireCodec for u16 {
    type Context = ();

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < size_of::<Self>() {
            return Err(());
        }
        let (bytes, remaining) = input.split_at(size_of::<Self>());
        let value = Self::from_le_bytes(bytes.try_into().map_err(|_| ())?);
        Ok((remaining, value))
    }
}

impl WireCodec for bool {
    type Context = ();

    fn encode_into(&self, out: &mut Vec<u8>) {
        u8::from(*self).encode_into(out);
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
        let (remaining, value) = u8::decode(input, context)?;
        Ok((remaining, Self::try_from(value).map_err(|_| ())?))
    }
}

impl WireCodec for Ed25519PublicKey {
    type Context = ();

    // Must be explicit: `size_of::<Ed25519PublicKey>()` is NOT the wire size — a
    // `VerifyingKey` also caches the decompressed point, so it is far larger than
    // the 32 bytes `encode_into` actually writes.
    fn encoded_length((): Self::Context) -> usize {
        ED25519_PUBLIC_KEY_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < ED25519_PUBLIC_KEY_SIZE {
            return Err(());
        }
        let (key_bytes, remaining) = input.split_at(ED25519_PUBLIC_KEY_SIZE);
        let key_array: [u8; _] = key_bytes.try_into().map_err(|_| ())?;
        let public_key = Self::from_bytes(&key_array).map_err(|_| ())?;
        Ok((remaining, public_key))
    }
}

impl WireCodec for ProofOfQuota {
    type Context = ();

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&<[u8; _]>::from(self)[..]);
    }

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < PROOF_OF_QUOTA_SIZE {
            return Err(());
        }
        let (proof_bytes, remaining) = input.split_at(PROOF_OF_QUOTA_SIZE);
        let proof_array: [u8; _] = proof_bytes.try_into().map_err(|_| ())?;
        let proof = proof_array.try_into().map_err(|_| ())?;
        Ok((remaining, proof))
    }
}

impl WireCodec for ProofOfSelection {
    type Context = ();

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&<[u8; _]>::from(self)[..]);
    }

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < PROOF_OF_SELECTION_SIZE {
            return Err(());
        }
        let (proof_bytes, remaining) = input.split_at(PROOF_OF_SELECTION_SIZE);
        let proof_array: [u8; _] = proof_bytes.try_into().map_err(|_| ())?;
        let proof = proof_array.try_into().map_err(|_| ())?;
        Ok((remaining, proof))
    }
}

impl WireCodec for Ed25519Signature {
    type Context = ();

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_bytes()[..]);
    }

    fn decode(input: &[u8], (): Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < ED25519_SIGNATURE_SIZE {
            return Err(());
        }
        let (sig_bytes, remaining) = input.split_at(ED25519_SIGNATURE_SIZE);
        let sig_array: [u8; _] = sig_bytes.try_into().map_err(|_| ())?;
        let signature = sig_array.into();
        Ok((remaining, signature))
    }
}
