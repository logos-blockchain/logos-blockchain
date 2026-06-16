use std::pin::Pin;

use async_trait::async_trait;
use futures::{
    Stream, StreamExt as _,
    future::{AbortHandle, Abortable},
};
use lb_blend::message::encap::validated::{
    EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
};
use lb_chain_service::Epoch;
use lb_log_targets::blend;
use libp2p::PeerId;
use overwatch::overwatch::handle::OverwatchHandle;
use rand::RngCore;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::{
    core::{
        backends::{
            BackendEpochInfo, BlendBackend,
            libp2p::{
                swarm::{BlendSwarm, BlendSwarmMessage, SwarmParams},
                tokio_provider::ObservationWindowTokioIntervalProvider,
            },
        },
        settings::RunningBlendConfig as BlendConfig,
    },
    message::NetworkInfo,
    metrics,
};

const LOG_TARGET: &str = blend::backend::LIBP2P;

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
    incoming_message_sender: broadcast::Sender<(EncapsulatedMessageWithVerifiedSignature, Epoch)>,
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
        current_epoch_info: BackendEpochInfo<PeerId>,
        rng: Rng,
    ) -> Self {
        let (swarm_message_sender, swarm_message_receiver) = mpsc::channel(CHANNEL_SIZE);
        let (incoming_message_sender, _) = broadcast::channel(CHANNEL_SIZE);
        let minimum_network_size = config.minimum_network_size.try_into().unwrap();

        let swarm = BlendSwarm::<_, ObservationWindowTokioIntervalProvider>::new(SwarmParams {
            config: &config,
            current_epoch_info,
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
        intended_epoch: Epoch,
    ) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::Publish {
                message: Box::new(msg),
                epoch: intended_epoch,
            })
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send message to BlendSwarm: {e}");
        }
    }

    async fn rotate_epoch(&mut self, new_epoch_info: BackendEpochInfo<PeerId>) {
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::StartNewEpoch(new_epoch_info))
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
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedSignature, Epoch)> + Send>> {
        Box::pin(
            BroadcastStream::new(self.incoming_message_sender.subscribe())
                .filter_map(async |event| handle_incoming_broadcast_event(event)),
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

/// Maps a raw broadcast event to the message it carries, if any.
///
/// On `Lagged`, the consumer fell behind the producer and the broadcast channel
/// overwrote `dropped_count` messages before they could be read. Rather than
/// dropping that silently (as a plain `event.ok()` would), we log it and bump a
/// metric so the loss is observable, then continue from the oldest
/// still-buffered message.
fn handle_incoming_broadcast_event<T>(event: Result<T, BroadcastStreamRecvError>) -> Option<T> {
    match event {
        Ok(message) => Some(message),
        Err(BroadcastStreamRecvError::Lagged(dropped_count)) => {
            tracing::warn!(
                target: LOG_TARGET,
                dropped_count,
                "Incoming blend message channel lagged behind: {dropped_count} message(s) were dropped before the event loop could process them.",
            );
            metrics::inbound_messages_dropped(dropped_count);
            None
        }
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
