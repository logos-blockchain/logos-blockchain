use lb_groth16::{CompressedGroth16Proof, Fr};
use lb_program_spend::{ZkProgramSpendProof, ZkProgramSpendVerifierInput};
use lb_zktransfer::{ZkTransferProof, ZkTransferVerifierInput};

use crate::mantle::programs::{PROGRAM_PUBLIC_INPUTS, RegisteredProgram};

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
pub type DeferredZkProgramSpend = DeferredProof<ZkProgramSpendProof, ZkProgramSpendVerifierInput>;
pub type DeferredProgramProof = DeferredProof<CompressedGroth16Proof, ProgramProofInput>;

#[derive(Clone, Debug)]
pub struct ProgramProofInput {
    pub program: RegisteredProgram,
    pub public: [Fr; PROGRAM_PUBLIC_INPUTS],
}

impl DeferrableProof for DeferredZkTransfer {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.zk_transfers.push(self);
    }
}

impl DeferrableProof for DeferredZkProgramSpend {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.program_spends.push(self);
    }
}

impl DeferrableProof for DeferredProgramProof {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.program_proofs.push(self);
    }
}

// The proofs of the steps of an inscription.
#[derive(Debug, Default)]
pub struct InscriptionDeferredProofs {
    // The ZkTransfers of the user steps and of the deposits, in order.
    pub zk_transfers: Vec<DeferredZkTransfer>,
    pub program_spends: Vec<DeferredZkProgramSpend>,
    pub program_proofs: Vec<DeferredProgramProof>,
}

impl DeferrableProof for InscriptionDeferredProofs {
    fn defer_into(self, batch: &mut DeferredProofs) {
        batch.zk_transfers.extend(self.zk_transfers);
        batch.program_spends.extend(self.program_spends);
        batch.program_proofs.extend(self.program_proofs);
    }
}

/// A group of deferred proof verifications while applying a block
#[derive(Default)]
#[must_use]
pub struct DeferredProofs {
    zk_transfers: Vec<DeferredZkTransfer>,
    program_spends: Vec<DeferredZkProgramSpend>,
    program_proofs: Vec<DeferredProgramProof>,
}

impl DeferredProofs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend(&mut self, other: Self) {
        self.zk_transfers.extend(other.zk_transfers);
        self.program_spends.extend(other.program_spends);
        self.program_proofs.extend(other.program_proofs);
    }

    pub fn verify(self) -> Result<(), Error> {
        Self::verify_zk_transfers(self.zk_transfers)?;
        Self::verify_program_spends(self.program_spends)?;
        Self::verify_program_proofs(self.program_proofs)
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

    fn verify_program_spends(proofs: Vec<DeferredZkProgramSpend>) -> Result<(), Error> {
        if proofs.is_empty() {
            return Ok(());
        }

        let proof_parts = proofs
            .into_iter()
            .map(DeferredZkProgramSpend::into_parts)
            .collect::<Vec<_>>();
        match lb_program_spend::batch_verify(proof_parts.as_slice()) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::InvalidProgramSpends),
            Err(e) => Err(Error::MalformedProgramSpend(format!("{e:?}"))),
        }
    }

    // Each program proof is verified alone, it cannot share the batch of another
    // key.
    fn verify_program_proofs(proofs: Vec<DeferredProgramProof>) -> Result<(), Error> {
        for (proof, input) in proofs.into_iter().map(DeferredProgramProof::into_parts) {
            match input.program.verify(&proof, &input.public) {
                Ok(true) => {}
                Ok(false) => return Err(Error::InvalidProgramProof),
                Err(e) => return Err(Error::MalformedProgramProof(e.to_string())),
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn zk_transfers(&self) -> &[DeferredZkTransfer] {
        &self.zk_transfers
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn program_spends(&self) -> &[DeferredZkProgramSpend] {
        &self.program_spends
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn program_proofs(&self) -> &[DeferredProgramProof] {
        &self.program_proofs
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
    #[error("deferred ZkProgramSpend proofs are invalid")]
    InvalidProgramSpends,
    #[error("deferred ZkProgramSpend proof is malformed: {0}")]
    MalformedProgramSpend(String),
    #[error("a deferred program proof is invalid")]
    InvalidProgramProof,
    #[error("a deferred program proof is malformed: {0}")]
    MalformedProgramProof(String),
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
    use lb_groth16::{
        Field as _,
        test_utils::{three_input_prove, three_input_setup},
    };

    use super::*;
    use crate::mantle::programs::ProgramVerificationKey;

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

    #[test]
    fn verify_accepts_valid_program_spends_and_program_proofs() {
        let mut batch = DeferredProofs::new();
        program_spend(Fr::ONE).defer_into(&mut batch);
        program_spend(Fr::from(2u64)).defer_into(&mut batch);
        program_proof(1, 1).defer_into(&mut batch);
        batch.verify().expect("every proof is valid");
    }

    #[test]
    fn verify_rejects_a_program_spend_checked_against_another_paid_value() {
        let (proof, inputs) = program_spend(Fr::ONE).into_parts();
        let tampered = DeferredZkProgramSpend::new(
            proof,
            ZkProgramSpendVerifierInput {
                paid_value: Fr::ONE.into(),
                ..inputs
            },
        );
        assert!(matches!(
            std::iter::once(tampered)
                .collect::<DeferredProofs>()
                .verify(),
            Err(Error::InvalidProgramSpends)
        ));
    }

    #[test]
    fn verify_rejects_a_program_proof_under_another_key() {
        assert!(matches!(
            std::iter::once(program_proof(1, 2))
                .collect::<DeferredProofs>()
                .verify(),
            Err(Error::InvalidProgramProof)
        ));
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

    fn program_spend(msg: Fr) -> DeferredZkProgramSpend {
        let mut data = lb_program_spend::test_utils::sample_witness_data();
        data.chain.msg = msg;
        let (proof, inputs) =
            lb_program_spend::prove(data.into()).expect("the sample witness proves");
        DeferredZkProgramSpend::new(proof, inputs)
    }

    fn program_proof(key_seed: u64, verified_under: u64) -> DeferredProgramProof {
        let (_, proving_key) = three_input_setup(key_seed);
        let (key, _) = three_input_setup(verified_under);
        let program =
            RegisteredProgram::new(ProgramVerificationKey::try_from(&key).unwrap()).unwrap();
        let public = [Fr::from(1u64), Fr::from(2u64), Fr::from(3u64)];
        let proof =
            CompressedGroth16Proof::try_from(&three_input_prove(&proving_key, public, 4)).unwrap();
        DeferredProgramProof::new(proof, ProgramProofInput { program, public })
    }
}
