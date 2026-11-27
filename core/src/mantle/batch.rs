use lb_zktransfer::{ZkTransferProof, ZkTransferVerifierInput};

/// A proof that can be handed off to a [`DeferredProofs`] batch.
pub trait DeferrableProof {
    fn defer_into(self, batch: &mut DeferredProofs);
}

/// A deferred proof verification while processing an operation
#[derive(Debug)]
pub struct DeferredProof<Proof, Inputs> {
    proof: Proof,
    inputs: Inputs,
}

impl<Proof, Inputs> DeferredProof<Proof, Inputs> {
    #[must_use]
    pub const fn new(proof: Proof, inputs: Inputs) -> Self {
        Self { proof, inputs }
    }

    pub const fn proof(&self) -> &Proof {
        &self.proof
    }

    pub const fn inputs(&self) -> &Inputs {
        &self.inputs
    }

    #[must_use]
    pub fn into_parts(self) -> (Proof, Inputs) {
        (self.proof, self.inputs)
    }
}

pub type DeferredZkTransfer = DeferredProof<ZkTransferProof, ZkTransferVerifierInput>;

impl DeferrableProof for DeferredZkTransfer {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.zk_transfers.push(self);
    }
}

// The proofs of the steps of an inscription.
#[derive(Debug, Default)]
pub struct InscriptionDeferredProofs {
    // The ZkTransfers of the steps, in order.
    pub zk_transfers: Vec<DeferredZkTransfer>,
}

impl DeferrableProof for InscriptionDeferredProofs {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.zk_transfers.extend(self.zk_transfers);
    }
}

/// A group of deferred proof verifications while applying a block
#[derive(Default)]
#[must_use]
pub struct DeferredProofs {
    zk_transfers: Vec<DeferredZkTransfer>,
}

impl DeferredProofs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend(&mut self, other: Self) {
        self.zk_transfers.extend(other.zk_transfers);
    }

    pub fn verify(self) -> Result<(), Error> {
        Self::verify_zk_transfers(self.zk_transfers)
    }

    fn verify_zk_transfers(proofs: Vec<DeferredZkTransfer>) -> Result<(), Error> {
        if proofs.is_empty() {
            return Ok(());
        }

        let proof_parts = proofs
            .into_iter()
            .map(DeferredZkTransfer::into_parts)
            .collect::<Vec<_>>();
        match lb_zktransfer::batch_verify(proof_parts.as_slice()) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::InvalidZkTransfers),
            Err(e) => Err(Error::MalformedZkTransfer(format!("{e:?}"))),
        }
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn zk_transfers(&self) -> &[DeferredZkTransfer] {
        &self.zk_transfers
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
    #[error("deferred ZkTransfer proofs are invalid")]
    InvalidZkTransfers,
    #[error("deferred ZkTransfer proof is malformed: {0}")]
    MalformedZkTransfer(String),
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::{DeferrableProof, DeferredProofs, Error};

    pub fn batch_verify(proof: impl DeferrableProof) -> Result<(), Error> {
        DeferredProofs::from_iter([proof]).verify()
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{Field as _, Fr};

    use super::*;

    #[test]
    fn verify_accepts_empty_batch() {
        DeferredProofs::new().verify().expect("must succeed");
    }

    #[test]
    fn verify_accepts_batch_of_valid_zk_transfers() {
        [zk_transfer(Fr::ONE), zk_transfer(Fr::from(2u64))]
            .into_iter()
            .collect::<DeferredProofs>()
            .verify()
            .expect("must succeed");
    }

    #[test]
    fn verify_rejects_batch_containing_invalid_zk_transfer() {
        let err = [
            zk_transfer(Fr::ONE),
            zk_transfer_checked_against_another_msg(),
        ]
        .into_iter()
        .collect::<DeferredProofs>()
        .verify()
        .unwrap_err();
        assert!(matches!(err, Error::InvalidZkTransfers));
    }

    fn zk_transfer(msg: Fr) -> DeferredZkTransfer {
        let mut data = lb_zktransfer::test_utils::sample_witness_data();
        data.chain.msg = msg;
        let (proof, inputs) = lb_zktransfer::prove(data.into()).expect("the sample witness proves");
        DeferredZkTransfer::new(proof, inputs)
    }

    fn zk_transfer_checked_against_another_msg() -> DeferredZkTransfer {
        let (proof, inputs) = zk_transfer(Fr::ONE).into_parts();
        DeferredZkTransfer::new(
            proof,
            ZkTransferVerifierInput {
                msg: Fr::from(2u64).into(),
                ..inputs
            },
        )
    }
}
