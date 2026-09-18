use lb_blend_proofs::quota::{self, PROOF_OF_QUOTA_SIZE, ProofOfQuota, VerifiedProofOfQuota};
use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_key_management_system_keys::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature,
};
use serde::{Deserialize, Serialize};

use crate::{Error, MessageIdentifier, encap::ProofsVerifier};

/// The exact number of bytes a [`PublicHeader`] encodes to (fixed-size fields
/// only). Compile-time constant.
pub const PUBLIC_HEADER_ENCODED_SIZE: usize =
    ED25519_PUBLIC_KEY_SIZE + PROOF_OF_QUOTA_SIZE + ED25519_SIGNATURE_SIZE;

// A public header that is revealed to all nodes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PublicHeader {
    signing_pubkey: Ed25519PublicKey,
    proof_of_quota: ProofOfQuota,
    signature: Ed25519Signature,
}

impl PublicHeader {
    pub const fn new(
        signing_pubkey: Ed25519PublicKey,
        proof_of_quota: &ProofOfQuota,
        signature: Ed25519Signature,
    ) -> Self {
        Self {
            proof_of_quota: *proof_of_quota,
            signature,
            signing_pubkey,
        }
    }

    pub fn verify_signature(
        &self,
        body: &[u8],
    ) -> Result<PublicHeaderWithVerifiedSignature, Error> {
        if self.signing_pubkey.verify(body, &self.signature).is_ok() {
            Ok(PublicHeaderWithVerifiedSignature {
                signing_pubkey: self.signing_pubkey,
                proof_of_quota: self.proof_of_quota,
                signature: self.signature,
            })
        } else {
            Err(Error::SignatureVerificationFailed)
        }
    }

    pub fn verify_proof_of_quota<Verifier>(&self, verifier: &Verifier) -> Result<(), Error>
    where
        Verifier: ProofsVerifier,
    {
        verifier
            .verify_proof_of_quota(self.proof_of_quota, &self.signing_pubkey)
            .map_err(|_| Error::ProofOfQuotaVerificationFailed(quota::Error::InvalidProof))?;
        Ok(())
    }

    pub const fn signing_pubkey(&self) -> &Ed25519PublicKey {
        &self.signing_pubkey
    }

    pub const fn proof_of_quota(&self) -> &ProofOfQuota {
        &self.proof_of_quota
    }

    pub const fn signature(&self) -> &Ed25519Signature {
        &self.signature
    }

    pub const fn into_components(self) -> (Ed25519PublicKey, ProofOfQuota, Ed25519Signature) {
        (self.signing_pubkey, self.proof_of_quota, self.signature)
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub const fn signature_mut(&mut self) -> &mut Ed25519Signature {
        &mut self.signature
    }
}

impl BinaryEncode for PublicHeader {
    fn encoded_length(&self) -> usize {
        PUBLIC_HEADER_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.signing_pubkey.encode_into(out);
        self.proof_of_quota.encode_into(out);
        self.signature.encode_into(out);
    }
}

impl BinaryDecode for PublicHeader {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, signing_pubkey) = Ed25519PublicKey::decode(input, &())?;
        let (input, proof_of_quota) = ProofOfQuota::decode(input, &())?;
        let (input, signature) = Ed25519Signature::decode(input, &())?;
        Ok((
            input,
            Self {
                signing_pubkey,
                proof_of_quota,
                signature,
            },
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PublicHeaderWithVerifiedSignature {
    signing_pubkey: Ed25519PublicKey,
    proof_of_quota: ProofOfQuota,
    signature: Ed25519Signature,
}

impl From<PublicHeaderWithVerifiedSignature> for PublicHeader {
    fn from(
        PublicHeaderWithVerifiedSignature {
            signing_pubkey,
            proof_of_quota,
            signature,
        }: PublicHeaderWithVerifiedSignature,
    ) -> Self {
        Self::new(signing_pubkey, &proof_of_quota, signature)
    }
}

impl PublicHeaderWithVerifiedSignature {
    pub const fn new(
        proof_of_quota: ProofOfQuota,
        signing_pubkey: Ed25519PublicKey,
        signature: Ed25519Signature,
    ) -> Self {
        Self {
            signing_pubkey,
            proof_of_quota,
            signature,
        }
    }

    pub fn verify_proof_of_quota<Verifier>(
        self,
        verifier: &Verifier,
    ) -> Result<VerifiedPublicHeader, Error>
    where
        Verifier: ProofsVerifier,
    {
        let verified_proof_of_quota = verifier
            .verify_proof_of_quota(self.proof_of_quota, &self.signing_pubkey)
            .map_err(|_| Error::ProofOfQuotaVerificationFailed(quota::Error::InvalidProof))?;
        Ok(VerifiedPublicHeader::new(
            verified_proof_of_quota,
            self.signing_pubkey,
            self.signature,
        ))
    }

    pub const fn into_components(self) -> (Ed25519PublicKey, ProofOfQuota, Ed25519Signature) {
        (self.signing_pubkey, self.proof_of_quota, self.signature)
    }

    pub const fn id(&self) -> MessageIdentifier {
        self.proof_of_quota.key_nullifier()
    }

    #[must_use]
    pub const fn signing_key(&self) -> &Ed25519PublicKey {
        &self.signing_pubkey
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn signature_mut(&mut self) -> &mut Ed25519Signature {
        &mut self.signature
    }
}

// The verified public-header variants are never decoded from the wire (a peer's
// bytes always decode into an unverified `PublicHeader`); they only need to
// encode, and all three variants produce identical bytes. Implementing only
// `BinaryEncode` for them means a verified message can be serialized directly,
// with no conversion/copy through `PublicHeader`.
impl BinaryEncode for PublicHeaderWithVerifiedSignature {
    fn encoded_length(&self) -> usize {
        PUBLIC_HEADER_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.signing_pubkey.encode_into(out);
        self.proof_of_quota.encode_into(out);
        self.signature.encode_into(out);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct VerifiedPublicHeader {
    signing_pubkey: Ed25519PublicKey,
    proof_of_quota: VerifiedProofOfQuota,
    signature: Ed25519Signature,
}

impl From<VerifiedPublicHeader> for PublicHeaderWithVerifiedSignature {
    fn from(
        VerifiedPublicHeader {
            proof_of_quota,
            signature,
            signing_pubkey,
        }: VerifiedPublicHeader,
    ) -> Self {
        Self::new(proof_of_quota.into_inner(), signing_pubkey, signature)
    }
}

impl From<VerifiedPublicHeader> for PublicHeader {
    fn from(
        VerifiedPublicHeader {
            proof_of_quota,
            signature,
            signing_pubkey,
        }: VerifiedPublicHeader,
    ) -> Self {
        Self::new(signing_pubkey, &proof_of_quota.into(), signature)
    }
}

impl VerifiedPublicHeader {
    pub const fn new(
        proof_of_quota: VerifiedProofOfQuota,
        signing_pubkey: Ed25519PublicKey,
        signature: Ed25519Signature,
    ) -> Self {
        Self {
            signing_pubkey,
            proof_of_quota,
            signature,
        }
    }

    pub const fn from_header_unchecked(
        PublicHeader {
            proof_of_quota,
            signature,
            signing_pubkey,
        }: &PublicHeader,
    ) -> Self {
        Self {
            signing_pubkey: *signing_pubkey,
            proof_of_quota: VerifiedProofOfQuota::from_proof_of_quota_unchecked(*proof_of_quota),
            signature: *signature,
        }
    }

    #[must_use]
    pub const fn proof_of_quota(&self) -> &VerifiedProofOfQuota {
        &self.proof_of_quota
    }

    #[must_use]
    pub const fn signing_key(&self) -> &Ed25519PublicKey {
        &self.signing_pubkey
    }

    pub const fn id(&self) -> MessageIdentifier {
        self.proof_of_quota.key_nullifier()
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn signature_mut(&mut self) -> &mut Ed25519Signature {
        &mut self.signature
    }

    #[must_use]
    pub const fn into_components(
        self,
    ) -> (Ed25519PublicKey, VerifiedProofOfQuota, Ed25519Signature) {
        (self.signing_pubkey, self.proof_of_quota, self.signature)
    }
}

impl BinaryEncode for VerifiedPublicHeader {
    fn encoded_length(&self) -> usize {
        PUBLIC_HEADER_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.signing_pubkey.encode_into(out);
        self.proof_of_quota.as_ref().encode_into(out);
        self.signature.encode_into(out);
    }
}

#[cfg(test)]
mod tests {
    use lb_blend_proofs::quota::VerifiedProofOfQuota;
    use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
    use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};

    use crate::message::{PublicHeader, public_header::VerifiedPublicHeader};

    #[test]
    fn serde_verified_and_unverified() {
        let verified_header = VerifiedPublicHeader {
            signing_pubkey: Ed25519PublicKey::from_bytes(&[200; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([201; _]),
            signature: [202; 64].into(),
        };
        let serialized_header = verified_header.to_bytes().unwrap();

        let deserialized_as_unverified = PublicHeader::from_bytes(&serialized_header).unwrap();
        assert_eq!(deserialized_as_unverified, verified_header.into());
    }
}
