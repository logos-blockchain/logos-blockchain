use lb_poc::{PoCProof, PoCVerifierInput};
use lb_zksign::{ZkSignProof, ZkSignVerifierInputs};

/// A proof that can be handed off to a [`DeferredProofs`] batch.
pub trait DeferrableProof {
    fn defer_into(self, batch: &mut DeferredProofs);
}

impl DeferrableProof for DeferredProof {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.push(self);
    }
}

/// A ZKP verification deferred while processing an operation.
#[expect(
    clippy::large_enum_variant,
    reason = "This is short-lived; each is pushed into DeferredProofs almost immediately, \
    which stores the two kinds in separate vectors. Also, most of them are the larger ZkSig variant."
)]
#[derive(Debug)]
pub enum DeferredProof {
    ZkSig(ZkSignProof, ZkSignVerifierInputs),
    LeaderClaim(PoCProof, PoCVerifierInput),
}

/// ZKP verifications deferred while applying a block.
#[derive(Default)]
#[must_use]
pub struct DeferredProofs {
    zk_sigs: Vec<(ZkSignProof, ZkSignVerifierInputs)>,
    leader_claims: Vec<(PoCProof, PoCVerifierInput)>,
}

impl DeferredProofs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, proof: DeferredProof) {
        match proof {
            DeferredProof::ZkSig(zk_sig, inputs) => self.zk_sigs.push((zk_sig, inputs)),
            DeferredProof::LeaderClaim(claim_proof, public) => {
                self.leader_claims.push((claim_proof, public));
            }
        }
    }

    pub fn extend(&mut self, other: Self) {
        self.zk_sigs.extend(other.zk_sigs);
        self.leader_claims.extend(other.leader_claims);
    }

    pub fn verify(self) -> Result<(), Error> {
        Self::verify_zk_sigs(&self.zk_sigs)?;
        Self::verify_leader_claims(&self.leader_claims)
    }

    fn verify_zk_sigs(zk_sigs: &[(ZkSignProof, ZkSignVerifierInputs)]) -> Result<(), Error> {
        if zk_sigs.is_empty() {
            return Ok(());
        }

        match lb_zksign::batch_verify(zk_sigs) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::InvalidZkSignatures),
            Err(e) => Err(Error::MalformedZkSignature(format!("{e:?}"))),
        }
    }

    fn verify_leader_claims(proofs: &[(PoCProof, PoCVerifierInput)]) -> Result<(), Error> {
        if proofs.is_empty() {
            return Ok(());
        }

        match lb_poc::batch_verify(proofs) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::InvalidLeaderClaimProofs),
            Err(e) => Err(Error::MalformedLeaderClaimProof(format!("{e:?}"))),
        }
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn zk_sigs(&self) -> &[(ZkSignProof, ZkSignVerifierInputs)] {
        &self.zk_sigs
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn leader_claims(&self) -> &[(PoCProof, PoCVerifierInput)] {
        &self.leader_claims
    }
}

impl<Proof: DeferrableProof> FromIterator<Proof> for DeferredProofs {
    fn from_iter<T: IntoIterator<Item = Proof>>(iter: T) -> Self {
        let mut batch = Self::new();
        for item in iter {
            item.defer_into(&mut batch);
        }
        batch
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("deferred ZkSignatures are invalid")]
    InvalidZkSignatures,
    #[error("deferred ZkSignature is malformed: {0}")]
    MalformedZkSignature(String),
    #[error("deferred leader claim proofs are invalid")]
    InvalidLeaderClaimProofs,
    #[error("deferred leader claim proof is malformed: {0}")]
    MalformedLeaderClaimProof(String),
}

#[cfg(test)]
pub mod test_utils {
    use super::{DeferrableProof, DeferredProofs, Error};

    pub fn batch_verify(proof: impl DeferrableProof) -> Result<(), Error> {
        DeferredProofs::from_iter([proof]).verify()
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{ZkKey, public_inputs_from_pks};
    use lb_mmr::MerkleMountainRange;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        crypto::ZkHasher,
        mantle::ops::leader_claim::{RewardsRoot, VoucherCm, VoucherNullifier, VoucherSecret},
        proofs::leader_claim_proof::{
            Groth16LeaderClaimProof, LeaderClaimPrivate, LeaderClaimPublic,
        },
    };

    #[test]
    fn verify_accepts_empty_batch() {
        DeferredProofs::new().verify().expect("must succeed");
    }

    #[test]
    fn verify_accepts_batch_of_valid_zk_signatures() {
        [valid_zk_sig(7), valid_zk_sig(8), valid_zk_sig(9)]
            .into_iter()
            .collect::<DeferredProofs>()
            .verify()
            .expect("must succeed");
    }

    #[test]
    fn verify_rejects_batch_containing_invalid_zk_signature() {
        let err = [valid_zk_sig(7), invalid_zk_sig(8), valid_zk_sig(9)]
            .into_iter()
            .collect::<DeferredProofs>()
            .verify()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidZkSignatures));
    }

    #[test]
    fn verify_accepts_batch_of_valid_leader_claims() {
        [valid_leader_claim(7), valid_leader_claim(8)]
            .into_iter()
            .collect::<DeferredProofs>()
            .verify()
            .expect("must succeed");
    }

    #[test]
    fn verify_rejects_batch_containing_invalid_leader_claim() {
        let err = [valid_leader_claim(7), invalid_leader_claim(8)]
            .into_iter()
            .collect::<DeferredProofs>()
            .verify()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidLeaderClaimProofs));
    }

    #[test]
    fn verify_rejects_invalid_leader_claim_alongside_valid_zk_signature() {
        let err = [valid_zk_sig(7), invalid_leader_claim(8)]
            .into_iter()
            .collect::<DeferredProofs>()
            .verify()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidLeaderClaimProofs));
    }

    fn valid_zk_sig(message: u64) -> DeferredProof {
        zk_sig(message, message)
    }

    fn invalid_zk_sig(message: u64) -> DeferredProof {
        zk_sig(message + 1, message)
    }

    /// Signs `msg`, but pairs the proof with the public inputs the
    /// verifier checks it against: those of `msg_for_input`.
    /// If `msg != msg_for_input`, an invalid proof will be produced.
    fn zk_sig(msg: u64, msg_for_input: u64) -> DeferredProof {
        let key = ZkKey::from(BigUint::from(1u8));
        let signature = ZkKey::multi_sign(std::slice::from_ref(&key), &Fr::from(msg)).unwrap();
        let inputs =
            public_inputs_from_pks(Fr::from(msg_for_input).into(), &[key.to_public_key()]).unwrap();

        DeferredProof::ZkSig(*signature.as_proof(), inputs)
    }

    fn valid_leader_claim(voucher: u64) -> DeferredProof {
        leader_claim(voucher, voucher)
    }

    fn invalid_leader_claim(voucher: u64) -> DeferredProof {
        leader_claim(voucher + 1, voucher)
    }

    /// Proves a claim over the voucher of `secret`, but pairs the proof
    /// with the nullifier the verifier checks it against: that of
    /// `secret_for_input`.
    /// If `secret != secret_for_input`, an invalid proof will be produced.
    fn leader_claim(secret: u64, secret_for_input: u64) -> DeferredProof {
        let voucher_secret = VoucherSecret::from(Fr::from(secret));
        let (mmr, voucher_path) = MerkleMountainRange::<VoucherCm, ZkHasher>::new()
            .push_with_paths(VoucherCm::from_secret(voucher_secret), &mut [])
            .expect("MMR shouldn't be full");
        let voucher_root = RewardsRoot::from(mmr.frontier_root());
        let tx_hash = Fr::from(11u64);
        let proof = Groth16LeaderClaimProof::prove(
            LeaderClaimPrivate::try_new(
                LeaderClaimPublic::new(
                    VoucherNullifier::from_secret(voucher_secret).into(),
                    voucher_root.into(),
                    tx_hash,
                ),
                &voucher_path,
                voucher_secret,
            )
            .expect("voucher path should match the PoC circuit height"),
        )
        .expect("proof generation should succeed");

        DeferredProof::LeaderClaim(
            *proof.proof(),
            PoCVerifierInput::new(
                VoucherNullifier::from_secret(VoucherSecret::from(Fr::from(secret_for_input)))
                    .into(),
                voucher_root.into(),
                tx_hash,
            ),
        )
    }
}
