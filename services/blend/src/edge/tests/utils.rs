use core::{num::NonZeroU64, time::Duration};
use std::{
    fmt::{Debug, Display},
    sync::Arc,
};

use async_trait::async_trait;
use futures::StreamExt as _;
use lb_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::{
        epoch::UninitializedEpochEventStream,
        membership::Membership,
        message_blend::provers::{
            BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
            leader_and_pow::LeaderAndPowProofsGenerator,
        },
    },
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use overwatch::overwatch::{OverwatchHandle, commands::OverwatchCommand};
use rand::{RngCore, rngs::OsRng};
use rayon::ThreadPoolBuilder;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    core::settings::CoverTrafficSettings,
    edge::{
        backends::BlendBackend, handlers::Error, run, settings::RunningBlendConfig as BlendConfig,
        tests::test_blend_epoch_state,
    },
    epoch_info::PolInfoProvider,
    message::ServiceMessage,
    settings::{TimingSettings, max_data_message_delay_in_rounds},
    test_utils::{
        crypto::mock_blend_proof,
        dispatcher::{TestBroadcastingChannel, TestPayloadDispatcher},
        epoch::OncePolStreamProvider,
        membership::key,
    },
};

/// A round short enough that a test can wait out a whole delivery deadline
/// without being slow: the deadline is a count of rounds, and the tests here
/// have to sit through [`TEST_DELIVERY_DEADLINE`] of them.
pub const TEST_ROUND: Duration = Duration::from_millis(20);

/// `ß_c` for the tests.
const TEST_BLEND_LAYERS: NonZeroU64 = NonZeroU64::new(1).unwrap();
/// `∆max` for the tests: the rounds a blend node may hold a message for.
const TEST_MAX_BLEND_DELAY: NonZeroU64 = NonZeroU64::new(5).unwrap();

/// `T_D` for the tests, in rounds: the very derivation the service makes from
/// the two settings above, so the tests wait exactly as long as the code they
/// exercise and the two cannot drift apart. Pick a different deadline by
/// changing one of those, never by restating this.
pub const TEST_DELIVERY_DEADLINE: NonZeroU64 =
    max_data_message_delay_in_rounds(TEST_BLEND_LAYERS, TEST_MAX_BLEND_DELAY);

pub struct MockLeaderProofsGenerator;

#[async_trait]
impl LeaderAndPowProofsGenerator for MockLeaderProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self {
        Self
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }
}

/// What [`spawn_run`] hands back: the running service, the channels that drive
/// it, and both sides of its exit door.
pub struct RunningEdgeService {
    pub handle: JoinHandle<Result<(), Error>>,
    pub epochs: mpsc::Sender<Membership<NodeId>>,
    pub messages: mpsc::Sender<ServiceMessage<NodeId>>,
    pub blended_to: mpsc::Receiver<NodeId>,
    pub broadcasting_channel: TestBroadcastingChannel,
}

pub async fn spawn_run(
    local_node: NodeId,
    minimal_network_size: u64,
    initial_membership: Option<Membership<NodeId>>,
) -> RunningEdgeService {
    spawn_run_with_pol::<OncePolStreamProvider>(
        local_node,
        minimal_network_size,
        initial_membership,
        true,
    )
    .await
}

/// [`spawn_run`] for a node whose operator has turned the direct broadcast off.
pub async fn spawn_run_without_direct_broadcast(
    local_node: NodeId,
    minimal_network_size: u64,
    initial_membership: Option<Membership<NodeId>>,
) -> RunningEdgeService {
    spawn_run_with_pol::<OncePolStreamProvider>(
        local_node,
        minimal_network_size,
        initial_membership,
        false,
    )
    .await
}

/// [`spawn_run`], with the source of this epoch's secret `PoL` info left to the
/// caller — a test that needs to hold it back picks
/// [`GatedPolStreamProvider`](crate::test_utils::epoch::GatedPolStreamProvider).
pub async fn spawn_run_with_pol<PolProvider>(
    local_node: NodeId,
    minimal_network_size: u64,
    initial_membership: Option<Membership<NodeId>>,
    blend_failure_fallback: bool,
) -> RunningEdgeService
where
    PolProvider: PolInfoProvider<usize, Stream: Unpin + Send> + Send + 'static,
{
    let (epoch_sender, epoch_receiver) = mpsc::channel(1);
    let (msg_sender, msg_receiver) = mpsc::channel(1);
    let (node_id_sender, node_id_receiver) = mpsc::channel(1);

    if let Some(initial_membership) = initial_membership {
        epoch_sender
            .send(initial_membership)
            .await
            .expect("channel opened");
    }

    let epoch_stream = ReceiverStream::new(epoch_receiver)
        .map(|membership| test_blend_epoch_state(0.into(), membership));

    let mut settings = settings(local_node, minimal_network_size, node_id_sender);
    settings.blend_failure_fallback = blend_failure_fallback;
    let (payload_dispatcher, broadcasting_channel) = TestPayloadDispatcher::new();
    let join_handle = tokio::spawn(async move {
        Box::pin(run::<
            TestBackend,
            _,
            MockLeaderProofsGenerator,
            TestPayloadDispatcher,
            PolProvider,
            _,
        >(
            UninitializedEpochEventStream::new(epoch_stream, Duration::ZERO),
            ReceiverStream::new(msg_receiver),
            local_node,
            settings,
            payload_dispatcher,
            &overwatch_handle(),
            || {},
        ))
        .await
    });

    RunningEdgeService {
        handle: join_handle,
        epochs: epoch_sender,
        messages: msg_sender,
        blended_to: node_id_receiver,
        broadcasting_channel,
    }
}

pub fn settings(
    local_id: NodeId,
    minimum_network_size: u64,
    msg_sender: NodeIdSender,
) -> BlendConfig<NodeIdSender> {
    BlendConfig {
        blend_failure_fallback: true,
        time: TimingSettings {
            rounds_per_epoch: NonZeroU64::new(1).unwrap(),
            round_duration: TEST_ROUND,
            rounds_per_observation_window: NonZeroU64::new(1).unwrap(),
            epoch_transition_period: Duration::from_secs(1),
        },
        non_ephemeral_signing_key: key(local_id).0,
        num_blend_layers: TEST_BLEND_LAYERS,
        max_blend_delay_in_rounds: TEST_MAX_BLEND_DELAY,
        backend: msg_sender,
        minimum_network_size: NonZeroU64::new(minimum_network_size).unwrap(),
        cover: CoverTrafficSettings::default(),
        data_replication_factor: 0,
        pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
    }
}

pub type NodeIdSender = mpsc::Sender<NodeId>;

pub struct TestBackend {
    membership: Membership<NodeId>,
    sender: NodeIdSender,
}

#[async_trait::async_trait]
impl<RuntimeServiceId> BlendBackend<NodeId, RuntimeServiceId> for TestBackend
where
    NodeId: Clone,
    RuntimeServiceId: Debug + Sync + Display,
{
    type Settings = NodeIdSender;

    fn new<Rng>(
        settings: Self::Settings,
        _: OverwatchHandle<RuntimeServiceId>,
        membership: Membership<NodeId>,
        _: Rng,
        _: UnsecuredEd25519Key,
    ) -> Self
    where
        Rng: RngCore + Send + 'static,
    {
        Self {
            membership,
            sender: settings,
        }
    }

    fn shutdown(self) {}

    async fn send(&self, _: EncapsulatedMessageWithVerifiedPublicHeader) {
        let node_id = self
            .membership
            .choose_remote_nodes(&mut OsRng, 1)
            .next()
            .expect("Membership should not be empty")
            .id;
        self.sender.send(node_id).await.unwrap();
    }
}

pub fn overwatch_handle() -> OverwatchHandle<usize> {
    let (sender, _) = mpsc::channel::<OverwatchCommand<usize>>(1);
    OverwatchHandle::new(tokio::runtime::Handle::current(), sender)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct NodeId(pub u8);

impl From<NodeId> for [u8; 32] {
    fn from(id: NodeId) -> Self {
        [id.0; 32]
    }
}
