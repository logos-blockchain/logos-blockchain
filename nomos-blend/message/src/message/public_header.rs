use key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature};
use nomos_blend_proofs::quota::{self, ProofOfQuota, VerifiedProofOfQuota};
use serde::{Deserialize, Serialize};

use crate::{Error, MessageIdentifier, encap::ProofsVerifier};

const LATEST_BLEND_MESSAGE_VERSION: u8 = 1;

// A public header that is revealed to all nodes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PublicHeader {
    version: u8,
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
            version: LATEST_BLEND_MESSAGE_VERSION,
        }
    }

    pub fn verify_signature(&self, body: &[u8]) -> Result<(), Error> {
        if self.signing_pubkey.verify(body, &self.signature).is_ok() {
            Ok(())
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

    pub const fn into_components(self) -> (u8, Ed25519PublicKey, ProofOfQuota, Ed25519Signature) {
        (
            self.version,
            self.signing_pubkey,
            self.proof_of_quota,
            self.signature,
        )
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub const fn signature_mut(&mut self) -> &mut Ed25519Signature {
        &mut self.signature
    }

    #[cfg(test)]
    pub const fn proof_of_quota_mut(&mut self) -> &mut ProofOfQuota {
        &mut self.proof_of_quota
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct VerifiedPublicHeader {
    version: u8,
    signing_pubkey: Ed25519PublicKey,
    proof_of_quota: VerifiedProofOfQuota,
    signature: Ed25519Signature,
}

impl From<VerifiedPublicHeader> for PublicHeader {
    fn from(
        VerifiedPublicHeader {
            proof_of_quota,
            signature,
            signing_pubkey,
            ..
        }: VerifiedPublicHeader,
    ) -> Self {
        Self::new(signing_pubkey, &proof_of_quota.into(), signature)
    }
}

impl VerifiedPublicHeader {
    pub fn new(
        proof_of_quota: VerifiedProofOfQuota,
        signing_pubkey: Ed25519PublicKey,
        signature: Ed25519Signature,
    ) -> Self {
        let (version, signing_pubkey, _, signature) =
            PublicHeader::new(signing_pubkey, proof_of_quota.as_ref(), signature).into_components();
        Self {
            version,
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
            version,
        }: &PublicHeader,
    ) -> Self {
        Self {
            version: *version,
            signing_pubkey: *signing_pubkey,
            proof_of_quota: VerifiedProofOfQuota::from_proof_of_quota_unchecked(*proof_of_quota),
            signature: *signature,
        }
    }

    #[must_use]
    pub const fn proof_of_quota(&self) -> &VerifiedProofOfQuota {
        &self.proof_of_quota
    }

    pub const fn id(&self) -> MessageIdentifier {
        self.signing_pubkey
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn signature_mut(&mut self) -> &mut Ed25519Signature {
        &mut self.signature
    }

    #[must_use]
    pub const fn into_components(
        self,
    ) -> (u8, Ed25519PublicKey, VerifiedProofOfQuota, Ed25519Signature) {
        (
            self.version,
            self.signing_pubkey,
            self.proof_of_quota,
            self.signature,
        )
    }
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
    use nomos_blend_proofs::quota::VerifiedProofOfQuota;
    use nomos_core::codec::{DeserializeOp as _, SerializeOp as _};

    use crate::message::{PublicHeader, public_header::VerifiedPublicHeader};

    #[test]
    fn serde_verified_and_unverified() {
        let verified_header = VerifiedPublicHeader {
            version: 1,
            signing_pubkey: Ed25519PublicKey::from_bytes(&[200; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([201; _]),
            signature: [202; 64].into(),
        };
        let serialized_header = verified_header.to_bytes().unwrap();

        let deserialized_as_unverified = PublicHeader::from_bytes(&serialized_header).unwrap();
        assert_eq!(deserialized_as_unverified, verified_header.into());
    }
}
