use core::num::NonZeroU64;
use std::pin::Pin;

use async_trait::async_trait;
use futures::{
    Stream, StreamExt as _,
    future::{AbortHandle, Abortable},
};
use lb_blend::message::encap::{
    ProofsVerifier as ProofsVerifierTrait, encapsulated_message_encoded_size,
    validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_chain_service::Epoch;
use lb_cryptarchia_engine::{Era, EraSchedule};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_log_targets::{blend, diagnostic::BLEND_REACHABILITY};
use lb_utils::tokio::task::spawn_on;
use libp2p::PeerId;
use overwatch::overwatch::handle::OverwatchHandle;
use rand::RngCore;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::{
    core::backends::{
        BackendEpochInfo, BlendBackend,
        libp2p::swarm::{BlendSwarm, BlendSwarmMessage, SwarmParams},
    },
    membership::node_id::libp2p::keypair,
    message::NetworkInfo,
    metrics,
};

const LOG_TARGET: &str = blend::backend::LIBP2P;

pub(crate) mod behaviour;
pub mod settings;
pub use self::settings::{Libp2pBlendBackendEraSettings, Libp2pBlendBackendSettings};
pub(crate) mod swarm;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use self::tests::utils as core_swarm_test_utils;

/// A blend backend that uses the libp2p network stack.
pub struct Libp2pBlendBackend<ProofsVerifier> {
    swarm_task_abort_handle: AbortHandle,
    swarm_message_sender: mpsc::Sender<BlendSwarmMessage<ProofsVerifier>>,
    incoming_message_sender:
        broadcast::Sender<(EncapsulatedMessageWithVerifiedPublicHeader, Epoch)>,
    era_schedule: EraSchedule<Libp2pBlendBackendEraSettings>,
}

const CHANNEL_SIZE: usize = 64;

/// The bytes a connection may hold for us before its sender feels backpressure.
///
/// One round's share, which a neighbour sending at the rate the protocol
/// expects may have in flight before this node has read it, plus the `η` rounds
/// that neighbour waits for a stalled connection before giving up on a message.
/// Sized this way, a pause in reading shorter than the sender's own patience
/// costs nothing, and one longer than it is felt within a round or two instead
/// of being swallowed by buffer.
fn connection_receive_window(
    connection_share_per_round: NonZeroU64,
    network_absorption_in_rounds: NonZeroU64,
    num_blend_layers: NonZeroU64,
) -> u32 {
    let frame_size = encapsulated_message_encoded_size(num_blend_layers).get();

    let rounds_of_slack = network_absorption_in_rounds.get().saturating_add(1);
    u32::try_from(
        connection_share_per_round
            .get()
            .saturating_mul(rounds_of_slack)
            .saturating_mul(u64::try_from(frame_size).unwrap()),
    )
    .unwrap_or(u32::MAX)
}

#[async_trait]
impl<Rng, ProofsVerifier, RuntimeServiceId>
    BlendBackend<PeerId, Rng, ProofsVerifier, RuntimeServiceId>
    for Libp2pBlendBackend<ProofsVerifier>
where
    Rng: RngCore + Clone + Send + 'static,
    ProofsVerifier: ProofsVerifierTrait + Clone + Send + Sync + 'static,
{
    type Settings = Libp2pBlendBackendSettings;

    fn new(
        settings: Self::Settings,
        current_era: Era,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        current_epoch_info: BackendEpochInfo<PeerId, ProofsVerifier>,
        rng: Rng,
        non_ephemeral_signing_key: UnsecuredEd25519Key,
    ) -> Self {
        let (swarm_message_sender, swarm_message_receiver) = mpsc::channel(CHANNEL_SIZE);
        let (incoming_message_sender, _) = broadcast::channel(CHANNEL_SIZE);
        let era_settings = settings
            .era_schedule
            .get(current_era)
            .expect("The current era is scheduled.")
            .parameters;
        let minimum_network_size = era_settings.minimum_network_size.try_into().unwrap();
        // The transport is built once, so its receive window is the largest
        // any era needs.
        // TODO: Look into what's actually possible to achieve, given the libp2p
        // constraints.
        let max_possible_receive_window_size = settings
            .era_schedule
            .iter()
            .map(|era| {
                connection_receive_window(
                    era.parameters.connection_share_per_round,
                    era.parameters.time.network_absorption_in_rounds,
                    era.parameters.num_blend_layers,
                )
            })
            .max()
            .expect("A schedule holds at least the genesis era.");

        let swarm = BlendSwarm::<_, _>::new(SwarmParams {
            settings: &settings,
            era_settings,
            keypair: keypair(&non_ephemeral_signing_key),
            receive_window_size: max_possible_receive_window_size,
            current_epoch_info,
            incoming_message_sender: incoming_message_sender.clone(),
            minimum_network_size,
            rng,
            swarm_message_receiver,
        });

        let (swarm_task_abort_handle, swarm_task_abort_registration) = AbortHandle::new_pair();
        spawn_on(
            overwatch_handle.runtime(),
            "logos/blend/libp2p-swarm",
            Abortable::new(swarm.run(), swarm_task_abort_registration),
        );

        Self {
            swarm_task_abort_handle,
            swarm_message_sender,
            incoming_message_sender,
            era_schedule: settings.era_schedule,
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
            tracing::error!(
                target: LOG_TARGET,
                diagnostic = BLEND_REACHABILITY,
                event = "blend_send_failure",
                epoch = u32::from(intended_epoch),
                error = %e,
                "Failed to send message to BlendSwarm"
            );
        }
    }

    async fn rotate_epoch(
        &mut self,
        new_epoch_info: BackendEpochInfo<PeerId, ProofsVerifier>,
        new_era: Option<Era>,
    ) {
        let new_era = new_era.and_then(|era| {
            let era_settings = self.era_schedule.get(era);
            if era_settings.is_none() {
                tracing::error!(
                    target: LOG_TARGET,
                    "Asked to enter era {}, which is not scheduled",
                    era.into_inner()
                );
            }
            era_settings.map(|era_settings| Box::new(era_settings.parameters.clone()))
        });
        if let Err(e) = self
            .swarm_message_sender
            .send(BlendSwarmMessage::StartNewEpoch {
                new_epoch_info,
                new_era_settings: new_era,
            })
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
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedPublicHeader, Epoch)> + Send>>
    {
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

impl<ProofsVerifier> Drop for Libp2pBlendBackend<ProofsVerifier> {
    fn drop(&mut self) {
        let Self {
            swarm_task_abort_handle,
            ..
        } = self;
        swarm_task_abort_handle.abort();
        metrics::peers_negotiated_stop_reporting();
    }
}
