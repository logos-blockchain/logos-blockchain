use lb_poc::{PoCProof, PoCVerifierInput};
use lb_zksign::{ZkSignProof, ZkSignVerifierInputs};

/// A ZKP verification deferred while processing an operation.
#[expect(
    clippy::large_enum_variant,
    reason = "This is short-lived; each is pushed into DeferredZkpVerifications almost immediately, \
    which stores the two kinds in separate vectors. Also, most of them are the larger ZkSig variant."
)]
pub enum DeferredZkpVerification {
    ZkSig(ZkSignProof, ZkSignVerifierInputs),
    LeaderClaim(PoCProof, PoCVerifierInput),
}

/// ZKP verifications deferred while applying a block.
#[derive(Default)]
pub struct DeferredZkpVerifications {
    zk_sigs: Vec<(ZkSignProof, ZkSignVerifierInputs)>,
    leader_claims: Vec<(PoCProof, PoCVerifierInput)>,
}

impl DeferredZkpVerifications {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, verification: DeferredZkpVerification) {
        match verification {
            DeferredZkpVerification::ZkSig(zk_sig, inputs) => self.zk_sigs.push((zk_sig, inputs)),
            DeferredZkpVerification::LeaderClaim(proof, public) => {
                self.leader_claims.push((proof, public));
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
}

impl FromIterator<DeferredZkpVerification> for DeferredZkpVerifications {
    fn from_iter<T: IntoIterator<Item = DeferredZkpVerification>>(iter: T) -> Self {
        let mut verifications = Self::new();
        for verification in iter {
            verifications.push(verification);
        }
        verifications
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
        DeferredZkpVerifications::new()
            .verify()
            .expect("must succeed");
    }

    #[test]
    fn verify_accepts_batch_of_valid_zk_signatures() {
        [valid_zk_sig(7), valid_zk_sig(8), valid_zk_sig(9)]
            .into_iter()
            .collect::<DeferredZkpVerifications>()
            .verify()
            .expect("must succeed");
    }

    #[test]
    fn verify_rejects_batch_containing_invalid_zk_signature() {
        let err = [valid_zk_sig(7), invalid_zk_sig(8), valid_zk_sig(9)]
            .into_iter()
            .collect::<DeferredZkpVerifications>()
            .verify()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidZkSignatures));
    }

    #[test]
    fn verify_accepts_batch_of_valid_leader_claims() {
        [valid_leader_claim(7), valid_leader_claim(8)]
            .into_iter()
            .collect::<DeferredZkpVerifications>()
            .verify()
            .expect("must succeed");
    }

    #[test]
    fn verify_rejects_batch_containing_invalid_leader_claim() {
        let err = [valid_leader_claim(7), invalid_leader_claim(8)]
            .into_iter()
            .collect::<DeferredZkpVerifications>()
            .verify()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidLeaderClaimProofs));
    }

    #[test]
    fn verify_rejects_invalid_leader_claim_alongside_valid_zk_signature() {
        let err = [valid_zk_sig(7), invalid_leader_claim(8)]
            .into_iter()
            .collect::<DeferredZkpVerifications>()
            .verify()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidLeaderClaimProofs));
    }

    fn valid_zk_sig(message: u64) -> DeferredZkpVerification {
        zk_sig(message, message)
    }

    fn invalid_zk_sig(message: u64) -> DeferredZkpVerification {
        zk_sig(message + 1, message)
    }

    /// Signs `msg`, but pairs the proof with the public inputs the
    /// verifier checks it against: those of `msg_for_input`.
    /// If `msg != msg_for_input`, an invalid proof will be produced.
    fn zk_sig(msg: u64, msg_for_input: u64) -> DeferredZkpVerification {
        let key = ZkKey::from(BigUint::from(1u8));
        let signature = ZkKey::multi_sign(std::slice::from_ref(&key), &Fr::from(msg)).unwrap();
        let inputs =
            public_inputs_from_pks(Fr::from(msg_for_input).into(), &[key.to_public_key()]).unwrap();

        DeferredZkpVerification::ZkSig(*signature.as_proof(), inputs)
    }

    fn valid_leader_claim(voucher: u64) -> DeferredZkpVerification {
        leader_claim(voucher, voucher)
    }

    fn invalid_leader_claim(voucher: u64) -> DeferredZkpVerification {
        leader_claim(voucher + 1, voucher)
    }

    /// Proves a claim over the voucher of `secret`, but pairs the proof
    /// with the nullifier the verifier checks it against: that of
    /// `secret_for_input`.
    /// If `secret != secret_for_input`, an invalid proof will be produced.
    fn leader_claim(secret: u64, secret_for_input: u64) -> DeferredZkpVerification {
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

        DeferredZkpVerification::LeaderClaim(
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
