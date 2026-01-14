use core::{num::NonZeroU64, time::Duration};
use std::{
    fmt::{Debug, Display},
    panic,
};

use async_trait::async_trait;
use futures::{StreamExt as _, future::ready, stream::once};
use nomos_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    proofs::quota::inputs::prove::{private::ProofOfLeadershipQuotaInputs, public::LeaderInputs},
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::SessionCryptographicProcessorSettings,
            provers::{BlendLayerProof, ProofsGeneratorSettings, leader::LeaderProofsGenerator},
        },
        session::UninitializedSessionEventStream,
        stream::UninitializedFirstReadyStream,
    },
};
use nomos_time::SlotTick;
use overwatch::overwatch::{OverwatchHandle, commands::OverwatchCommand};
use rand::{RngCore, rngs::OsRng};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    core::settings::CoverTrafficSettings,
    edge::{backends::BlendBackend, handlers::Error, run, settings::BlendConfig},
    epoch_info::EpochHandler,
    membership::MembershipInfo,
    settings::{FIRST_STREAM_ITEM_READY_TIMEOUT, TimingSettings},
    test_utils::{
        crypto::mock_blend_proof,
        epoch::{OncePolStreamProvider, TestChainService},
        membership::key,
    },
};

pub struct MockLeaderProofsGenerator;

#[async_trait]
impl LeaderProofsGenerator for MockLeaderProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        Self
    }

    fn rotate_epoch(
        &mut self,
        _new_epoch_public: LeaderInputs,
        _new_private_inputs: ProofOfLeadershipQuotaInputs,
    ) {
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        mock_blend_proof()
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
    let (session_sender, session_receiver) = mpsc::channel(1);
    let (msg_sender, msg_receiver) = mpsc::channel(1);
    let (node_id_sender, node_id_receiver) = mpsc::channel(1);

    if let Some(initial_membership) = initial_membership {
        session_sender
            .send(initial_membership)
            .await
            .expect("channel opened");
    }

    let session_stream = ReceiverStream::new(session_receiver)
        .map(|membership| MembershipInfo::from_membership_and_session_number(membership, 1));

    let settings = settings(local_node, minimal_network_size, node_id_sender);
    let join_handle = tokio::spawn(async move {
        Box::pin(run::<
            TestBackend,
            _,
            MockLeaderProofsGenerator,
            _,
            OncePolStreamProvider,
            _,
        >(
            UninitializedSessionEventStream::new(
                session_stream,
                FIRST_STREAM_ITEM_READY_TIMEOUT,
                Duration::ZERO,
            ),
            UninitializedFirstReadyStream::new(
                once(ready(SlotTick {
                    epoch: 1.into(),
                    slot: 1.into(),
                })),
                Duration::from_secs(1),
            ),
            ReceiverStream::new(msg_receiver),
            EpochHandler::new(TestChainService, 1.try_into().unwrap()),
            &settings,
            &overwatch_handle(),
            || {},
        ))
        .await
    });

    (join_handle, session_sender, msg_sender, node_id_receiver)
}

/// Expect the panic from the given async task,
/// and resume the panic, so the async test can check the panic message.
pub async fn resume_panic_from(join_handle: JoinHandle<Result<(), Error>>) {
    panic::resume_unwind(join_handle.await.unwrap_err().into_panic());
}

pub fn settings(
    local_id: NodeId,
    minimum_network_size: u64,
    msg_sender: NodeIdSender,
) -> BlendConfig<NodeIdSender> {
    BlendConfig {
        time: TimingSettings {
            rounds_per_session: NonZeroU64::new(1).unwrap(),
            rounds_per_interval: NonZeroU64::new(1).unwrap(),
            round_duration: Duration::from_secs(1),
            rounds_per_observation_window: NonZeroU64::new(1).unwrap(),
            rounds_per_session_transition_period: NonZeroU64::new(1).unwrap(),
            epoch_transition_period_in_slots: NonZeroU64::new(1).unwrap(),
        },
        crypto: SessionCryptographicProcessorSettings {
            non_ephemeral_signing_key: key(local_id).0,
            num_blend_layers: NonZeroU64::new(1).unwrap(),
        },
        backend: msg_sender,
        minimum_network_size: NonZeroU64::new(minimum_network_size).unwrap(),
        cover: CoverTrafficSettings::default(),
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
        settings: BlendConfig<Self::Settings>,
        _: OverwatchHandle<RuntimeServiceId>,
        membership: Membership<NodeId>,
        _: Rng,
    ) -> Self
    where
        Rng: RngCore + Send + 'static,
    {
        Self {
            membership,
            sender: settings.backend,
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
