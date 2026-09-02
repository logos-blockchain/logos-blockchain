use core::{
    cell::{Cell, RefCell},
    convert::Infallible,
};

use async_trait::async_trait;
use lb_blend::{
    message::{
        crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
        encap::ProofsVerifier,
    },
    proofs::{
        quota::{KeyIndex, ProofOfQuota, VerifiedProofOfQuota},
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    },
    scheduling::message_blend::provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
    },
};
use lb_chain_service::Epoch;
use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use tokio::sync::watch;

thread_local! {
    /// Records the core key index each [`MockCoreAndLeaderProofsGenerator`] was
    /// built to start from, so tests can assert that a recovered quota reaches
    /// the generator rather than it silently restarting at zero. Reliable
    /// because `#[tokio::test]` uses a single-threaded runtime, so the value is
    /// test-isolated.
    static STARTING_CORE_KEY_INDICES: RefCell<Vec<KeyIndex>> = const { RefCell::new(Vec::new()) };
}

/// Clears the record of generator starting key indices. Call before the code
/// under test to isolate the constructions of interest.
pub fn reset_starting_core_key_indices() {
    STARTING_CORE_KEY_INDICES.with(|indices| indices.borrow_mut().clear());
}

/// Returns the starting key index of every generator built since the last
/// reset, in construction order.
pub fn recorded_starting_core_key_indices() -> Vec<KeyIndex> {
    STARTING_CORE_KEY_INDICES.with(|indices| indices.borrow().clone())
}

pub struct MockCoreAndLeaderProofsGenerator;

#[async_trait]
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for MockCoreAndLeaderProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        starting_key_index: KeyIndex,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        STARTING_CORE_KEY_INDICES.with(|indices| indices.borrow_mut().push(starting_key_index));
        Self
    }

    fn set_epoch_private(
        &mut self,
        _winning_pol_info_stream: WinningPolInfoStream,
        _target_epoch: Epoch,
    ) {
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }
}

#[derive(Debug, Clone)]
pub struct MockProofsVerifier;

impl ProofsVerifier for MockProofsVerifier {
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
    /// Static value used by the `StaticFetchVerifier` below to count after how many
    /// `Ok`s it should return `Err`s when verifying encapsulated message layers.
    ///
    /// This value refers to proof of selections, since when decapsulating a message, we already assume the `PoQ`
    /// in the public header was correct, so we use `PoSel` to control the number of `Ok`s before failing at the given level.
    static REMAINING_VALID_LAYERS: Cell<u64> = const { Cell::new(0) };
}

#[derive(Debug, Clone)]
pub struct StaticFetchVerifier;

impl StaticFetchVerifier {
    pub fn set_remaining_valid_poq_proofs(remaining_valid_proofs: u64) {
        REMAINING_VALID_LAYERS.with(|val| val.set(remaining_valid_proofs));
    }
}

impl ProofsVerifier for StaticFetchVerifier {
    type Error = ();

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
        REMAINING_VALID_LAYERS.with(|val| {
            let remaining = val.get();
            if remaining > 0 {
                val.set(remaining - 1);
                Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
                    proof,
                ))
            } else {
                Err(())
            }
        })
    }
}

pub fn mock_blend_proof() -> BlendLayerProof {
    BlendLayerProof {
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
        proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
        ephemeral_signing_key: UnsecuredEd25519Key::generate_with_chacha_rng(),
    }
}

/// A proofs generator whose `PoW` branch only yields once a test lets it.
///
/// Standing in for the puzzle search, which in production takes long enough
/// that awaiting it anywhere on the event loop's critical path would stall the
/// service. Core and leadership proofs stay immediate, as they are in
/// production, so a test can tell the two apart.
pub struct GatedPowProofsGenerator;

thread_local! {
    static POW_GATE: RefCell<Option<watch::Receiver<bool>>> = const { RefCell::new(None) };
}

/// Holds the `PoW` branch shut until [`Self::release`] is called.
///
/// Level-triggered on purpose: the event loop re-creates the branch future on
/// every iteration, so an edge-triggered gate would lose the release whenever
/// it happened to fire between two of them.
pub struct PowGate(watch::Sender<bool>);

impl PowGate {
    /// Sets up a closed gate for generators created on this thread.
    #[must_use]
    pub fn setup() -> Self {
        let (sender, receiver) = watch::channel(false);
        POW_GATE.with_borrow_mut(|gate| *gate = Some(receiver));
        Self(sender)
    }

    /// Lets `PoW` proof requests through, now and from now on.
    pub fn release(&self) {
        self.0.send_replace(true);
    }
}

#[async_trait]
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for GatedPowProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        _starting_key_index: KeyIndex,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self
    }

    fn set_epoch_private(
        &mut self,
        _winning_pol_info_stream: WinningPolInfoStream,
        _target_epoch: Epoch,
    ) {
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        let mut gate = POW_GATE.with_borrow(Clone::clone)?;
        gate.wait_for(|open| *open)
            .await
            .expect("the gate should outlive the generator");
        Some(mock_blend_proof())
    }
}

/// A generator whose leadership branch, like the real one, has nothing to give
/// until this epoch's secret `PoL` info arrives.
///
/// `RealCoreAndLeaderProofsGenerator` holds its leader generator behind an
/// `Option` that `set_epoch_private` fills, and returns `None` until then — so
/// a proposal encapsulated before that point fails outright rather than
/// waiting.
pub struct PolAwareProofsGenerator {
    leadership_available: bool,
}

#[async_trait]
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for PolAwareProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        _starting_key_index: KeyIndex,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self {
            leadership_available: false,
        }
    }

    fn set_epoch_private(&mut self, _: WinningPolInfoStream, _: Epoch) {
        self.leadership_available = true;
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        self.leadership_available.then(mock_blend_proof)
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }
}
