use std::pin::Pin;

use async_trait::async_trait;
use futures::{
    Stream, StreamExt as _,
    future::{AbortHandle, Abortable},
};
use libp2p::PeerId;
use nomos_blend::{
    message::encap::{
        ProofsVerifier as ProofsVerifierTrait, encapsulated::EncapsulatedMessage,
        validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
    proofs::quota::inputs::prove::public::LeaderInputs,
};
use overwatch::overwatch::handle::OverwatchHandle;
use rand::RngCore;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::BroadcastStream;

use crate::core::{
    backends::{
        BlendBackend, PublicInfo, SessionInfo,
        libp2p::{
            swarm::{BlendSwarm, BlendSwarmMessage, SwarmParams},
            tokio_provider::ObservationWindowTokioIntervalProvider,
        },
    },
    settings::BlendConfig,
};

const LOG_TARGET: &str = "blend::backend::libp2p";

pub(crate) mod behaviour;
pub mod settings;
pub use self::settings::Libp2pBlendBackendSettings;
mod swarm;
pub(crate) mod tokio_provider;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use self::tests::utils as core_swarm_test_utils;

/// A blend backend that uses the libp2p network stack.
pub struct Libp2pBlendBackend {
    swarm_task_abort_handle: AbortHandle,
    swarm_message_sender: mpsc::Sender<BlendSwarmMessage>,
    incoming_message_sender: broadcast::Sender<EncapsulatedMessageWithVerifiedPublicHeader>,
}

const CHANNEL_SIZE: usize = 64;

#[async_trait]
impl<Rng, ProofsVerifier, RuntimeServiceId>
    BlendBackend<PeerId, Rng, ProofsVerifier, RuntimeServiceId> for Libp2pBlendBackend
where
    ProofsVerifier: ProofsVerifierTrait + Clone + Send + 'static,
    Rng: RngCore + Clone + Send + 'static,
{
    type Settings = Libp2pBlendBackendSettings;

    fn new(
        config: BlendConfig<Self::Settings>,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        current_public_info: PublicInfo<PeerId>,
        rng: Rng,
    ) -> Self {
        let (swarm_message_sender, swarm_message_receiver) = mpsc::channel(CHANNEL_SIZE);
        let (incoming_message_sender, _) = broadcast::channel(CHANNEL_SIZE);
        let minimum_network_size = config.minimum_network_size.try_into().unwrap();

        let swarm = BlendSwarm::<_, ProofsVerifier, ObservationWindowTokioIntervalProvider>::new(
            SwarmParams {
                config: &config,
                current_public_info,
                incoming_message_sender: incoming_message_sender.clone(),
                minimum_network_size,
                rng,
                swarm_message_receiver,
            },
        );

        let (swarm_task_abort_handle, swarm_task_abort_registration) = AbortHandle::new_pair();
        overwatch_handle
            .runtime()
            .spawn(Abortable::new(swarm.run(), swarm_task_abort_registration));

        Self {
            swarm_task_abort_handle,
            swarm_message_sender,
            incoming_message_sender,
        }
    }

    fn shutdown(self) {
        drop(self);
    }

    async fn publish(&self, msg: EncapsulatedMessage) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::Publish(Box::new(msg)))
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send message to BlendSwarm: {e}");
        }
    }

    async fn rotate_session(&mut self, new_session_info: SessionInfo<PeerId>) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::StartNewSession(new_session_info))
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send new public session info to BlendSwarm: {e}");
        }
    }

    async fn complete_session_transition(&mut self) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::CompleteSessionTransition)
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send session transition termination command to BlendSwarm: {e}");
        }
    }

    async fn rotate_epoch(&mut self, new_epoch_public_info: LeaderInputs) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::StartNewEpoch(new_epoch_public_info))
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send new public epoch info to BlendSwarm: {e}");
        }
    }

    async fn complete_epoch_transition(&mut self) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::CompleteEpochTransition)
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send epoch transition termination command to BlendSwarm: {e}");
        }
    }

    fn listen_to_incoming_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = EncapsulatedMessageWithVerifiedPublicHeader> + Send>> {
        Box::pin(
            BroadcastStream::new(self.incoming_message_sender.subscribe())
                .filter_map(async |event| event.ok()),
        )
    }
}

impl Drop for Libp2pBlendBackend {
    fn drop(&mut self) {
        let Self {
            swarm_task_abort_handle,
            ..
        } = self;
        swarm_task_abort_handle.abort();
    }
}
