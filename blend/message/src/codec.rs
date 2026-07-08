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

    fn encoded_length(context: Self::Context) -> usize;
    fn encode_into(&self, out: &mut Vec<u8>);
    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()>;
}

impl WireCodec for u8 {
    type Context = ();

    fn encoded_length(_context: Self::Context) -> usize {
        1
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(*self);
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < 1 {
            return Err(());
        }
        let value = input[0];
        let remaining = &input[1..];
        Ok((remaining, value))
    }
}

impl WireCodec for bool {
    type Context = ();

    fn encoded_length(context: Self::Context) -> usize {
        u8::encoded_length(context)
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        (*self as u8).encode_into(out);
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
        let (remaining, value) = u8::decode(input, context)?;
        Ok((remaining, Self::try_from(value).map_err(|_| ())?))
    }
}

impl WireCodec for Ed25519PublicKey {
    type Context = ();

    fn encoded_length(context: Self::Context) -> usize {
        ED25519_PUBLIC_KEY_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
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

    fn encoded_length(context: Self::Context) -> usize {
        PROOF_OF_QUOTA_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < PROOF_OF_QUOTA_SIZE {
            return Err(());
        }
        let (proof_bytes, remaining) = input.split_at(PROOF_OF_QUOTA_SIZE);
        let proof_array: [u8; _] = proof_bytes.try_into().map_err(|_| ())?;
        let proof = Self::from_bytes(&proof_array).map_err(|_| ())?;
        Ok((remaining, proof))
    }
}

impl WireCodec for ProofOfSelection {
    type Context = ();

    fn encoded_length(context: Self::Context) -> usize {
        PROOF_OF_SELECTION_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < PROOF_OF_SELECTION_SIZE {
            return Err(());
        }
        let (proof_bytes, remaining) = input.split_at(PROOF_OF_SELECTION_SIZE);
        let proof_array: [u8; _] = proof_bytes.try_into().map_err(|_| ())?;
        let proof = Self::from_bytes(&proof_array).map_err(|_| ())?;
        Ok((remaining, proof))
    }
}

impl WireCodec for Ed25519Signature {
    type Context = ();

    fn encoded_length(context: Self::Context) -> usize {
        ED25519_SIGNATURE_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }

    fn decode(input: &[u8], context: Self::Context) -> Result<(&[u8], Self), ()> {
        if input.len() < ED25519_SIGNATURE_SIZE {
            return Err(());
        }
        let (sig_bytes, remaining) = input.split_at(ED25519_SIGNATURE_SIZE);
        let sig_array: [u8; _] = sig_bytes.try_into().map_err(|_| ())?;
        let signature = Self::from_bytes(&sig_array).map_err(|_| ())?;
        Ok((remaining, signature))
    }
}
