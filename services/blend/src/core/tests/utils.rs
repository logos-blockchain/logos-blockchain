use core::cell::RefCell;
use std::{num::NonZeroU64, pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, stream, stream::BoxStream};
use lb_blend::{
    message::{
        crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
        encap::{ProofsVerifier, validated::EncapsulatedMessageWithVerifiedPublicHeader},
        reward,
    },
    proofs::{
        quota::{
            ProofOfQuota, VerifiedProofOfQuota,
            inputs::prove::{
                private::ProofOfLeadershipQuotaInputs,
                public::{CoreInputs, LeaderInputs, PowInputs},
            },
        },
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    },
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::EpochCryptographicProcessorSettings,
            provers::{
                BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
                core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
            },
        },
        message_scheduler::{self, epoch_info::EpochInfo as SchedulerEpochInfo},
    },
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_groth16::{AdditiveGroup as _, Fr, fr_from_bytes_unchecked, fr_to_bytes};
use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use lb_network_service::{NetworkService, backends::NetworkBackend};
use lb_poq::{CorePathAndSelectors, KeyIndex};
use lb_sdp_service::SdpMessage;
use overwatch::{
    overwatch::{OverwatchHandle, commands::OverwatchCommand},
    services::{ServiceData, relay::OutboundRelay, state::StateUpdater},
};
use rand::SeedableRng as _;
use rand_chacha::ChaCha20Rng;
use rayon::ThreadPoolBuilder;
use tokio::sync::{
    broadcast::{self},
    mpsc, watch,
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use crate::{
    core::{
        backends::{BackendEpochInfo, BlendBackend},
        dispatcher::PayloadDispatcher,
        kms::KmsPoQAdapter,
        processor::CoreCryptographicProcessor,
        settings::{
            CoverTrafficSettings, MessageDelayerSettings, RunningBlendConfig as BlendConfig,
            SchedulerSettings, ZkSettings,
        },
        state::RecoveryServiceState,
        tests::RuntimeServiceId,
    },
    delivery::DeliveryLogic,
    epoch::CoreEpochPublicInfo,
    message::{BlendPayload, NetworkInfo},
    settings::TimingSettings,
    test_utils,
    test_utils::parked::{TestChainNetworkService, TestMempoolService},
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
pub fn settings<BackendSettings>(
    local_private_key: UnsecuredEd25519Key,
    minimum_network_size: NonZeroU64,
    backend_settings: BackendSettings,
    data_replication_factor: u64,
) -> BlendConfig<BackendSettings> {
    BlendConfig {
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
        data_replication_factor,
        activity_threshold_sensitivity: 1,
        pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
        blend_failure_fallback: true,
    }
}

/// The seed every test's release delayer is built from.
const RELEASE_DELAY_SEED: u64 = 1;

/// A release delayer that draws the same delays on every run.
///
/// `initialize` takes this rather than drawing from entropy so that how many
/// rounds a message waits is a fixed property of a test rather than a fresh
/// draw each time. Note what this does *not* buy: the delay is in whole rounds
/// (`release_delayer` picks from `[1, max]`, never zero), so it fixes which
/// round a message goes out on, not how that round falls against events driven
/// from elsewhere — an epoch rotation arriving over a channel, say. A test that
/// depends on such an ordering needs more than this.
pub fn seeded_release_delay_rng() -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(RELEASE_DELAY_SEED)
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
impl<NodeId, Rng, ProofsVerifier> BlendBackend<NodeId, Rng, ProofsVerifier, RuntimeServiceId>
    for TestBlendBackend
where
    NodeId: Send + 'static,
    ProofsVerifier: Send + 'static,
{
    type Settings = ();

    fn new(
        _service_config: BlendConfig<Self::Settings>,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        _current_epoch_info: BackendEpochInfo<NodeId, ProofsVerifier>,
        _rng: Rng,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(CHANNEL_SIZE);
        Self { event_sender }
    }

    fn shutdown(self) {}
    async fn publish(
        &self,
        _msg: EncapsulatedMessageWithVerifiedPublicHeader,
        intended_epoch: Epoch,
    ) {
        note_outgoing_message();
        note_published_epoch(intended_epoch);
    }

    async fn rotate_epoch(&mut self, new_epoch_info: BackendEpochInfo<NodeId, ProofsVerifier>) {
        // Notify tests that the backend rotated to a new epoch, carrying the new
        // epoch and membership size so tests can assert the new membership was
        // propagated to the backend.
        let BackendEpochInfo {
            membership, epoch, ..
        } = new_epoch_info;
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
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedPublicHeader, Epoch)> + Send>>
    {
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

thread_local! {
    /// Installed by [`record_outgoing_messages`] for the duration of a test.
    static OUTGOING_MESSAGES: RefCell<Option<mpsc::UnboundedSender<()>>> =
        const { RefCell::new(None) };

    /// Installed by [`published_epochs_recorder`] for the duration of a test.
    static PUBLISHED_EPOCHS: RefCell<Option<mpsc::UnboundedSender<Epoch>>> =
        const { RefCell::new(None) };
}

/// Starts recording every message the service sends onwards, whether it goes
/// to a Blend peer through the backend or to a local service through the
/// dispatcher.
pub fn outgoing_messages_recorder() -> mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    OUTGOING_MESSAGES.with_borrow_mut(|recorder| *recorder = Some(sender));
    receiver
}

fn note_outgoing_message() {
    OUTGOING_MESSAGES.with_borrow(|recorder| {
        if let Some(sender) = recorder.as_ref() {
            let _ = sender.send(());
        }
    });
}

/// Records the epoch each message is published under, which is what tells a
/// transitioning epoch's release apart from the current one's. Separate from
/// [`outgoing_messages_recorder`], which also counts payloads handed to the
/// local dispatcher and so has no epoch to report.
pub fn published_epochs_recorder() -> mpsc::UnboundedReceiver<Epoch> {
    let (sender, receiver) = mpsc::unbounded_channel();
    PUBLISHED_EPOCHS.with_borrow_mut(|recorder| *recorder = Some(sender));
    receiver
}

fn note_published_epoch(intended_epoch: Epoch) {
    PUBLISHED_EPOCHS.with_borrow(|recorder| {
        if let Some(sender) = recorder.as_ref() {
            let _ = sender.send(intended_epoch);
        }
    });
}

pub struct TestPayloadDispatcher;

#[async_trait]
impl<RuntimeServiceId> PayloadDispatcher<RuntimeServiceId> for TestPayloadDispatcher
where
    RuntimeServiceId: Send + 'static,
{
    type Backend = TestNetworkBackend;
    type ChainNetworkService = TestChainNetworkService<RuntimeServiceId>;
    type MempoolService = TestMempoolService<RuntimeServiceId>;
    type Settings = ();

    fn new(
        _network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        _mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        _chain_network_relay: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
        _settings: Self::Settings,
    ) -> Self {
        Self
    }

    async fn dispatch(&self, _payload: BlendPayload) {
        note_outgoing_message();
    }

    async fn observe_broadcasts(&self) -> BoxStream<'static, BlendPayload> {
        stream::empty().boxed()
    }
}

/// A delivery tracker for a test that is not about the direct broadcast: it
/// watches a broadcasting channel nothing ever appears on, so nothing this node
/// releases is ever seen delivered.
///
/// That is deliberately the pessimistic case, and it is still quiet: the
/// deadline is the one [`settings`] implies, of rounds a second long, and no
/// test here runs long enough to reach it.
#[must_use]
pub fn no_deliveries_to_watch(settings: &BlendConfig<()>) -> DeliveryLogic {
    DeliveryLogic::watching(
        settings.max_data_message_delay_in_rounds(),
        settings.time.round_duration,
        stream::empty().boxed(),
    )
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
pub fn dummy_overwatch_resources<BackendSettings, NetworkSettings, RuntimeServiceId>() -> (
    OverwatchHandle<RuntimeServiceId>,
    mpsc::Receiver<OverwatchCommand<RuntimeServiceId>>,
    StateUpdater<Option<RecoveryServiceState<BackendSettings, NetworkSettings>>>,
    watch::Receiver<Option<RecoveryServiceState<BackendSettings, NetworkSettings>>>,
) {
    let (cmd_sender, cmd_receiver) = mpsc::channel(CHANNEL_SIZE);
    let handle =
        OverwatchHandle::<RuntimeServiceId>::new(tokio::runtime::Handle::current(), cmd_sender);
    let (state_sender, state_receiver) = watch::channel(None);
    let state_updater = StateUpdater::<
        Option<RecoveryServiceState<BackendSettings, NetworkSettings>>,
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
            pow: PowInputs::disabled(),
        },
        core_poq_generator,
        epoch_info.epoch,
    )
    .expect("crypto processor must be created successfully")
}

/// The [`BackendEpochInfo`] the service hands to the backend for an epoch,
/// including the `PoQ` verifier the backend uses to check received messages.
pub fn backend_epoch_info(
    public_info: &CoreEpochPublicInfo<NodeId>,
) -> BackendEpochInfo<NodeId, MockProofsVerifier> {
    BackendEpochInfo {
        membership: public_info.membership.clone(),
        epoch: public_info.epoch,
        proofs_verifier: MockProofsVerifier::new(PoQVerificationInputsMinusSigningKey {
            core: public_info.poq_core_public_inputs,
            leader: public_info.poq_leadership_public_inputs,
            pow: PowInputs::disabled(),
        }),
    }
}

pub fn new_epoch_info<BackendSettings>(
    epoch: Epoch,
    membership: Membership<NodeId>,
    settings: &BlendConfig<BackendSettings>,
) -> CoreEpochPublicInfo<NodeId> {
    let core_quota = settings.epoch_core_quota(membership.size());
    CoreEpochPublicInfo {
        poq_pow_public_inputs: PowInputs::disabled(),
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
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for MockCoreAndLeaderProofsGenerator
{
    fn new(
        settings: ProofsGeneratorSettings,
        _starting_key_index: KeyIndex,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(settings.public_inputs.leader.pol_epoch_nonce)
    }

    fn set_epoch_private(&mut self, _: WinningPolInfoStream, target_epoch: Epoch) {
        SET_EPOCH_PRIVATE_CALLS.with(|calls| calls.borrow_mut().push(target_epoch));
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(epoch_based_dummy_proofs(self.0))
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(epoch_based_dummy_proofs(self.0))
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
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
        ephemeral_signing_key: UnsecuredEd25519Key::generate_with_chacha_rng(),
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
