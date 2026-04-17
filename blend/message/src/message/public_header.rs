use lb_blend_proofs::quota::{self, ProofOfQuota, VerifiedProofOfQuota};
use lb_key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature};
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{Error, MessageIdentifier, encap::ProofsVerifier};

const LATEST_BLEND_MESSAGE_VERSION: u8 = 1;

// A public header that is revealed to all nodes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PublicHeader {
    #[serde(deserialize_with = "deserialize_version_number")]
    version: u8,
    signing_pubkey: Ed25519PublicKey,
    proof_of_quota: ProofOfQuota,
    signature: Ed25519Signature,
}

fn deserialize_version_number<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u8::deserialize(deserializer)?;
    if version == LATEST_BLEND_MESSAGE_VERSION {
        Ok(version)
    } else {
        Err(de::Error::custom(format!(
            "Unsupported message version: {version}",
        )))
    }
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

    pub fn verify_signature(
        &self,
        body: &[u8],
    ) -> Result<PublicHeaderWithVerifiedSignature, Error> {
        if self.signing_pubkey.verify(body, &self.signature).is_ok() {
            Ok(PublicHeaderWithVerifiedSignature {
                version: self.version,
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
pub struct PublicHeaderWithVerifiedSignature {
    version: u8,
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
            ..
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
        let (version, signing_pubkey, _, signature) =
            PublicHeader::new(signing_pubkey, &proof_of_quota, signature).into_components();
        Self {
            version,
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

    pub const fn into_components(self) -> (u8, Ed25519PublicKey, ProofOfQuota, Ed25519Signature) {
        (
            self.version,
            self.signing_pubkey,
            self.proof_of_quota,
            self.signature,
        )
    }

    pub const fn id(&self) -> MessageIdentifier {
        self.signing_pubkey
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn signature_mut(&mut self) -> &mut Ed25519Signature {
        &mut self.signature
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct VerifiedPublicHeader {
    version: u8,
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
            ..
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

    #[must_use]
    pub const fn signing_key(&self) -> &Ed25519PublicKey {
        &self.signing_pubkey
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
    use lb_blend_proofs::quota::VerifiedProofOfQuota;
    use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
    use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};

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

    #[test]
    fn serde_invalid_version_number() {
        let header_with_invalid_version = PublicHeader {
            version: 2,
            signing_pubkey: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            proof_of_quota: [1; _].try_into().unwrap(),
            signature: [2; _].into(),
        };

        let serialized_header = header_with_invalid_version.to_bytes().unwrap();
        PublicHeader::from_bytes(&serialized_header).unwrap_err();
    }
}
