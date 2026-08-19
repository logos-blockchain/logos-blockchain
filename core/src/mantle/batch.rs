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
