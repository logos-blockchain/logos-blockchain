use core::{cell::Cell, convert::Infallible};

use async_trait::async_trait;
use futures::future::ready;
use lb_blend_message::{
    crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
    encap::ProofsVerifier,
};
use lb_blend_proofs::{
    quota::{self, KeyIndex, ProofOfQuota, VerifiedProofOfQuota, inputs::prove::PublicInputs},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_core::crypto::ZkHash;
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};

use crate::{
    CoreProofOfQuotaGenerator,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
    },
};

pub struct MockCorePoQGenerator;

impl CoreProofOfQuotaGenerator for MockCorePoQGenerator {
    fn generate_poq(
        &self,
        _public_inputs: &PublicInputs,
        _key_index: KeyIndex,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
    {
        use lb_groth16::AdditiveGroup as _;

        ready(Ok((
            VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            ZkHash::ZERO,
        )))
    }
}

pub struct TestEpochChangeCoreAndLeaderProofsGenerator(pub Option<WinningPolInfoStream>);

#[async_trait]
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for TestEpochChangeCoreAndLeaderProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        _starting_key_index: KeyIndex,
        _proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(None)
    }

    fn set_epoch_private(
        &mut self,
        winning_pol_info_stream: WinningPolInfoStream,
        _target_epoch: Epoch,
    ) {
        self.0 = Some(winning_pol_info_stream);
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }
}

pub struct TestEpochChangeProofsVerifier;

#[async_trait]
impl ProofsVerifier for TestEpochChangeProofsVerifier {
    type Error = Infallible;

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
            proof,
        ))
    }
}

thread_local! {
    /// How many more core proofs [`RationedCoreProofsGenerator`] will hand out.
    static CORE_PROOFS_AVAILABLE: Cell<usize> = const { Cell::new(0) };
    /// Whether running out means "no more this epoch" or "not yet".
    static CORE_BRANCH_EXHAUSTED: Cell<bool> = const { Cell::new(false) };
}

/// Lets the next `count` core proof requests succeed. Once they are used up the
/// branch either reports itself exhausted or blocks, depending on
/// [`exhaust_core_branch`] — which is the difference between a draw that has to
/// settle for fewer layers and one that a caller can abandon part-way.
///
/// Reliable because `#[tokio::test]` runs on a current-thread runtime.
pub fn ration_core_proofs(count: usize) {
    CORE_PROOFS_AVAILABLE.with(|available| available.set(count));
}

/// Makes a rationed-out core branch report `None` rather than block.
pub fn exhaust_core_branch(exhausted: bool) {
    CORE_BRANCH_EXHAUSTED.with(|flag| flag.set(exhausted));
}

/// A generator whose core branch runs out on demand, so a test can stop a draw
/// part-way through a message and then let it finish.
pub struct RationedCoreProofsGenerator;

#[async_trait]
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for RationedCoreProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        _starting_key_index: KeyIndex,
        _proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self
    }

    fn set_epoch_private(&mut self, _: WinningPolInfoStream, _: Epoch) {}

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        if CORE_PROOFS_AVAILABLE.with(Cell::get) == 0 && !CORE_BRANCH_EXHAUSTED.with(Cell::get) {
            // Not exhausted, just nothing right now: block, so a caller can be
            // abandoned mid-draw.
            core::future::pending::<()>().await;
        }
        CORE_PROOFS_AVAILABLE.with(|available| {
            let left = available.get();
            available.set(left.checked_sub(1)?);
            Some(BlendLayerProof {
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
                ephemeral_signing_key: UnsecuredEd25519Key::generate_with_chacha_rng(),
            })
        })
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }
}
