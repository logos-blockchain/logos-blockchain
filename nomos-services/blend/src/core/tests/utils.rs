use std::{num::NonZeroU64, pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::Stream;
use groth16::Field as _;
use key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use nomos_blend::{
    message::{
        crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
        encap::{
            ProofsVerifier, encapsulated::EncapsulatedMessage,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
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
            crypto::SessionCryptographicProcessorSettings,
            provers::{
                BlendLayerProof, ProofsGeneratorSettings,
                core_and_leader::CoreAndLeaderProofsGenerator,
            },
        },
        message_scheduler::{self, session_info::SessionInfo as SchedulerSessionInfo},
    },
};
use nomos_core::{crypto::ZkHash, sdp::SessionNumber};
use nomos_network::{NetworkService, backends::NetworkBackend};
use nomos_sdp::SdpMessage;
use overwatch::{
    overwatch::{OverwatchHandle, commands::OverwatchCommand},
    services::{ServiceData, relay::OutboundRelay, state::StateUpdater},
};
use poq::CorePathAndSelectors;
use tempfile::NamedTempFile;
use tokio::sync::{
    broadcast::{self},
    mpsc, watch,
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use crate::{
    core::{
        backends::{BlendBackend, EpochInfo, PublicInfo, SessionInfo},
        kms::KmsPoQAdapter,
        network::NetworkAdapter,
        processor::CoreCryptographicProcessor,
        settings::{
            BlendConfig, CoverTrafficSettings, MessageDelayerSettings, SchedulerSettings,
            ZkSettings,
        },
        state::RecoveryServiceState,
        tests::RuntimeServiceId,
    },
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
) -> (BlendConfig<BackendSettings>, NamedTempFile) {
    let recovery_file = NamedTempFile::new().unwrap();
    let settings = BlendConfig {
        backend: backend_settings,
        crypto: SessionCryptographicProcessorSettings {
            non_ephemeral_signing_key: local_private_key,
            num_blend_layers: NonZeroU64::try_from(1).unwrap(),
        },
        scheduler: SchedulerSettings {
            cover: CoverTrafficSettings {
                message_frequency_per_round: 1.0.try_into().unwrap(),
                intervals_for_safety_buffer: 0,
            },
            delayer: MessageDelayerSettings {
                maximum_release_delay_in_rounds: 1.try_into().unwrap(),
            },
        },
        time: timing_settings(),
        zk: ZkSettings {
            secret_key_kms_id: "test-key".to_owned(),
        },
        minimum_network_size,
        recovery_path: recovery_file.path().to_path_buf(),
    };
    (settings, recovery_file)
}

pub fn timing_settings() -> TimingSettings {
    TimingSettings {
        rounds_per_session: 10.try_into().unwrap(),
        rounds_per_interval: 10.try_into().unwrap(),
        round_duration: Duration::from_secs(1),
        rounds_per_observation_window: 5.try_into().unwrap(),
        rounds_per_session_transition_period: 2.try_into().unwrap(),
        epoch_transition_period_in_slots: 1.try_into().unwrap(),
    }
}

pub fn scheduler_settings(
    timing_settings: &TimingSettings,
    num_blend_layers: NonZeroU64,
) -> message_scheduler::Settings {
    message_scheduler::Settings {
        additional_safety_intervals: 0,
        expected_intervals_per_session: NonZeroU64::try_from(1).unwrap(),
        maximum_release_delay_in_rounds: NonZeroU64::try_from(1).unwrap(),
        round_duration: timing_settings.round_duration,
        rounds_per_interval: timing_settings.rounds_per_interval,
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
{
    type Settings = ();

    fn new(
        _service_config: BlendConfig<Self::Settings>,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        _current_public_info: PublicInfo<NodeId>,
        _rng: Rng,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(CHANNEL_SIZE);
        Self { event_sender }
    }

    fn shutdown(self) {}
    async fn publish(&self, _msg: EncapsulatedMessage) {}
    async fn rotate_session(&mut self, _new_session_info: SessionInfo<NodeId>) {}

    async fn complete_session_transition(&mut self) {
        // Notify tests that the backend completed the session transition.
        self.event_sender
            .send(TestBlendBackendEvent::SessionTransitionCompleted)
            .unwrap();
    }

    async fn rotate_epoch(&mut self, _new_epoch_public_info: EpochInfo) {}
    async fn complete_epoch_transition(&mut self) {}

    fn listen_to_incoming_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = EncapsulatedMessageWithVerifiedPublicHeader> + Send>> {
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
    SessionTransitionCompleted,
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
    settings: &SessionCryptographicProcessorSettings,
    public_info: &PublicInfo<NodeId>,
    core_poq_generator: CorePoQGenerator,
) -> CoreCryptographicProcessor<
    NodeId,
    CorePoQGenerator,
    MockCoreAndLeaderProofsGenerator,
    MockProofsVerifier,
> {
    let minimum_network_size = u64::try_from(public_info.session.membership.size())
        .expect("membership size must fit into u64")
        .try_into()
        .expect("minimum_network_size must be non-zero");
    CoreCryptographicProcessor::try_new_with_core_condition_check(
        public_info.session.membership.clone(),
        minimum_network_size,
        settings,
        PoQVerificationInputsMinusSigningKey {
            session: public_info.session.session_number,
            core: public_info.session.core_public_inputs,
            leader: public_info.epoch,
        },
        core_poq_generator,
    )
    .expect("crypto processor must be created successfully")
}

pub fn new_public_info<BackendSettings>(
    session: u64,
    membership: Membership<NodeId>,
    settings: &BlendConfig<BackendSettings>,
) -> PublicInfo<NodeId> {
    let core_quota = settings.session_quota(membership.size());
    PublicInfo {
        session: SessionInfo {
            session_number: session,
            membership,
            core_public_inputs: CoreInputs {
                zk_root: ZkHash::ZERO,
                quota: core_quota,
            },
        },
        epoch: LeaderInputs {
            pol_ledger_aged: ZkHash::ZERO,
            pol_epoch_nonce: ZkHash::ZERO,
            message_quota: settings.crypto.num_blend_layers.get(),
            total_stake: 10,
        },
    }
}

pub fn scheduler_session_info(public_info: &PublicInfo<NodeId>) -> SchedulerSessionInfo {
    SchedulerSessionInfo {
        core_quota: public_info.session.core_public_inputs.quota,
        session_number: u128::from(public_info.session.session_number).into(),
    }
}

pub fn reward_session_info(public_info: &PublicInfo<NodeId>) -> reward::SessionInfo {
    reward::SessionInfo::new(
        public_info.session.session_number,
        &public_info.epoch.pol_epoch_nonce,
        public_info
            .session
            .membership
            .size()
            .try_into()
            .expect("num_core_nodes must fit into u64"),
        public_info.session.core_public_inputs.quota,
    )
    .expect("session info must be created successfully")
}

pub struct MockCoreAndLeaderProofsGenerator(SessionNumber);

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for MockCoreAndLeaderProofsGenerator
{
    fn new(
        settings: ProofsGeneratorSettings,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(settings.public_inputs.session)
    }

    fn rotate_epoch(&mut self, _: LeaderInputs) {}
    fn set_epoch_private(&mut self, _: ProofOfLeadershipQuotaInputs) {}

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(session_based_dummy_proofs(self.0))
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(session_based_dummy_proofs(self.0))
    }
}

#[derive(Debug, Clone)]
pub struct MockProofsVerifier(SessionNumber);

impl ProofsVerifier for MockProofsVerifier {
    type Error = ();

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self(public_inputs.session)
    }

    fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

    fn complete_epoch_transition(&mut self) {}

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let expected_proof = session_based_dummy_proofs(self.0).proof_of_quota;
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
        let expected_proof = session_based_dummy_proofs(self.0).proof_of_selection;
        if proof == expected_proof {
            Ok(expected_proof)
        } else {
            Err(())
        }
    }
}

fn session_based_dummy_proofs(session: SessionNumber) -> BlendLayerProof {
    let session_bytes = session.to_le_bytes();
    BlendLayerProof {
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked({
            let mut bytes = [0u8; _];
            bytes[..session_bytes.len()].copy_from_slice(&session_bytes);
            bytes
        }),
        proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked({
            let mut bytes = [0u8; _];
            bytes[..session_bytes.len()].copy_from_slice(&session_bytes);
            bytes
        }),
        ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
    }
}

impl MockProofsVerifier {
    pub fn session_number(&self) -> SessionNumber {
        self.0
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
