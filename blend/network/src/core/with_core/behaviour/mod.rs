use core::{
    mem::{self, swap},
    num::NonZeroUsize,
    time::Duration,
};
use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    convert::Infallible,
    ops::RangeInclusive,
    task::{Context, Poll, Waker},
    time::Instant,
};

use either::Either;
use futures::Stream;
use lb_blend_message::{
    MessageIdentifier,
    encap::{
        self, encapsulated::EncapsulatedMessage,
        validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
};
use lb_blend_proofs::quota::inputs::prove::public::LeaderInputs;
use lb_blend_scheduling::{
    deserialize_encapsulated_message, membership::Membership, serialize_encapsulated_message,
};
use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
        NotifyHandler, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
        dummy::ConnectionHandler as DummyConnectionHandler,
    },
};

use crate::core::with_core::{
    behaviour::{
        handler::{
            ConnectionHandler, FromBehaviour, ToBehaviour, conn_maintenance::ConnectionMonitor,
        },
        old_session::OldSession,
    },
    error::Error,
};

mod handler;
mod old_session;

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::network::core::core::behaviour";
const SENSITIVITY_INTERVAL_FOR_DUPLICATES: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct Config {
    /// The [minimum, maximum] peering degree of this node.
    pub peering_degree: RangeInclusive<usize>,
    /// The minimum Blend network size for messages to be relayed between peers.
    pub minimum_network_size: NonZeroUsize,
}

#[derive(Debug, Clone, Copy)]
pub struct RemotePeerConnectionDetails {
    /// Role of the remote peer in this connection.
    role: Endpoint,
    /// Latest negotiated state of the peer.
    negotiated_state: NegotiatedPeerState,
    /// The ID of the connection with the peer.
    connection_id: ConnectionId,
}

impl RemotePeerConnectionDetails {
    #[must_use]
    pub const fn role(&self) -> Endpoint {
        self.role
    }

    #[must_use]
    pub const fn negotiated_state(&self) -> NegotiatedPeerState {
        self.negotiated_state
    }

    #[must_use]
    pub const fn connection_id(&self) -> ConnectionId {
        self.connection_id
    }
}

/// A [`NetworkBehaviour`] that processes incoming Blend messages, and
/// propagates messages from the Blend service to the rest of the Blend network.
///
/// The public header and uniqueness of incoming messages is validated according to the [Blend v1 specification](https://www.notion.so/nomos-tech/Blend-Protocol-Version-1-215261aa09df81ae8857d71066a80084) before the message is propagated to the swarm and to the Blend service.
/// The same checks are applied to messages received by the Blend service before
/// they are propagated to the rest of the network, making sure no peer marks
/// this node as malicious due to an invalid Blend message.
pub struct Behaviour<ProofsVerifier, ObservationWindowClockProvider> {
    /// Tracks connections between this node and other core nodes.
    ///
    /// Only connections with other core nodes that are established before the
    /// specified connection limit is reached will be upgraded and the state of
    /// the peer negotiated, monitored, and reported to the swarm.
    negotiated_peers: HashMap<PeerId, RemotePeerConnectionDetails>,
    /// The set of connections established but not yet upgraded.
    ///
    /// We use this to keep track of the role of the remote peer, to be used
    /// when deciding which connection to close when a duplicate connection to
    /// the same peer is detected.
    connections_waiting_upgrade: HashMap<(PeerId, ConnectionId), Endpoint>,
    /// Queue of events to yield to the swarm.
    events: VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    /// Waker that handles polling
    waker: Option<Waker>,
    /// The session-bound storage keeping track, for each peer, what message
    /// identifiers have been exchanged between them.
    /// Sending a message with the same identifier more than once results in
    /// the peer being flagged as malicious, and the connection dropped.
    exchanged_message_identifiers: HashMap<PeerId, HashMap<MessageIdentifier, Instant>>,
    observation_window_clock_provider: ObservationWindowClockProvider,
    current_membership: Membership<PeerId>,
    /// The [minimum, maximum] peering degree of this node.
    peering_degree: RangeInclusive<usize>,
    local_peer_id: PeerId,
    protocol_name: StreamProtocol,
    /// The minimum Blend network size for messages to be relayed between peers.
    minimum_network_size: NonZeroUsize,
    /// States for processing messages from the old session
    /// before the transition period has passed.
    old_session: Option<OldSession<ProofsVerifier>>,
    /// Verifier of the incoming messages' `PoQ`s. This is updated once per
    /// session, with the old one ending up in the old session until the
    /// transition period has elapsed.
    poq_verifier: ProofsVerifier,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum NegotiatedPeerState {
    Healthy,
    Unhealthy,
    Spammy(SpamReason),
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum SpamReason {
    UndeserializableMessage,
    DuplicateMessage,
    InvalidPublicHeader,
    TooManyMessages,
}

impl NegotiatedPeerState {
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        matches!(*self, Self::Healthy)
    }

    #[must_use]
    pub const fn is_unhealthy(&self) -> bool {
        matches!(*self, Self::Unhealthy)
    }

    #[must_use]
    pub const fn is_spammy(&self) -> bool {
        matches!(*self, Self::Spammy(_))
    }
}

#[derive(Debug)]
pub enum Event {
    /// A message received from one of the core peers, after its public header
    /// has been verified.
    Message(
        Box<EncapsulatedMessageWithVerifiedPublicHeader>,
        (PeerId, ConnectionId),
    ),
    /// A peer on a given connection has been detected as unhealthy.
    UnhealthyPeer(PeerId),
    /// A peer on a given connection that was previously unhealthy has returned
    /// to a healthy state.
    HealthyPeer(PeerId),
    /// A connection with a peer has dropped. The last state that was negotiated
    /// with the peer is also returned.
    PeerDisconnected(PeerId, NegotiatedPeerState),
    /// An outbound connection request was successfully negotiated with the
    /// remote peer.
    OutboundConnectionUpgradeSucceeded(PeerId),
    /// An inbound connection was successfully negotiated.
    InboundConnectionUpgradeSucceeded(PeerId),
    /// An outbound connection request failed to be upgraded, meaning the peer
    /// is a remote core but something failed when negotiating Blend protocol
    /// support.
    OutboundConnectionUpgradeFailed(PeerId),
    /// An inbound connection failed to be upgraded, meaning the peer is a
    /// remote core but something failed when negotiating Blend protocol
    /// support.
    InboundConnectionUpgradeFailed(PeerId),
}

impl<ProofsVerifier, ObservationWindowClockProvider>
    Behaviour<ProofsVerifier, ObservationWindowClockProvider>
{
    #[must_use]
    pub fn new(
        config: &Config,
        observation_window_clock_provider: ObservationWindowClockProvider,
        current_membership: Membership<PeerId>,
        local_peer_id: PeerId,
        protocol_name: StreamProtocol,
        poq_verifier: ProofsVerifier,
    ) -> Self {
        Self {
            negotiated_peers: HashMap::with_capacity(*config.peering_degree.end()),
            events: VecDeque::new(),
            waker: None,
            exchanged_message_identifiers: HashMap::with_capacity(current_membership.size()),
            observation_window_clock_provider,
            current_membership,
            peering_degree: config.peering_degree.clone(),
            connections_waiting_upgrade: HashMap::new(),
            local_peer_id,
            protocol_name,
            minimum_network_size: config.minimum_network_size,
            old_session: None,
            poq_verifier,
        }
    }

    pub(crate) fn start_new_session(
        &mut self,
        new_membership: Membership<PeerId>,
        new_verifier: ProofsVerifier,
    ) {
        self.connections_waiting_upgrade.clear();
        self.current_membership = new_membership;

        self.stop_old_session();

        let old_verifier = {
            let mut new_verifier = new_verifier;
            swap(&mut new_verifier, &mut self.poq_verifier);
            new_verifier
        };

        self.old_session = Some(OldSession::new(
            mem::take(&mut self.negotiated_peers)
                .into_iter()
                .map(|(peer_id, details)| (peer_id, details.connection_id))
                .collect(),
            mem::take(&mut self.exchanged_message_identifiers),
            old_verifier,
        ));

        tracing::info!(target: LOG_TARGET, "Started a new session by passing negotiated peers and exchanged message IDs to the old session. Now, no negotiated peers in the current session.");
    }

    pub(crate) fn finish_session_transition(&mut self) {
        self.stop_old_session();
    }

    fn stop_old_session(&mut self) {
        if let Some(old_session) = self.old_session.take() {
            let mut events = old_session.stop();
            let num_events = events.len();
            self.events.append(&mut events);
            if num_events > 0 {
                self.try_wake();
            }
        }
    }

    #[must_use]
    pub fn num_healthy_peers(&self) -> usize {
        self.negotiated_peers
            .values()
            .filter(|state| state.negotiated_state.is_healthy())
            .count()
    }

    pub const fn minimum_healthy_peering_degree(&self) -> usize {
        *self.peering_degree.start()
    }

    #[must_use]
    pub fn available_connection_slots(&self) -> usize {
        self.peering_degree
            .end()
            .saturating_sub(self.negotiated_peers.len())
    }

    /// Force send a message to a peer, as long as the peer is connected, no
    /// matter the state the connection is in.
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn force_send_message_to_peer(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        peer_id: PeerId,
    ) -> Result<(), Error> {
        let serialized_message = serialize_encapsulated_message(message);
        self.force_send_serialized_message_to_peer(serialized_message, peer_id)
    }

    /// Force send a serialized message to a peer (without trying to deserialize
    /// nor validating it first), as long as the peer is connected, no
    /// matter the state the connection is in.
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn force_send_serialized_message_to_peer(
        &mut self,
        serialized_message: Vec<u8>,
        peer_id: PeerId,
    ) -> Result<(), Error> {
        let Some(RemotePeerConnectionDetails { connection_id, .. }) =
            self.negotiated_peers.get(&peer_id)
        else {
            return Err(Error::NoPeers);
        };
        tracing::debug!(target: LOG_TARGET, "Notifying handler with peer {peer_id:?} on connection {connection_id:?} to deliver already-serialized message.");
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(serialized_message)),
        });
        self.try_wake();
        Ok(())
    }

    #[cfg(test)]
    pub const fn exchanged_message_identifiers(
        &self,
    ) -> &HashMap<PeerId, HashMap<MessageIdentifier, Instant>> {
        &self.exchanged_message_identifiers
    }

    pub const fn negotiated_peers(&self) -> &HashMap<PeerId, RemotePeerConnectionDetails> {
        &self.negotiated_peers
    }

    /// Forwards a message to all connected and healthy peers except the
    /// excluded peer.
    ///
    /// For each potential recipient, a uniqueness check is performed to avoid
    /// sending a duplicate message to a peer and be marked as malicious by
    /// them.
    ///
    /// Returns [`Error::NoPeers`] if there are no connected peers
    /// that support the blend protocol or that have not yet received the
    /// message.
    fn forward_validated_message_and_maybe_exclude(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        excluded_peer: Option<PeerId>,
    ) -> Result<(), Error> {
        let message_id = message.id();

        let serialized_message = serialize_encapsulated_message(message);
        let mut at_least_one_receiver = false;
        tracing::debug!(target: LOG_TARGET, "Forwarding message with id {:?}. Negotiated peers: {:?}. Excluded peer: {excluded_peer:?}", hex::encode(message_id), self.negotiated_peers());
        self.negotiated_peers
            .iter()
            // Exclude the peer the message was received from.
            .filter(|(peer_id, _)| excluded_peer != Some(**peer_id))
            // Exclude from the list of candidate peers any peer that is not in a healthy state.
            .filter(|(_, peer_state)| peer_state.negotiated_state.is_healthy())
            .for_each(|(peer_id, RemotePeerConnectionDetails { connection_id, .. })| {
                if let Entry::Vacant(message_peer_entry) = self
                    .exchanged_message_identifiers
                    .entry(*peer_id)
                    .or_default()
                    .entry(message_id)
                {
                    tracing::debug!(target: LOG_TARGET, "Notifying handler with peer {peer_id:?} on connection {connection_id:?} to deliver message.");
                    message_peer_entry.insert(Instant::now());
                    self.events.push_back(ToSwarm::NotifyHandler {
                        peer_id: *peer_id,
                        handler: NotifyHandler::One(*connection_id),
                        event: Either::Left(FromBehaviour::Message(serialized_message.clone())),
                    });
                    at_least_one_receiver = true;
                } else {
                    tracing::trace!(target: LOG_TARGET, "Not sending message to peer {peer_id:?} because we already exchanged this message with them.");
                }
            });

        if at_least_one_receiver {
            self.try_wake();
            Ok(())
        } else {
            Err(Error::NoPeers)
        }
    }

    fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    /// Notify the handler of the provided connection to close all its
    /// substreams. Leaving it up to the swarm to decide what to do with the
    /// connection.
    ///
    /// This function does not perform any checks to verify whether the
    /// specified connection is stored or not.
    fn close_connection(&mut self, (peer_id, connection_id): (PeerId, ConnectionId)) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: Either::Left(FromBehaviour::CloseSubstreams),
        });
        self.try_wake();
    }

    fn is_network_large_enough(&self) -> bool {
        self.current_membership.size() >= self.minimum_network_size.get()
    }

    /// Handle a new negotiated connection.
    ///
    /// If this peer has already a connection with the connecting peer, the
    /// connection selection logic will be run. Otherwise, the new connection
    /// will be accepted as long as this peer does not have the maximum number
    /// of connections already established.
    ///
    /// Regardless of which road is taken, the connection is removed from the
    /// set of pending connections since it has now been processed.
    ///
    /// # Panics
    ///
    /// If the specified connection is not present in the map of connections
    /// waiting to be upgraded and this connection has not already been upgraded
    /// before, since we need to peer role (i.e., dialer or listener) before
    /// moving the connection into a different storage map.
    fn handle_negotiated_connection(&mut self, (peer_id, connection_id): (PeerId, ConnectionId)) {
        let Some(new_connection_peer_role) = self
            .connections_waiting_upgrade
            .remove(&(peer_id, connection_id))
        else {
            tracing::trace!(target: LOG_TARGET, "Negotiated connection ({peer_id:?}, {connection_id:?}) not found in map of waiting connections. This is because a different substream event was used to upgrade or drop the connection");
            // We cannot assert anything here, since also for a connection we are not
            // willing to upgrade, there can be two connection handler events.
            return;
        };

        if self.negotiated_peers.contains_key(&peer_id) {
            self.handle_negotiated_connection_for_existing_peer(
                (peer_id, connection_id),
                new_connection_peer_role,
            );
        } else {
            self.handle_negotiated_connection_for_new_peer(
                (peer_id, connection_id),
                new_connection_peer_role,
            );
        }
    }

    /// Handle a newly upgraded connection for a peer that this peer is not
    /// already connected to.
    ///
    /// If this peer has already reached its maximum peering degree, the
    /// connection will be discarded.
    ///
    /// This function assumes that no entry for the provided peer ID is present
    /// in the map of already upgraded connections.
    fn handle_negotiated_connection_for_new_peer(
        &mut self,
        (peer_id, connection_id): (PeerId, ConnectionId),
        peer_role: Endpoint,
    ) {
        // We need to check if we still have available connection slots, as it is
        // possible, especially upon session transition, that more than the maximum
        // allowed number of peers are trying to connect to us. So once the stream is
        // actually upgraded, we downgrade it again if we do not have space left for it.
        // By not adding the new connection to the map of negotiated peers, the swarm
        // will not be notified about this dropped connection, which is what we want.
        if self.available_connection_slots() == 0 {
            tracing::debug!(target: LOG_TARGET, "Connection {connection_id:?} with peer {peer_id:?} must be closed because peering degree limit has already been reached.");
            self.close_connection((peer_id, connection_id));
            return;
        }
        debug_assert!(
            !self.negotiated_peers.contains_key(&peer_id),
            "We are assuming the peer is not connected to us."
        );
        tracing::debug!(target: LOG_TARGET, "Connection {connection_id:?} with peer {peer_id:?} has been negotiated.");
        self.negotiated_peers.insert(
            peer_id,
            RemotePeerConnectionDetails {
                role: peer_role,
                negotiated_state: NegotiatedPeerState::Healthy,
                connection_id,
            },
        );
        // Notify the Swarm about the successful negotiation.
        self.events
            .push_back(ToSwarm::GenerateEvent(if peer_role == Endpoint::Listener {
                Event::OutboundConnectionUpgradeSucceeded(peer_id)
            } else {
                Event::InboundConnectionUpgradeSucceeded(peer_id)
            }));
        self.try_wake();
    }

    /// Handle a newly upgraded connection for a peer that this peer is already
    /// connected to.
    ///
    /// Depending on the outcome of comparing the two peers' IDs, either the
    /// existing connection is replaced with the new one, or the new one is
    /// discarded in favor of the existing one.
    ///
    /// # Panics
    ///
    /// If there is no negotiated connection for the given peer in the relative
    /// storage.
    fn handle_negotiated_connection_for_existing_peer(
        &mut self,
        (peer_id, new_connection_id): (PeerId, ConnectionId),
        new_role: Endpoint,
    ) {
        tracing::trace!(target: LOG_TARGET, "Handling connection ({peer_id:?}, {new_connection_id:?}) where the peer is already negotiated.");
        let existing_connection = self
            .negotiated_peers
            .get(&peer_id)
            .unwrap_or_else(|| {
                panic!(
                    "Currently established connection with peer {peer_id:?} not found in storage of established connections.",
                )
            });
        match (existing_connection.role, new_role) {
            // Same connection direction (in case it was not caught at connection establishment
            // time), we ignore the new connection.
            (Endpoint::Dialer, Endpoint::Dialer) | (Endpoint::Listener, Endpoint::Listener) => {
                self.handle_connected_peer_duplicate_connection((peer_id, new_connection_id));
            }
            (Endpoint::Listener, Endpoint::Dialer) | (Endpoint::Dialer, Endpoint::Listener) => {
                self.handle_connected_peer_reverse_connection((peer_id, new_connection_id));
            }
        }
    }

    /// Close the new connection since there is already an established one in
    /// the same direction.
    fn handle_connected_peer_duplicate_connection(
        &mut self,
        (peer_id, new_connection_id): (PeerId, ConnectionId),
    ) {
        tracing::trace!(target: LOG_TARGET, "Connection {new_connection_id:?} with peer {peer_id:?} will be closed since there is already a connection established in the same direction.");
        self.close_connection((peer_id, new_connection_id));
    }

    /// Decide which connection to keep between an established one and
    /// a new incoming one.
    ///
    /// Depending on the outcome of comparing the two peers' IDs, either the
    /// existing connection is replaced with the new one, or the new one is
    /// discarded in favor of the existing one.
    fn handle_connected_peer_reverse_connection(
        &mut self,
        (peer_id, new_connection_id): (PeerId, ConnectionId),
    ) {
        let existing_connection_details = self
            .negotiated_peers
            .get_mut(&peer_id)
            .unwrap_or_else(|| {
                panic!(
                    "Currently established connection with peer {peer_id:?} not found in storage of established connections.",
                )
            });
        // If the current connection is incoming, we close it if our peer ID is higher
        // than theirs.
        tracing::trace!(target: LOG_TARGET, "Connection with already connected peer {peer_id:?} found with the following details: {existing_connection_details:?}.");
        let should_close_established = if existing_connection_details.role == Endpoint::Dialer {
            self.local_peer_id.to_base58() > peer_id.to_base58()
        } else {
            // If the current connection is outgoing, we close it if our peer ID is lower
            // than theirs.
            self.local_peer_id.to_base58() <= peer_id.to_base58()
        };

        if should_close_established {
            tracing::trace!(target: LOG_TARGET, "Replacing established connection {:?} with peer {peer_id:?} with upgraded connection {new_connection_id:?}.", existing_connection_details.connection_id);
            let existing_connection = (peer_id, existing_connection_details.connection_id);
            // Modify the `negotiated_peers` storage directly so
            // that when the old connection is dropped, the swarm is
            // not notified.
            update_connection_id_and_direction(existing_connection_details, new_connection_id);
            // After the old connection details have been updated with the new
            // ones, notify the Swarm that the new connection has been upgraded.
            self.events.push_back(ToSwarm::GenerateEvent(
                if existing_connection_details.role == Endpoint::Listener {
                    Event::OutboundConnectionUpgradeSucceeded(peer_id)
                } else {
                    Event::InboundConnectionUpgradeSucceeded(peer_id)
                },
            ));
            // Notify the current connection handler to drop the substreams.
            self.close_connection(existing_connection);
        } else {
            tracing::trace!(target: LOG_TARGET, "Dropping upgraded connection {new_connection_id:?} with peer {peer_id:?} in favor of currently established connection {:?}", existing_connection_details.connection_id);
            // Notify the new connection handler to drop the substreams, and we do not
            // alter the storage.
            self.close_connection((peer_id, new_connection_id));
        }
    }

    /// Mark the connection with the sender of a malformed message as malicious
    /// and instruct its connection handler to drop the substream.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "TODO: enable this logic after investigating session/epoch transition issues"
    )]
    #[expect(
        clippy::unused_self,
        reason = "TODO: enable this logic after investigating session/epoch transition issues"
    )]
    fn close_spammy_connection(
        &mut self,
        (peer_id, connection_id): (PeerId, ConnectionId),
        reason: SpamReason,
    ) {
        tracing::debug!(target: LOG_TARGET, ?peer_id, ?connection_id, ?reason, "Peer has been marked as spammy, but not closing the connection just for debugging");
        // TODO: Enable this logic after investigating session/epoch transition
        // issues tracing::debug!(target: LOG_TARGET, "Closing
        // connection {connection_id:?} with spammy peer {peer_id:?}.");
        // self.set_connection_to_spammy((peer_id, connection_id), reason);
        // self.close_connection((peer_id, connection_id));
    }

    fn set_connection_to_spammy(
        &mut self,
        (peer_id, connection_id): (PeerId, ConnectionId),
        reason: SpamReason,
    ) {
        self.update_state_for_negotiated_peer(
            (peer_id, connection_id),
            NegotiatedPeerState::Spammy(reason),
        );
    }

    /// Update the state of an already negotiated peer if exists,
    /// returning the previous state.
    fn update_state_for_negotiated_peer(
        &mut self,
        (peer_id, connection_id): (PeerId, ConnectionId),
        state: NegotiatedPeerState,
    ) -> Option<NegotiatedPeerState> {
        let peer_details = self.negotiated_peers.get_mut(&peer_id)?;
        // We double check we are dealing with the expected connection.
        // This could be false if `connection_id` is from the old session.
        if peer_details.connection_id != connection_id {
            tracing::debug!(target: LOG_TARGET, "Provided connection ID {connection_id:?} does not match the stored connection ID {:?} for peer {peer_id:?}. Ignoring state update.", peer_details.connection_id);
            return Some(state);
        }
        Some(mem::replace(&mut peer_details.negotiated_state, state))
    }

    /// Handle an unhealthy connection if it exists in the current session.
    /// If not, it is ignored.
    fn handle_unhealthy_connection(&mut self, (peer_id, connection_id): (PeerId, ConnectionId)) {
        // Notify swarm only on first transition into unhealthy state.
        if let Some(prev_state) = self.update_state_for_negotiated_peer(
            (peer_id, connection_id),
            NegotiatedPeerState::Unhealthy,
        ) && prev_state != NegotiatedPeerState::Unhealthy
        {
            tracing::debug!(target: LOG_TARGET, "Peer {peer_id:?} has been marked as unhealthy.");
            self.events
                .push_back(ToSwarm::GenerateEvent(Event::UnhealthyPeer(peer_id)));
            self.try_wake();
        }
    }

    /// Handle a unhealthy connection if it exists in the current session.
    /// If not, it is ignored.
    fn handle_healthy_connection(&mut self, (peer_id, connection_id): (PeerId, ConnectionId)) {
        // Notify swarm only on first transition into healthy state.
        if let Some(prev_state) = self.update_state_for_negotiated_peer(
            (peer_id, connection_id),
            NegotiatedPeerState::Healthy,
        ) && prev_state != NegotiatedPeerState::Healthy
        {
            tracing::debug!(target: LOG_TARGET, "Peer {peer_id:?} has been marked as healthy.");
            self.events
                .push_back(ToSwarm::GenerateEvent(Event::HealthyPeer(peer_id)));
            self.try_wake();
        }
    }

    /// Check if a message with a given ID has been exchanged with a peer
    /// before. If not, the cache entry is updated. Otherwise an `Error` is
    /// returned.
    fn check_and_update_message_cache(
        &mut self,
        message_id: &MessageIdentifier,
        (peer_id, connection_id): (PeerId, ConnectionId),
    ) -> Result<(), ()> {
        let exchanged_message_identifiers = self
            .exchanged_message_identifiers
            .entry(peer_id)
            .or_default();

        match exchanged_message_identifiers.entry(*message_id) {
            Entry::Vacant(vacant_message_entry) => {
                vacant_message_entry.insert(Instant::now());
                Ok(())
            }
            Entry::Occupied(occupied_message_entry) => {
                let time_sent = occupied_message_entry.get();
                // If the duplicate arrived within the sensitivity interval, it is
                // likely due to a race condition (both peers forwarding the same
                // message to each other). Simply ignore it.
                if Instant::now().duration_since(*time_sent) <= SENSITIVITY_INTERVAL_FOR_DUPLICATES
                {
                    tracing::debug!(target: LOG_TARGET, "Neighbor {peer_id:?} on connection {connection_id:?} sent us a message previously already exchanged but within the sensitivity window. Simply ignoring the message.");
                    Ok(())
                } else {
                    tracing::debug!(target: LOG_TARGET, "Neighbor {peer_id:?} on connection {connection_id:?} sent us a message previously already exchanged. Marking it as spammy.");
                    self.close_spammy_connection(
                        (peer_id, connection_id),
                        SpamReason::DuplicateMessage,
                    );
                    Err(())
                }
            }
        }
    }

    /// Return `True` if this peer has an established (negotiated or not)
    /// incoming connection with the specified peer, `False` otherwise.
    fn has_incoming_connection_with_peer(&self, remote_peer: &PeerId) -> bool {
        self.has_negotiated_incoming_connection_with_peer(remote_peer)
            || self.has_pending_incoming_connection_with_peer(remote_peer)
    }

    /// Return `True` if this peer has an established (negotiated or not)
    /// outgoing connection with the specified peer, `False` otherwise.
    fn has_outgoing_connection_with_peer(&self, remote_peer: &PeerId) -> bool {
        self.has_negotiated_outgoing_connection_with_peer(remote_peer)
            || self.has_pending_outgoing_connection_with_peer(remote_peer)
    }

    /// Return `True` if there is a negotiated inbound connection with the
    /// provided peer.
    fn has_negotiated_incoming_connection_with_peer(&self, remote_peer: &PeerId) -> bool {
        self.negotiated_peers
            .get(remote_peer)
            .is_some_and(|remote| remote.role.is_dialer())
    }

    /// Return `true` if there is a negotiated outbound connection with the
    /// provided peer.
    fn has_negotiated_outgoing_connection_with_peer(&self, remote_peer: &PeerId) -> bool {
        self.negotiated_peers
            .get(remote_peer)
            .is_some_and(|remote| remote.role.is_listener())
    }

    /// Return `True` if there is at least one inbound connection pending
    /// upgrade with the provided peer.
    // TODO: Find a different data structure to be able to perform this check in
    // O(1).
    fn has_pending_incoming_connection_with_peer(&self, remote_peer: &PeerId) -> bool {
        self.connections_waiting_upgrade
            .iter()
            .any(|((peer_id, _), remote_endpoint)| {
                peer_id == remote_peer && remote_endpoint.is_dialer()
            })
    }

    /// Return `True` if there is at least one outbound connection pending
    /// upgrade with the provided peer.
    // TODO: Find a different data structure to be able to perform this check in
    // O(1).
    fn has_pending_outgoing_connection_with_peer(&self, remote_peer: &PeerId) -> bool {
        self.connections_waiting_upgrade
            .iter()
            .any(|((peer_id, _), remote_endpoint)| {
                peer_id == remote_peer && remote_endpoint.is_listener()
            })
    }
}

/// Revert the direction of a connection and updates its ID with the provided
/// one.
fn update_connection_id_and_direction(
    existing_connection: &mut RemotePeerConnectionDetails,
    new_connection_id: ConnectionId,
) {
    existing_connection.role = existing_connection.role.reverse();
    existing_connection.connection_id = new_connection_id;
}

impl<ProofsVerifier, ObservationWindowClockProvider>
    Behaviour<ProofsVerifier, ObservationWindowClockProvider>
where
    ProofsVerifier: encap::ProofsVerifier,
{
    /// Publish an already-encapsulated message to all connected peers
    /// in the current or old session.
    ///
    /// Before the message is propagated, its public header is validated to
    /// make sure the receiving peer won't mark us as malicious.
    ///
    /// If the message is successfully validated with the old session verifier,
    /// it is published using the old session.
    /// Otherwise, it is validated with the current session verifier, and
    /// if valid, published using the current session.
    pub fn validate_and_publish_message(
        &mut self,
        message: EncapsulatedMessage,
    ) -> Result<(), Error> {
        self.validate_and_forward_message_except(message, None)
    }

    /// Forwards a message to all healthy connections except the [`except`]
    /// connection.
    ///
    /// Before the message is forwarded, its public header is validated to
    /// make sure the receiving peer won't mark us as malicious.
    ///
    /// If the [`except`] connection is part of the old session, the message is
    /// forwarded to the connections in the old session.
    /// Otherwise, it is forwarded to the connections in the current session.
    ///
    /// Returns [`Error::NoPeers`] if there are no connected peers that support
    /// the blend protocol.
    pub fn validate_and_forward_message(
        &mut self,
        message: EncapsulatedMessage,
        except: (PeerId, ConnectionId),
    ) -> Result<(), Error> {
        self.validate_and_forward_message_except(message, Some(except))
    }

    fn validate_and_forward_message_except(
        &mut self,
        message: EncapsulatedMessage,
        except: Option<(PeerId, ConnectionId)>,
    ) -> Result<(), Error> {
        if let Some(old_session) = &mut self.old_session
            && old_session
                .validate_and_publish_message(message.clone())
                .is_ok()
        {
            return Ok(());
        }

        let validated_message =
            self.validate_encapsulated_message_public_header_with_current_session(message)?;
        self.forward_validated_message_and_maybe_exclude(
            &validated_message,
            except.map(|(peer_id, _)| peer_id),
        )
    }

    // Try to validate an encapsulated public header with the current session
    // verifier.
    fn validate_encapsulated_message_public_header_with_current_session(
        &self,
        message: EncapsulatedMessage,
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        message
            .verify_public_header(&self.poq_verifier)
            .map_err(|_| Error::InvalidMessage)
    }

    fn handle_received_serialized_encapsulated_message(
        &mut self,
        serialized_message: &[u8],
        (from_peer_id, from_connection_id): (PeerId, ConnectionId),
    ) {
        // First, try to handle the message in the context of the old session.
        // If it is not part of the old session, try with the current session.
        if let Some(old_session) = &mut self.old_session {
            match old_session.handle_received_serialized_encapsulated_message(
                serialized_message,
                (from_peer_id, from_connection_id),
            ) {
                Ok(handled) => {
                    if handled {
                        return;
                    }
                }
                Err(e) => {
                    tracing::trace!(target: LOG_TARGET, "Failed to handle message from the old session: {e:?}");
                    return;
                }
            }
        }

        // Mark a peer as malicious if it sends a un-deserializable message: https://www.notion.so/nomos-tech/Blend-Protocol-Version-1-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df8172927bebb75d8b988e.
        let Ok(deserialized_encapsulated_message) =
            deserialize_encapsulated_message(serialized_message)
        else {
            tracing::debug!(target: LOG_TARGET, "Failed to deserialize encapsulated message.");
            self.close_spammy_connection(
                (from_peer_id, from_connection_id),
                SpamReason::UndeserializableMessage,
            );
            return;
        };

        let message_identifier = deserialized_encapsulated_message.id();

        // Mark a core peer as malicious if it sends a duplicate message maliciously (i.e., if a message with the same identifier was already exchanged with them): https://www.notion.so/nomos-tech/Blend-Protocol-Version-1-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df81fc86bdce264466efd3.
        let Ok(()) = self.check_and_update_message_cache(
            &message_identifier,
            (from_peer_id, from_connection_id),
        ) else {
            return;
        };
        // Verify the message public header, or else mark the peer as malicious: https://www.notion.so/nomos-tech/Blend-Protocol-Version-1-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df81859cebf5e3d2a5cd8f.
        let Ok(validated_message) = self
            .validate_encapsulated_message_public_header_with_current_session(
                deserialized_encapsulated_message,
            )
        else {
            tracing::debug!(target: LOG_TARGET, "Neighbor sent us a message with an invalid public header. Marking it as spammy.");
            self.close_spammy_connection(
                (from_peer_id, from_connection_id),
                SpamReason::InvalidPublicHeader,
            );
            return;
        };

        // Notify the swarm about the received message, so that it can be further
        // processed by the core protocol module.
        self.events.push_back(ToSwarm::GenerateEvent(Event::Message(
            Box::new(validated_message),
            (from_peer_id, from_connection_id),
        )));
        self.try_wake();
    }

    /// Instruct both current and past session proof verifier (if present) of a
    /// new epoch.
    pub(crate) fn start_new_epoch(&mut self, new_pol_inputs: LeaderInputs) {
        self.poq_verifier.start_epoch_transition(new_pol_inputs);
        if let Some(old_session) = &mut self.old_session {
            old_session.start_new_epoch(new_pol_inputs);
        }
    }

    /// Instruct both current and past session proof verifier (if present) that
    /// the epoch transition period is over.
    pub(crate) fn finish_epoch_transition(&mut self) {
        self.poq_verifier.complete_epoch_transition();
        if let Some(old_session) = &mut self.old_session {
            old_session.finish_epoch_transition();
        }
    }
}

impl<ProofsVerifier, ObservationWindowClockProvider> NetworkBehaviour
    for Behaviour<ProofsVerifier, ObservationWindowClockProvider>
where
    ProofsVerifier: encap::ProofsVerifier + 'static,
    ObservationWindowClockProvider: IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>
        + 'static,
{
    type ConnectionHandler = Either<
        ConnectionHandler<ObservationWindowClockProvider::IntervalStream>,
        DummyConnectionHandler,
    >;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // If the new peer makes the set of established connections too large, do not
        // try to upgrade the connection.
        if self.negotiated_peers.len() >= *self.peering_degree.end() {
            tracing::trace!(target: LOG_TARGET, "Inbound connection {connection_id:?} with peer {peer_id:?} will not be upgraded since we are already at maximum peering capacity.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        // If there is already an established or pending inbound connection with
        // the given peer, do not try to upgrade the new one as we already have an
        // inbound connection. Otherwise, we let the connection upgrade, and we will
        // close one of the two connections depending on the comparison result of
        // local and remote peer IDs.
        if self.has_incoming_connection_with_peer(&peer_id) {
            tracing::trace!(target: LOG_TARGET, "Inbound connection {connection_id:?} with peer {peer_id:?} will not be upgraded since there is already an inbound connection established or pending.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        Ok(if !self.is_network_large_enough() {
            tracing::debug!(target: LOG_TARGET, "Denying inbound connection {connection_id:?} with peer {peer_id:?} because membership size is too small.");
            Either::Right(DummyConnectionHandler)
        } else if self.current_membership.contains(&peer_id) {
            tracing::debug!(target: LOG_TARGET, "Upgrading inbound connection {connection_id:?} with core peer {peer_id:?}.");
            self.connections_waiting_upgrade
                .insert((peer_id, connection_id), Endpoint::Dialer);
            Either::Left(ConnectionHandler::new(
                ConnectionMonitor::new(self.observation_window_clock_provider.interval_stream()),
                self.protocol_name.clone(),
            ))
        } else {
            tracing::debug!(target: LOG_TARGET, "Denying inbound connection {connection_id:?} with edge peer {peer_id:?}.");
            Either::Right(DummyConnectionHandler)
        })
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // If the new peer makes the set of established connections too large, do not
        // try to upgrade the connection.
        if self.negotiated_peers.len() >= *self.peering_degree.end() {
            tracing::trace!(target: LOG_TARGET, "Outbound connection {connection_id:?} with peer {peer_id:?} will not be upgraded since we are already at maximum peering capacity.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        // If there is already an established outbound connection with the given peer,
        // do not try to upgrade the new one as we already have an outbound connection.
        // Otherwise, we let the connection upgrade, and we will close one of the two
        // connections depending on the comparison result of local and remote peer IDs.
        if self.has_outgoing_connection_with_peer(&peer_id) {
            tracing::trace!(target: LOG_TARGET, "Outbound connection {connection_id:?} with peer {peer_id:?} will not be upgraded since there is already an outbound connection established.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        Ok(if !self.is_network_large_enough() {
            tracing::debug!(target: LOG_TARGET, "Denying outbound connection {connection_id:?} with peer {peer_id:?} because membership size is too small.");
            Either::Right(DummyConnectionHandler)
        } else if self.current_membership.contains(&peer_id) {
            tracing::debug!(target: LOG_TARGET, "Upgrading outbound connection {connection_id:?} with core peer {peer_id:?}.");
            self.connections_waiting_upgrade
                .insert((peer_id, connection_id), Endpoint::Listener);
            Either::Left(ConnectionHandler::new(
                ConnectionMonitor::new(self.observation_window_clock_provider.interval_stream()),
                self.protocol_name.clone(),
            ))
        } else {
            tracing::debug!(target: LOG_TARGET, "Denying outbound connection {connection_id:?} with edge peer {peer_id:?}.");
            Either::Right(DummyConnectionHandler)
        })
    }

    /// Informs the behaviour about an event from the [`Swarm`].
    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id,
            connection_id,
            endpoint: local_endpoint,
            ..
        }) = event
        {
            // Try to close the connection if it exists in the old session.
            if let Some(old_session) = &mut self.old_session
                && old_session.handle_closed_connection(&(peer_id, connection_id))
            {
                return;
            }

            // We notify the swarm of any connection that failed to be upgraded.
            if let Some(remote_peer_role) = self
                .connections_waiting_upgrade
                .remove(&(peer_id, connection_id))
            {
                debug_assert!(
                    local_endpoint.to_endpoint().reverse() == remote_peer_role,
                    "Remote peer endpoint provided by event and the one stored do not match."
                );
                // Notify the swarm about the negotiation failure.
                self.events.push_back(ToSwarm::GenerateEvent(
                    if remote_peer_role == Endpoint::Listener {
                        Event::OutboundConnectionUpgradeFailed(peer_id)
                    } else {
                        Event::InboundConnectionUpgradeFailed(peer_id)
                    },
                ));
                self.try_wake();
                return;
            }

            let Entry::Occupied(peer_details_entry) = self.negotiated_peers.entry(peer_id) else {
                // This event was not meant for us.
                return;
            };

            let negotiated_connection_id = peer_details_entry.get().connection_id;

            if negotiated_connection_id == connection_id {
                let negotiated_peer_details = peer_details_entry.remove();
                self.exchanged_message_identifiers.remove(&peer_id);
                self.events
                    .push_back(ToSwarm::GenerateEvent(Event::PeerDisconnected(
                        peer_id,
                        negotiated_peer_details.negotiated_state,
                    )));
                self.try_wake();
            } else {
                // We are closing a different connection for the same peer, so a
                // connection we have either replaced with a new one or ignored
                // in favor of the old one.
                tracing::trace!(target: LOG_TARGET, "Closing replaced or ignored connection {connection_id:?} with peer {peer_id:?}.");
            }
        }
    }

    /// Handles an event generated by the [`BlendConnectionHandler`]
    /// dedicated to the connection identified by `peer_id` and `connection_id`.
    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            Either::Left(event) => match event {
                // A message was forwarded from the peer.
                ToBehaviour::Message(message) => {
                    self.handle_received_serialized_encapsulated_message(
                        &message,
                        (peer_id, connection_id),
                    );
                }
                // The inbound/outbound connection was fully negotiated by the peer,
                // which means that the peer supports the blend protocol. We consider them healthy
                // by default.
                ToBehaviour::FullyNegotiatedInbound | ToBehaviour::FullyNegotiatedOutbound => {
                    self.handle_negotiated_connection((peer_id, connection_id));
                }
                ToBehaviour::SpammyPeer => {
                    // We do not explicitly close the connection here since the connection handler
                    // will already do that for us.
                    self.set_connection_to_spammy(
                        (peer_id, connection_id),
                        SpamReason::TooManyMessages,
                    );
                }
                ToBehaviour::UnhealthyPeer => {
                    self.handle_unhealthy_connection((peer_id, connection_id));
                }
                ToBehaviour::HealthyPeer => {
                    self.handle_healthy_connection((peer_id, connection_id));
                }
                ToBehaviour::IOError(_) | ToBehaviour::DialUpgradeError(_) => {}
            },
        }
    }

    /// Polls for things that swarm should do.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(old_session) = &mut self.old_session
            && let Poll::Ready(event) = old_session.poll(cx)
        {
            return Poll::Ready(event);
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        self.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub trait IntervalStreamProvider {
    type IntervalStream: Stream<Item = Self::IntervalItem>;
    type IntervalItem;

    fn interval_stream(&self) -> Self::IntervalStream;
}

/// A trait for reversable types.
trait Reverse: Sized {
    /// Consumes `self` and returns its reverse.
    fn reverse(self) -> Self;
}

impl Reverse for Endpoint {
    fn reverse(self) -> Self {
        match self {
            Self::Dialer => Self::Listener,
            Self::Listener => Self::Dialer,
        }
    }
}
