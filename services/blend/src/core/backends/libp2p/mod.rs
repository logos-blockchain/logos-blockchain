use std::pin::Pin;

use async_trait::async_trait;
use futures::{
    Stream, StreamExt as _,
    future::{AbortHandle, Abortable},
};
use lb_blend::message::encap::validated::{
    EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
};
use libp2p::PeerId;
use overwatch::overwatch::handle::OverwatchHandle;
use rand::RngCore;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    core::{
        backends::{
            BlendBackend, PublicInfo, SessionInfo,
            libp2p::{
                swarm::{BlendSwarm, BlendSwarmMessage, SwarmParams},
                tokio_provider::ObservationWindowTokioIntervalProvider,
            },
        },
        settings::RunningBlendConfig as BlendConfig,
    },
    message::NetworkInfo,
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
    incoming_message_sender: broadcast::Sender<(EncapsulatedMessageWithVerifiedSignature, u64)>,
}

const CHANNEL_SIZE: usize = 64;

#[async_trait]
impl<Rng, RuntimeServiceId> BlendBackend<PeerId, Rng, RuntimeServiceId> for Libp2pBlendBackend
where
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

        let swarm = BlendSwarm::<_, ObservationWindowTokioIntervalProvider>::new(SwarmParams {
            config: &config,
            current_public_info,
            incoming_message_sender: incoming_message_sender.clone(),
            minimum_network_size,
            rng,
            swarm_message_receiver,
        });

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

    async fn publish(
        &self,
        msg: EncapsulatedMessageWithVerifiedPublicHeader,
        intended_session: u64,
    ) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::Publish {
                message: Box::new(msg),
                session: intended_session,
            })
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

    fn listen_to_incoming_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedSignature, u64)> + Send>> {
        Box::pin(
            BroadcastStream::new(self.incoming_message_sender.subscribe())
                .filter_map(async |event| event.ok()),
        )
    }

    async fn network_info(&self) -> Option<NetworkInfo<PeerId>> {
        let (sender, receiver) = oneshot::channel();
        if self
            .swarm_message_sender
            .send(BlendSwarmMessage::GetNetworkInfo { reply: sender })
            .await
            .is_err()
        {
            tracing::error!(target: LOG_TARGET, "Failed to send NetworkInfo request to BlendSwarm");
            return None;
        }
        receiver.await.unwrap_or(None)
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
