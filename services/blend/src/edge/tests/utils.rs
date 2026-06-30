use core::{num::NonZeroU64, time::Duration};
use std::fmt::{Debug, Display};

use async_trait::async_trait;
use futures::StreamExt as _;
use lb_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::{
        epoch::UninitializedEpochEventStream,
        membership::Membership,
        message_blend::provers::{
            BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
            leader::LeaderProofsGenerator,
        },
    },
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use overwatch::overwatch::{OverwatchHandle, commands::OverwatchCommand};
use rand::{RngCore, rngs::OsRng};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    core::settings::CoverTrafficSettings,
    edge::{
        backends::BlendBackend, handlers::Error, run, settings::RunningBlendConfig as BlendConfig,
        tests::test_blend_epoch_state,
    },
    settings::TimingSettings,
    test_utils::{crypto::mock_blend_proof, epoch::OncePolStreamProvider, membership::key},
};

pub struct MockLeaderProofsGenerator;

#[async_trait]
impl LeaderProofsGenerator for MockLeaderProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self {
        Self
    }

    async fn get_next_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }
}

pub async fn spawn_run(
    local_node: NodeId,
    minimal_network_size: u64,
    initial_membership: Option<Membership<NodeId>>,
) -> (
    JoinHandle<Result<(), Error>>,
    mpsc::Sender<Membership<NodeId>>,
    mpsc::Sender<Vec<u8>>,
    mpsc::Receiver<NodeId>,
) {
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

    let settings = settings(local_node, minimal_network_size, node_id_sender);
    let join_handle = tokio::spawn(async move {
        Box::pin(run::<
            TestBackend,
            _,
            MockLeaderProofsGenerator,
            OncePolStreamProvider,
            _,
        >(
            UninitializedEpochEventStream::new(epoch_stream, Duration::ZERO),
            ReceiverStream::new(msg_receiver),
            settings,
            &overwatch_handle(),
            || {},
        ))
        .await
    });

    (join_handle, epoch_sender, msg_sender, node_id_receiver)
}

pub fn settings(
    local_id: NodeId,
    minimum_network_size: u64,
    msg_sender: NodeIdSender,
) -> BlendConfig<NodeIdSender> {
    BlendConfig {
        time: TimingSettings {
            rounds_per_epoch: NonZeroU64::new(1).unwrap(),
            round_duration: Duration::from_secs(1),
            rounds_per_observation_window: NonZeroU64::new(1).unwrap(),
            epoch_transition_period: Duration::from_secs(1),
        },
        non_ephemeral_signing_key: key(local_id).0,
        num_blend_layers: NonZeroU64::new(1).unwrap(),
        backend: msg_sender,
        minimum_network_size: NonZeroU64::new(minimum_network_size).unwrap(),
        cover: CoverTrafficSettings::default(),
        data_replication_factor: 0,
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
