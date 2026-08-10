use core::num::{NonZeroU64, NonZeroUsize};
use std::{
    collections::{HashSet, VecDeque},
    convert::Infallible,
    mem,
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Duration,
};

use either::Either;
use futures::StreamExt as _;
use lb_blend_message::encap::{
    ProofsVerifier as ProofsVerifierTrait, validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_blend_scheduling::{deserialize_encapsulated_message, membership::Membership};
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::blend;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
        NotifyHandler, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
        dummy::ConnectionHandler as DummyConnectionHandler,
    },
};

use crate::core::{
    poq_verification::{PendingPoQVerifications, PoQVerificationOutcome, spawn_poq_verification},
    with_edge::behaviour::handler::{ConnectionHandler, FromBehaviour, ToBehaviour},
};

mod handler;

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::network::core::edge::BEHAVIOUR;

#[cfg_attr(
    test,
    expect(
        clippy::large_enum_variant,
        reason = "We have a second variant only for tests. We can ignore the Clippy warning in that case."
    )
)]
#[derive(Debug)]
pub enum Event {
    /// A message received from one of the edge peers, after its whole public
    /// header — signature and `PoQ` — has been verified.
    Message(EncapsulatedMessageWithVerifiedPublicHeader),
    #[cfg(test)]
    NegotiatedConnection { peer: PeerId },
}

#[derive(Debug)]
pub struct Config {
    pub connection_timeout: Duration,
    pub max_incoming_connections: usize,
    pub minimum_network_size: NonZeroUsize,
    /// `ß_c`: the fixed number of encapsulation layers every well-formed Blend
    /// message carries. Used to validate the layout of messages received from
    /// remote peers before processing them.
    pub num_blend_layers: NonZeroU64,
}

/// A [`NetworkBehaviour`]:
/// - receives messages from edge nodes and forwards them to the swarm.
pub struct Behaviour<ProofsVerifier> {
    /// Queue of events to yield to the swarm.
    events: VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    /// Waker that handles polling
    waker: Option<Waker>,
    current_membership: Membership<PeerId>,
    /// The epoch the messages received from edge nodes belong to, needed to
    /// pick them up again once their `PoQ` has been verified.
    current_epoch: Epoch,
    /// Verifier for the `PoQ`s of the messages received from edge nodes.
    ///
    /// Shared rather than owned because a handle to it is passed to the
    /// blocking pool for every message received.
    proofs_verifier: Arc<ProofsVerifier>,
    /// `PoQ` verifications currently running on the blocking pool.
    pending_poq_verifications: PendingPoQVerifications,
    // Timeout to close connection with an edge node if a message is not received on time.
    connection_timeout: Duration,
    upgraded_edge_peers: HashSet<(PeerId, ConnectionId)>,
    max_incoming_connections: usize,
    protocol_name: StreamProtocol,
    minimum_network_size: NonZeroUsize,
    num_blend_layers: NonZeroU64,
}

impl<ProofsVerifier> Behaviour<ProofsVerifier> {
    #[must_use]
    pub fn new(
        config: &Config,
        current_epoch_info: (Membership<PeerId>, Epoch),
        proofs_verifier: ProofsVerifier,
        protocol_name: StreamProtocol,
    ) -> Self {
        Self {
            events: VecDeque::new(),
            waker: None,
            current_membership: current_epoch_info.0,
            current_epoch: current_epoch_info.1,
            proofs_verifier: Arc::new(proofs_verifier),
            pending_poq_verifications: PendingPoQVerifications::new(),
            connection_timeout: config.connection_timeout,
            upgraded_edge_peers: HashSet::with_capacity(config.max_incoming_connections),
            max_incoming_connections: config.max_incoming_connections,
            protocol_name,
            minimum_network_size: config.minimum_network_size,
            num_blend_layers: config.num_blend_layers,
        }
    }

    pub(crate) fn start_new_epoch(
        &mut self,
        new_epoch_info: (Membership<PeerId>, Epoch),
        new_proofs_verifier: ProofsVerifier,
    ) {
        self.current_membership = new_epoch_info.0;
        self.current_epoch = new_epoch_info.1;
        self.proofs_verifier = Arc::new(new_proofs_verifier);
        // Close all the connections without waiting for the transition period,
        // so that edge nodes can retry with the new membership.
        let peers = mem::take(&mut self.upgraded_edge_peers);
        for conn in &peers {
            self.close_substream(*conn);
        }
    }

    fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    #[must_use]
    fn available_connection_slots(&self) -> usize {
        self.max_incoming_connections
            .saturating_sub(self.upgraded_edge_peers.len())
    }

    fn handle_negotiated_connection(&mut self, connection: (PeerId, ConnectionId)) {
        // We need to check if we still have available connection slots, as it
        // is possible, especially upon epoch transition, that more
        // than the maximum allowed number of peers are trying to
        // connect to us. So once we stream is actually upgraded, we
        // downgrade it again if we do not have space left for it. This will
        // most likely, depending on the swarm configuration, result in the
        // connection being dropped.
        if self.available_connection_slots() == 0 {
            tracing::debug!(target: LOG_TARGET, "Connection {connection:?} must be closed because peering degree limit has been reached.");
            self.close_substream(connection);
            return;
        }
        tracing::debug!(target: LOG_TARGET, "Connection {connection:?} has been negotiated.");
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id: connection.0,
            handler: NotifyHandler::One(connection.1),
            event: Either::Left(FromBehaviour::StartReceiving),
        });
        self.upgraded_edge_peers.insert(connection);
        #[cfg(test)]
        self.events
            .push_back(ToSwarm::GenerateEvent(Event::NegotiatedConnection {
                peer: connection.0,
            }));
        self.try_wake();
    }

    fn close_substream(&mut self, (peer_id, connection_id): (PeerId, ConnectionId)) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: Either::Left(FromBehaviour::CloseSubstream),
        });
        self.try_wake();
    }

    fn is_network_large_enough(&self) -> bool {
        self.current_membership.size() >= self.minimum_network_size.get()
    }

    fn handle_received_serialized_encapsulated_message(
        &mut self,
        serialized_message: &[u8],
        connection: (PeerId, ConnectionId),
    ) where
        ProofsVerifier: ProofsVerifierTrait + Send + Sync + 'static,
    {
        let Ok(deserialized_encapsulated_message) =
            deserialize_encapsulated_message(serialized_message, &self.num_blend_layers)
        else {
            tracing::trace!(target: LOG_TARGET, "Failed to deserialize received message. Ignoring...");
            return;
        };

        let Ok(validated_message) = deserialized_encapsulated_message.verify_header_signature()
        else {
            tracing::trace!(target: LOG_TARGET, "Failed to validate signature of received message. Ignoring...");
            return;
        };

        // Verify the `PoQ` before the message is reported to the swarm, and hence
        // before it can be published to the core nodes.
        spawn_poq_verification(
            &self.pending_poq_verifications,
            validated_message,
            connection,
            self.current_epoch,
            &self.proofs_verifier,
            &mut self.waker,
        );
    }

    /// Acts on a completed `PoQ` verification.
    ///
    /// Unlike a core peer, an edge node is not blocked when it fails: it holds
    /// no peering slot, and its connection is closed after the single message
    /// it came to deliver anyway.
    fn handle_poq_verification_outcome(&mut self, outcome: PoQVerificationOutcome) {
        match outcome {
            PoQVerificationOutcome::Verified { message, .. } => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(Event::Message(*message)));
                self.try_wake();
            }
            PoQVerificationOutcome::Failed {
                sender,
                connection_id,
            } => {
                tracing::debug!(target: LOG_TARGET, "Dropping message from edge peer {sender:?}: its PoQ failed to verify.");
                self.close_substream((sender, connection_id));
            }
        }
    }
}

impl<ProofsVerifier> NetworkBehaviour for Behaviour<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + Send + Sync + 'static,
{
    type ConnectionHandler = Either<ConnectionHandler, DummyConnectionHandler>;
    type ToSwarm = Event;

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // If the new peer makes the set of incoming connections too large, do not try
        // to upgrade the connection.
        if self.upgraded_edge_peers.len() >= self.max_incoming_connections {
            tracing::trace!(target: LOG_TARGET, "Connected peer {peer:?} with addr {remote_addr:?} on connection {connection_id:?} will not be upgraded since we are already at maximum incoming connection capacity.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        // Allow only inbound connections from edge nodes, if the Blend network is large
        // enough.
        Ok(if !self.is_network_large_enough() {
            tracing::debug!(target: LOG_TARGET, "Denying inbound connection {connection_id:?} with peer {peer:?} with addr {remote_addr:?} because membership size is too small.");
            Either::Right(DummyConnectionHandler)
        } else if self.current_membership.contains(&peer) {
            tracing::trace!(target: LOG_TARGET, "Denying inbound connection {connection_id:?} with core peer {peer:?} with addr {remote_addr:?}.");
            Either::Right(DummyConnectionHandler)
        } else {
            tracing::debug!(target: LOG_TARGET, "Upgrading inbound connection {connection_id:?} with edge peer {peer:?} with addr {remote_addr:?}.");
            Either::Left(ConnectionHandler::new(
                self.connection_timeout,
                self.protocol_name.clone(),
            ))
        })
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // No outbound sub-stream at all, since substreams with core nodes are handled
        // elsewhere, and substreams with edge nodes are not allowed.
        Ok(Either::Right(DummyConnectionHandler))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id,
            connection_id,
            ..
        }) = event
        {
            self.upgraded_edge_peers.remove(&(peer_id, connection_id));
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            Either::Left(ToBehaviour::Message(message)) => {
                self.handle_received_serialized_encapsulated_message(
                    &message,
                    (peer_id, connection_id),
                );
            }
            Either::Left(ToBehaviour::SubstreamOpened) => {
                self.handle_negotiated_connection((peer_id, connection_id));
            }
            Either::Left(_) | Either::Right(_) => {
                tracing::trace!(target: LOG_TARGET, "Unhandled connection handler event: {event:?} from peer {peer_id:?} on connection {connection_id:?}");
            }
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        // Verifications complete off this task, so their outcome is picked up
        // here: this is where a message becomes visible to the swarm, and hence
        // publishable to the core nodes.
        while let Poll::Ready(Some(outcome)) = self.pending_poq_verifications.poll_next_unpin(cx) {
            self.handle_poq_verification_outcome(outcome);
            if let Some(event) = self.events.pop_front() {
                return Poll::Ready(event);
            }
        }

        self.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
