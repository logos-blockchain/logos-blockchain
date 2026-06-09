use core::cell::RefCell;
use std::{num::NonZeroU64, pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::Stream;
use lb_blend::{
    message::{
        crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
        encap::{
            ProofsVerifier,
            validated::{
                EncapsulatedMessageWithVerifiedPublicHeader,
                EncapsulatedMessageWithVerifiedSignature,
            },
        },
        reward,
    },
    proofs::{
        quota::{
            ProofOfQuota, VerifiedProofOfQuota,
            inputs::prove::{
                private::ProofOfLeadershipQuotaInputs,
                public::{CoreInputs, LeaderInputs},
            },
        },
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    },
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::EpochCryptographicProcessorSettings,
            provers::{
                BlendLayerProof, ProofsGeneratorSettings,
                core_and_leader::CoreAndLeaderProofsGenerator,
            },
        },
        message_scheduler::{self, epoch_info::EpochInfo as SchedulerEpochInfo},
    },
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_groth16::{Field as _, Fr, fr_from_bytes_unchecked, fr_to_bytes};
use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use lb_network_service::{NetworkService, backends::NetworkBackend};
use lb_poq::CorePathAndSelectors;
use lb_sdp_service::SdpMessage;
use overwatch::{
    overwatch::{OverwatchHandle, commands::OverwatchCommand},
    services::{ServiceData, relay::OutboundRelay, state::StateUpdater},
};
use tempfile::NamedTempFile;
use tokio::sync::{
    broadcast::{self},
    mpsc, watch,
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use crate::{
    core::{
        backends::{BackendEpochInfo, BlendBackend},
        kms::KmsPoQAdapter,
        network::NetworkAdapter,
        processor::CoreCryptographicProcessor,
        settings::{
            CoverTrafficSettings, MessageDelayerSettings, RunningBlendConfig as BlendConfig,
            SchedulerSettings, ZkSettings,
        },
        state::RecoveryServiceState,
        tests::RuntimeServiceId,
    },
    epoch::CoreEpochPublicInfo,
    message::NetworkInfo,
    settings::TimingSettings,
    test_utils,
};

pub type NodeId = [u8; 32];

/// Creates a membership with the given size and returns it along with the
/// private key of the local node.
pub fn new_membership(size: u8) -> (Membership<NodeId>, UnsecuredEd25519Key) {
    let ids = (0..size).map(|i| [i; 32]).collect::<Vec<_>>();
    let local_id = *ids.first().unwrap();
    (
        test_utils::membership::membership(&ids, local_id),
        test_utils::membership::key(local_id).0,
    )
}

/// Creates a [`BlendConfig`] with the given parameters and reasonable defaults
/// for the rest.
///
/// Also returns a [`NamedTempFile`] used for service recovery
/// that must not be dropped, as doing so will delete the underlying temp file.
pub fn settings<BackendSettings>(
    local_private_key: UnsecuredEd25519Key,
    minimum_network_size: NonZeroU64,
    backend_settings: BackendSettings,
    data_replication_factor: u64,
) -> (BlendConfig<BackendSettings>, NamedTempFile) {
    let recovery_file = NamedTempFile::new().unwrap();
    let settings = BlendConfig {
        backend: backend_settings,
        scheduler: SchedulerSettings {
            cover: CoverTrafficSettings {
                message_frequency_per_round: 1.0.try_into().unwrap(),
            },
            delayer: MessageDelayerSettings {
                maximum_release_delay_in_rounds: 1.try_into().unwrap(),
            },
        },
        time: timing_settings(),
        zk: ZkSettings {
            secret_key_kms_id: "test-key".to_owned(),
        },
        non_ephemeral_signing_key: local_private_key,
        num_blend_layers: NonZeroU64::try_from(1).unwrap(),
        minimum_network_size,
        recovery_path: recovery_file.path().to_path_buf(),
        data_replication_factor,
        activity_threshold_sensitivity: 1,
    };
    (settings, recovery_file)
}

pub fn timing_settings() -> TimingSettings {
    TimingSettings {
        rounds_per_epoch: 10.try_into().unwrap(),
        round_duration: Duration::from_secs(1),
        rounds_per_observation_window: 5.try_into().unwrap(),
        epoch_transition_period: Duration::from_secs(1),
    }
}

pub fn scheduler_settings(
    timing_settings: &TimingSettings,
    num_blend_layers: NonZeroU64,
) -> message_scheduler::Settings {
    message_scheduler::Settings {
        maximum_release_delay_in_rounds: NonZeroU64::try_from(1).unwrap(),
        round_duration: timing_settings.round_duration,
        rounds_per_epoch: timing_settings.rounds_per_epoch,
        num_blend_layers,
    }
}

const CHANNEL_SIZE: usize = 10;

pub fn new_stream<Item>() -> (impl Stream<Item = Item> + Unpin, mpsc::Sender<Item>) {
    let (sender, receiver) = mpsc::channel(CHANNEL_SIZE);
    (ReceiverStream::new(receiver), sender)
}

pub struct TestBlendBackend {
    // To notify tests about events occurring within the backend.
    event_sender: broadcast::Sender<TestBlendBackendEvent>,
}

#[async_trait]
impl<NodeId, Rng> BlendBackend<NodeId, Rng, RuntimeServiceId> for TestBlendBackend
where
    NodeId: Send + 'static,
{
    type Settings = ();

    fn new(
        _service_config: BlendConfig<Self::Settings>,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        _current_epoch_info: BackendEpochInfo<NodeId>,
        _rng: Rng,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(CHANNEL_SIZE);
        Self { event_sender }
    }

    fn shutdown(self) {}
    async fn publish(
        &self,
        _msg: EncapsulatedMessageWithVerifiedPublicHeader,
        _intended_epoch: Epoch,
    ) {
    }
    async fn rotate_epoch(&mut self, new_epoch_info: BackendEpochInfo<NodeId>) {
        // Notify tests that the backend rotated to a new epoch, carrying the new
        // epoch and membership size so tests can assert the new membership was
        // propagated to the backend.
        let (membership, epoch) = new_epoch_info;
        // Ignore send errors: not all tests subscribe to backend events, and
        // `rotate_epoch` is also called right before a retirement (no subscriber).
        let _ = self.event_sender.send(TestBlendBackendEvent::EpochRotated {
            epoch,
            membership_size: membership.size(),
        });
    }

    async fn complete_epoch_transition(&mut self) {
        // Notify tests that the backend completed the epoch transition.
        self.event_sender
            .send(TestBlendBackendEvent::EpochTransitionCompleted)
            .unwrap();
    }

    fn listen_to_incoming_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedSignature, Epoch)> + Send>> {
        unimplemented!()
    }

    async fn network_info(&self) -> Option<NetworkInfo<NodeId>> {
        unimplemented!()
    }
}

impl TestBlendBackend {
    /// Subscribes to backend test events.
    pub fn subscribe_to_events(&self) -> broadcast::Receiver<TestBlendBackendEvent> {
        self.event_sender.subscribe()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestBlendBackendEvent {
    EpochTransitionCompleted,
    /// Emitted when the backend is rotated to a new epoch.
    EpochRotated {
        epoch: Epoch,
        membership_size: usize,
    },
}

/// Waits for the given event to be received on the provided channel.
/// All other events are ignored.
///
/// It panics if the channel is lagged or closed.
pub async fn wait_for_blend_backend_event(
    receiver: &mut broadcast::Receiver<TestBlendBackendEvent>,
    event: TestBlendBackendEvent,
) {
    loop {
        let received_event = receiver
            .recv()
            .await
            .expect("channel shouldn't be closed or lagged");
        if received_event == event {
            return;
        }
    }
}

pub struct TestNetworkAdapter;

#[async_trait]
impl<RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for TestNetworkAdapter {
    type Backend = TestNetworkBackend;
    type BroadcastSettings = ();

    fn new(
        _network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self
    }

    async fn broadcast(&self, _message: Vec<u8>, _broadcast_settings: Self::BroadcastSettings) {}
}

pub struct TestNetworkBackend {
    pubsub_sender: broadcast::Sender<()>,
    chainsync_sender: broadcast::Sender<()>,
}

#[async_trait]
impl<RuntimeServiceId> NetworkBackend<RuntimeServiceId> for TestNetworkBackend {
    type Settings = ();
    type Message = ();
    type PubSubEvent = ();
    type ChainSyncEvent = ();

    fn new(_config: Self::Settings, _overwatch_handle: OverwatchHandle<RuntimeServiceId>) -> Self {
        let (pubsub_sender, _) = broadcast::channel(CHANNEL_SIZE);
        let (chainsync_sender, _) = broadcast::channel(CHANNEL_SIZE);
        Self {
            pubsub_sender,
            chainsync_sender,
        }
    }

    async fn process(&self, _msg: Self::Message) {}

    async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent> {
        BroadcastStream::new(self.pubsub_sender.subscribe())
    }

    async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent> {
        BroadcastStream::new(self.chainsync_sender.subscribe())
    }
}

#[expect(clippy::type_complexity, reason = "a test utility")]
pub fn dummy_overwatch_resources<BackendSettings, BroadcastSettings, RuntimeServiceId>() -> (
    OverwatchHandle<RuntimeServiceId>,
    mpsc::Receiver<OverwatchCommand<RuntimeServiceId>>,
    StateUpdater<Option<RecoveryServiceState<BackendSettings, BroadcastSettings>>>,
    watch::Receiver<Option<RecoveryServiceState<BackendSettings, BroadcastSettings>>>,
) {
    let (cmd_sender, cmd_receiver) = mpsc::channel(CHANNEL_SIZE);
    let handle =
        OverwatchHandle::<RuntimeServiceId>::new(tokio::runtime::Handle::current(), cmd_sender);
    let (state_sender, state_receiver) = watch::channel(None);
    let state_updater = StateUpdater::<
        Option<RecoveryServiceState<BackendSettings, BroadcastSettings>>,
    >::new(Arc::new(state_sender));

    (handle, cmd_receiver, state_updater, state_receiver)
}

pub fn new_crypto_processor<CorePoQGenerator>(
    settings: EpochCryptographicProcessorSettings,
    epoch_info: &CoreEpochPublicInfo<NodeId>,
    core_poq_generator: CorePoQGenerator,
) -> CoreCryptographicProcessor<
    NodeId,
    CorePoQGenerator,
    MockCoreAndLeaderProofsGenerator,
    MockProofsVerifier,
> {
    let minimum_network_size = u64::try_from(epoch_info.membership.size())
        .expect("membership size must fit into u64")
        .try_into()
        .expect("minimum_network_size must be non-zero");
    CoreCryptographicProcessor::try_new_with_core_condition_check(
        epoch_info.membership.clone(),
        minimum_network_size,
        settings,
        PoQVerificationInputsMinusSigningKey {
            core: epoch_info.poq_core_public_inputs,
            leader: epoch_info.poq_leadership_public_inputs,
        },
        core_poq_generator,
        epoch_info.epoch,
    )
    .expect("crypto processor must be created successfully")
}

pub fn new_epoch_info<BackendSettings>(
    epoch: Epoch,
    membership: Membership<NodeId>,
    settings: &BlendConfig<BackendSettings>,
) -> CoreEpochPublicInfo<NodeId> {
    let core_quota = settings.epoch_core_quota(membership.size());
    CoreEpochPublicInfo {
        epoch,
        membership,
        poq_core_public_inputs: CoreInputs {
            zk_root: ZkHash::ZERO,
            quota: core_quota,
        },
        poq_leadership_public_inputs: LeaderInputs {
            pol_ledger_aged: ZkHash::ZERO,
            pol_epoch_nonce: fr_from_bytes_unchecked(&epoch.into_inner().to_le_bytes()),
            message_quota: settings.epoch_leadership_quota(),
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
        },
    }
}

/// Dummy secret `PoL` leadership inputs, for tests that exercise the
/// secret/public epoch-info coordination without needing valid proofs.
pub fn dummy_pol_private_inputs() -> ProofOfLeadershipQuotaInputs {
    ProofOfLeadershipQuotaInputs {
        slot: 1,
        note_value: 1,
        transaction_hash: ZkHash::ZERO,
        output_number: 1,
        aged_path_and_selectors: [(ZkHash::ZERO, false); _],
        secret_key: ZkHash::ZERO,
    }
}

pub fn scheduler_epoch_info(public_info: &CoreEpochPublicInfo<NodeId>) -> SchedulerEpochInfo {
    SchedulerEpochInfo {
        core_quota: public_info.poq_core_public_inputs.quota,
        epoch: public_info.epoch,
    }
}

pub fn reward_epoch_info(public_info: &CoreEpochPublicInfo<NodeId>) -> reward::EpochInfo {
    reward::EpochInfo::new(
        public_info.epoch,
        &public_info.poq_leadership_public_inputs.pol_epoch_nonce,
        public_info
            .membership
            .size()
            .try_into()
            .expect("num_core_nodes must fit into u64"),
        public_info.poq_core_public_inputs.quota,
        1,
    )
    .expect("epoch info must be created successfully")
}

thread_local! {
    /// Records the epochs for which [`MockCoreAndLeaderProofsGenerator::set_epoch_private`]
    /// was called, so tests can assert that the secret `PoL` info was applied to
    /// the expected generator. Reliable because `#[tokio::test]` uses a
    /// single-threaded runtime, so the value is test-isolated.
    static SET_EPOCH_PRIVATE_CALLS: RefCell<Vec<Epoch>> = const { RefCell::new(Vec::new()) };
}

/// Clears the record of `set_epoch_private` calls. Call before the code under
/// test to isolate the calls of interest.
pub fn reset_set_epoch_private_calls() {
    SET_EPOCH_PRIVATE_CALLS.with(|calls| calls.borrow_mut().clear());
}

/// Returns the epochs for which `set_epoch_private` has been called since the
/// last reset, in call order.
pub fn recorded_set_epoch_private_calls() -> Vec<Epoch> {
    SET_EPOCH_PRIVATE_CALLS.with(|calls| calls.borrow().clone())
}

pub struct MockCoreAndLeaderProofsGenerator(ZkHash);

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for MockCoreAndLeaderProofsGenerator
{
    fn new(
        settings: ProofsGeneratorSettings,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(settings.public_inputs.leader.pol_epoch_nonce)
    }

    fn set_epoch_private(&mut self, _: ProofOfLeadershipQuotaInputs, target_epoch: Epoch) {
        SET_EPOCH_PRIVATE_CALLS.with(|calls| calls.borrow_mut().push(target_epoch));
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(epoch_based_dummy_proofs(self.0))
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(epoch_based_dummy_proofs(self.0))
    }
}

#[derive(Debug, Clone)]
pub struct MockProofsVerifier(ZkHash);

impl ProofsVerifier for MockProofsVerifier {
    type Error = ();

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self(public_inputs.leader.pol_epoch_nonce)
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let expected_proof = epoch_based_dummy_proofs(self.0).proof_of_quota;
        if proof == expected_proof {
            Ok(expected_proof)
        } else {
            Err(())
        }
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        let expected_proof = epoch_based_dummy_proofs(self.0).proof_of_selection;
        if proof == expected_proof {
            Ok(expected_proof)
        } else {
            Err(())
        }
    }
}

fn epoch_based_dummy_proofs(epoch: ZkHash) -> BlendLayerProof {
    let epoch_bytes = fr_to_bytes(&epoch);
    BlendLayerProof {
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked({
            let mut bytes = [0u8; _];
            bytes[..epoch_bytes.len()].copy_from_slice(&epoch_bytes);
            bytes
        }),
        proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked({
            let mut bytes = [0u8; _];
            bytes[..epoch_bytes.len()].copy_from_slice(&epoch_bytes);
            bytes
        }),
        ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
    }
}

pub struct MockKmsAdapter;

impl<RuntimeServiceId> KmsPoQAdapter<RuntimeServiceId> for MockKmsAdapter {
    type CorePoQGenerator = ();
    // Required by the Blend core service.
    type KeyId = String;

    fn core_poq_generator(
        &self,
        _key_id: Self::KeyId,
        _core_path_and_selectors: Box<CorePathAndSelectors>,
    ) -> Self::CorePoQGenerator {
    }
}

pub fn sdp_relay() -> (OutboundRelay<SdpMessage>, mpsc::Receiver<SdpMessage>) {
    let (sender, receiver) = mpsc::channel(10);
    (OutboundRelay::new(sender), receiver)
}
