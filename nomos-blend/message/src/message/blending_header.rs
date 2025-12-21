use key_management_system_keys::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature,
    UnsecuredEd25519Key,
};
use nomos_blend_crypto::pseudo_random_sized_bytes;
use nomos_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota, VerifiedProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, ProofOfSelection, VerifiedProofOfSelection},
};
use serde::{Deserialize, Serialize};

use crate::crypto::domains;

/// A blending header that is fully decapsulated.
/// This must be encapsulated when being sent to the blend network.
#[derive(Clone, Serialize, Deserialize)]
pub struct BlendingHeader {
    pub signing_pubkey: Ed25519PublicKey,
    pub proof_of_quota: ProofOfQuota,
    pub signature: Ed25519Signature,
    pub proof_of_selection: ProofOfSelection,
    pub is_last: bool,
}

impl BlendingHeader {
    /// Build a blending header with random data based on the provided key.
    /// in the reconstructable way.
    /// Each field in the header is filled with pseudo-random bytes derived from
    /// the key concatenated with a unique byte (1, 2, 3, or 4).
    pub fn pseudo_random(key: &[u8]) -> Self {
        let r1 = pseudo_random_sized_bytes::<ED25519_PUBLIC_KEY_SIZE>(
            domains::INITIALIZATION,
            &concat(key, &[1]),
        );
        let r2 = pseudo_random_sized_bytes::<PROOF_OF_QUOTA_SIZE>(
            domains::INITIALIZATION,
            &concat(key, &[2]),
        );
        let r3 = pseudo_random_sized_bytes::<ED25519_SIGNATURE_SIZE>(
            domains::INITIALIZATION,
            &concat(key, &[3]),
        );
        let r4 = pseudo_random_sized_bytes::<PROOF_OF_SELECTION_SIZE>(
            domains::INITIALIZATION,
            &concat(key, &[4]),
        );
        Self {
            // Unlike the spec, derive a private key from random bytes
            // and then derive the public key from it
            // because a public key cannot always be successfully derived from random bytes.
            // TODO: This will be changed once we have zerocopy serde.
            signing_pubkey: UnsecuredEd25519Key::from_bytes(&r1).public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked(r2).into_inner(),
            signature: Ed25519Signature::from_bytes(&r3),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked(r4).into_inner(),
            is_last: false,
        }
    }
}

fn concat(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().chain(b.iter()).copied().collect::<Vec<_>>()
}
