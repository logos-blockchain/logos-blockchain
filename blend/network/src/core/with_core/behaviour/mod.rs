use core::{
    mem::{self},
    num::NonZeroUsize,
};
use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    convert::Infallible,
    ops::RangeInclusive,
    task::{Context, Poll, Waker},
};

use either::Either;
use futures::Stream;
use lb_blend_message::encap::validated::{
    EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
};
use lb_blend_scheduling::membership::Membership;
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
        message_cache::MessageCache,
        old_session::OldSession,
        utils::{
            forward_validated_message_and_update_cache,
            handle_received_serialized_encapsulated_message_and_update_cache,
        },
    },
    error::{ReceiveError, SendError},
};

mod handler;
mod message_cache;
mod old_session;
mod utils;

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::network::core::core::behaviour";

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
/// The public header signature and uniqueness of incoming messages is validated according to the [Blend v1 specification](https://www.notion.so/nomos-tech/Blend-Protocol-Version-1-215261aa09df81ae8857d71066a80084) before the message is propagated to the swarm and to the Blend service.
pub struct Behaviour<ObservationWindowClockProvider> {
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
    /// Cache of the messages that have been processed/forwarded by this node,
    /// to avoid processing the same message multiple times and being marked
    /// as malicious by our peers.
    message_cache: MessageCache,
    observation_window_clock_provider: ObservationWindowClockProvider,
    current_session_info: (Membership<PeerId>, u64),
    /// The [minimum, maximum] peering degree of this node.
    peering_degree: RangeInclusive<usize>,
    local_peer_id: PeerId,
    protocol_name: StreamProtocol,
    /// The minimum Blend network size for messages to be relayed between peers.
    minimum_network_size: NonZeroUsize,
    /// States for processing messages from the old session
    /// before the transition period has passed.
    old_session: Option<OldSession>,
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
    InvalidHeaderSignature,
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
pub enum ConnectionUpgradeFailureReason {
    /// The node has the reached the maximum peering degree, which prevents new
    /// connections from being established.
    MaximumPeeringDegreeReached,
    /// The node has tried to establish a new connection with a peer it already
    /// has a connection in the same direction.
    DuplicateConnection,
    /// The node has tried to establish a new connection with a peer, but the
    /// reverse direction is preferred, according to the Blend specification.
    ReverseDirectionPreferred,
    /// A failure happened during the connection upgrade that is not covered by
    /// any of the above cases.
    ConnectionFailure,
}

#[derive(Debug)]
struct ConnectionUpgradeFailure {
    remote_peer_role: Endpoint,
    reason: ConnectionUpgradeFailureReason,
}

#[derive(Debug)]
pub enum Event {
    /// A message received from one of the core peers, after its public header
    /// signature has been verified.
    Message {
        message: Box<EncapsulatedMessageWithVerifiedSignature>,
        sender: PeerId,
        session: u64,
    },
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
    OutboundConnectionUpgradeFailed {
        peer: PeerId,
        reason: ConnectionUpgradeFailureReason,
    },
    /// An inbound connection failed to be upgraded, meaning the peer is a
    /// remote core but something failed when negotiating Blend protocol
    /// support.
    InboundConnectionUpgradeFailed {
        peer: PeerId,
        reason: ConnectionUpgradeFailureReason,
    },
}

impl<ObservationWindowClockProvider> Behaviour<ObservationWindowClockProvider> {
    #[must_use]
    pub fn new(
        config: &Config,
        observation_window_clock_provider: ObservationWindowClockProvider,
        session_info: (Membership<PeerId>, u64),
        local_peer_id: PeerId,
        protocol_name: StreamProtocol,
    ) -> Self {
        Self {
            negotiated_peers: HashMap::with_capacity(*config.peering_degree.end()),
            events: VecDeque::new(),
            waker: None,
            observation_window_clock_provider,
            message_cache: MessageCache::new_with_peer_capacity(session_info.0.size()),
            current_session_info: session_info,
            peering_degree: config.peering_degree.clone(),
            connections_waiting_upgrade: HashMap::new(),
            local_peer_id,
            protocol_name,
            minimum_network_size: config.minimum_network_size,
            old_session: None,
        }
    }

    pub(crate) fn start_new_session(&mut self, new_session_info: (Membership<PeerId>, u64)) {
        let current_session_number = self.current_session_info.1;

        self.connections_waiting_upgrade.clear();
        self.current_session_info = new_session_info;

        self.stop_old_session();

        self.old_session = Some(OldSession::new(
            mem::take(&mut self.negotiated_peers)
                .into_iter()
                .map(|(peer_id, details)| (peer_id, details.connection_id))
                .collect(),
            mem::take(&mut self.message_cache),
            current_session_number,
        ));

        tracing::debug!(target: LOG_TARGET, "Started a new session by passing negotiated peers and exchanged message IDs to the old session. Now, no negotiated peers in the current session.");
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
    ) -> Result<(), SendError> {
        let serialized_message =
            lb_blend_scheduling::serialize_encapsulated_message_with_verified_public_header(
                message,
            );
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
    ) -> Result<(), SendError> {
        let Some(RemotePeerConnectionDetails { connection_id, .. }) =
            self.negotiated_peers.get(&peer_id)
        else {
            return Err(SendError::NoPeers);
        };
        tracing::trace!(
            target: LOG_TARGET,
            "Notifying handler with peer {peer_id:?} on connection {connection_id:?} to deliver already-serialized message."
        );
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(serialized_message)),
        });
        self.try_wake();
        Ok(())
    }

    pub const fn negotiated_peers(&self) -> &HashMap<PeerId, RemotePeerConnectionDetails> {
        &self.negotiated_peers
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

    fn notify_about_connection_upgrade_failure(
        &mut self,
        peer_id: PeerId,
        ConnectionUpgradeFailure {
            reason,
            remote_peer_role,
        }: ConnectionUpgradeFailure,
    ) {
        let event = if remote_peer_role == Endpoint::Dialer {
            Event::InboundConnectionUpgradeFailed {
                peer: peer_id,
                reason,
            }
        } else {
            Event::OutboundConnectionUpgradeFailed {
                peer: peer_id,
                reason,
            }
        };
        self.events.push_back(ToSwarm::GenerateEvent(event));
        self.try_wake();
    }

    fn notify_about_connection_upgrade_success(
        &mut self,
        peer_id: PeerId,
        remote_peer_role: Endpoint,
    ) {
        self.events.push_back(ToSwarm::GenerateEvent(
            if remote_peer_role == Endpoint::Listener {
                Event::OutboundConnectionUpgradeSucceeded(peer_id)
            } else {
                Event::InboundConnectionUpgradeSucceeded(peer_id)
            },
        ));
        self.try_wake();
    }

    fn is_network_large_enough(&self) -> bool {
        self.current_session_info.0.size() >= self.minimum_network_size.get()
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
        remote_peer_role: Endpoint,
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
            self.notify_about_connection_upgrade_failure(
                peer_id,
                ConnectionUpgradeFailure {
                    reason: ConnectionUpgradeFailureReason::MaximumPeeringDegreeReached,
                    remote_peer_role,
                },
            );
            return;
        }
        debug_assert!(
            !self.negotiated_peers.contains_key(&peer_id),
            "We are assuming the peer is not connected to us."
        );
        tracing::trace!(
            target: LOG_TARGET,
            "Connection {connection_id:?} with peer {peer_id:?} has been negotiated."
        );
        self.negotiated_peers.insert(
            peer_id,
            RemotePeerConnectionDetails {
                role: remote_peer_role,
                negotiated_state: NegotiatedPeerState::Healthy,
                connection_id,
            },
        );
        // Notify the Swarm about the successful negotiation.
        self.notify_about_connection_upgrade_success(peer_id, remote_peer_role);
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
        new_remote_peer_role: Endpoint,
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
        match (existing_connection.role, new_remote_peer_role) {
            // Same connection direction (in case it was not caught at connection establishment
            // time), we ignore the new connection.
            (Endpoint::Dialer, Endpoint::Dialer) | (Endpoint::Listener, Endpoint::Listener) => {
                self.handle_connected_peer_duplicate_connection(
                    (peer_id, new_connection_id),
                    new_remote_peer_role,
                );
            }
            (Endpoint::Listener, Endpoint::Dialer) | (Endpoint::Dialer, Endpoint::Listener) => {
                self.handle_connected_peer_reverse_connection(
                    (peer_id, new_connection_id),
                    new_remote_peer_role,
                );
            }
        }
    }

    /// Close the new connection since there is already an established one in
    /// the same direction.
    fn handle_connected_peer_duplicate_connection(
        &mut self,
        (peer_id, new_connection_id): (PeerId, ConnectionId),
        new_remote_peer_role: Endpoint,
    ) {
        tracing::trace!(target: LOG_TARGET, "Connection {new_connection_id:?} with peer {peer_id:?} will be closed since there is already a connection established in the same direction.");
        self.close_connection((peer_id, new_connection_id));
        self.notify_about_connection_upgrade_failure(
            peer_id,
            ConnectionUpgradeFailure {
                reason: ConnectionUpgradeFailureReason::DuplicateConnection,
                remote_peer_role: new_remote_peer_role,
            },
        );
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
        new_remote_peer_role: Endpoint,
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
            let existing_role = existing_connection_details.role;
            self.close_connection(existing_connection);
            self.notify_about_connection_upgrade_success(peer_id, existing_role);
        } else {
            tracing::trace!(target: LOG_TARGET, "Dropping upgraded connection {new_connection_id:?} with peer {peer_id:?} in favor of currently established connection {:?}", existing_connection_details.connection_id);
            // Notify the new connection handler to drop the substreams, and we do not
            // alter the storage.
            self.close_connection((peer_id, new_connection_id));
            self.notify_about_connection_upgrade_failure(
                peer_id,
                ConnectionUpgradeFailure {
                    reason: ConnectionUpgradeFailureReason::ReverseDirectionPreferred,
                    remote_peer_role: new_remote_peer_role,
                },
            );
        }
    }

    /// Mark the connection with the sender of a malformed message as malicious
    /// and instruct its connection handler to drop the substream.
    fn close_spammy_connection(
        &mut self,
        (peer_id, connection_id): (PeerId, ConnectionId),
        reason: SpamReason,
    ) {
        tracing::debug!(
            target: LOG_TARGET,
            "Closing connection {connection_id:?} with spammy peer {peer_id:?} for reason {reason:?}."
        );
        self.set_connection_to_spammy((peer_id, connection_id), reason);
        self.close_connection((peer_id, connection_id));
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
            tracing::trace!(
                target: LOG_TARGET,
                "Provided connection ID {connection_id:?} does not match the stored connection ID {:?} for peer {peer_id:?}. Ignoring state update.",
                peer_details.connection_id
            );
            return Some(state);
        }
        Some(mem::replace(&mut peer_details.negotiated_state, state))
    }

    /// Handle an unhealthy connection if it exists in the current session.
    /// If not, it is ignored.
    #[expect(
        dead_code,
        reason = "TODO: We currently do not handle unhealthy cases."
    )]
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

    /// Publish an already-encapsulated and validated message to all connected
    /// peers in the specified session.
    pub fn publish_message_with_validated_header(
        &mut self,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
        intended_session: u64,
    ) -> Result<(), SendError> {
        if self.current_session_info.1 != intended_session {
            let Some(old_session) = &mut self.old_session else {
                return Err(SendError::InvalidSession);
            };
            return old_session.publish_message_with_validated_header(message, intended_session);
        }
        self.forward_maybe_excluding(&message.into(), None)
    }

    /// Publish an already-encapsulated message with a valid public header
    /// signature to all connected peers in the current session.
    pub fn publish_message_with_validated_signature_to_current_session(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedSignature,
    ) -> Result<(), SendError> {
        self.forward_maybe_excluding(message, None)
    }

    /// Forwards a message with a valid public header signature to all
    /// non-spammy peers in the specified session, except the
    /// [`except`] peer.
    ///
    /// If the session is the previous session, the message is forwarded to the
    /// peers in the old session. Otherwise, it is forwarded to the peers in
    /// the current session.
    ///
    /// Returns [`Error::NoPeers`] if there are no connected peers that support
    /// the blend protocol, and [`Error::InvalidSession`] if the provided
    /// session does not match neither the current session nor the old session.
    pub fn forward_message_with_validated_signature(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedSignature,
        except: PeerId,
        intended_session: u64,
    ) -> Result<(), SendError> {
        if self.current_session_info.1 != intended_session {
            let Some(old_session) = &mut self.old_session else {
                return Err(SendError::InvalidSession);
            };
            return old_session.forward_message_with_validated_signature(
                message,
                except,
                intended_session,
            );
        }

        self.forward_maybe_excluding(message, Some(except))
    }

    fn forward_maybe_excluding(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedSignature,
        excluded_peer: Option<PeerId>,
    ) -> Result<(), SendError> {
        tracing::trace!(
            "Forwarding message with id {:?} to current session peers. Negotiated peers: {:?}. Excluded peer: {excluded_peer:?}",
            hex::encode(message.id()),
            self.negotiated_peers()
        );

        forward_validated_message_and_update_cache(
            message,
            self.negotiated_peers
                .iter()
                // Exclude the peer the message was received from.
                .filter(|(peer_id, _)| excluded_peer != Some(**peer_id))
                // Exclude from the list of candidates spammy peers.
                .filter(|(_, peer_state)| !peer_state.negotiated_state.is_spammy())
                // Take only the connection ID, which the inner function requires.
                .map(
                    |(peer_id, RemotePeerConnectionDetails { connection_id, .. })| {
                        (peer_id, connection_id)
                    },
                ),
            &mut self.events,
            &mut self.message_cache,
            self.waker.take(),
        )
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
                    tracing::debug!(target: LOG_TARGET, "Failed to handle message from the old session: {e:?}");
                    return;
                }
            }
        }

        if let Err(receive_error) = handle_received_serialized_encapsulated_message_and_update_cache(
            serialized_message,
            &mut self.message_cache,
            from_peer_id,
            &mut self.events,
            self.waker.take(),
            self.current_session_info.1,
        ) {
            tracing::debug!(target: LOG_TARGET, "Failed to handle message from the current session: {receive_error:?}");
            let spam_reason = match receive_error {
                ReceiveError::DuplicateMessageFromPeer(_) => SpamReason::DuplicateMessage,
                ReceiveError::InvalidHeaderSignature => SpamReason::InvalidHeaderSignature,
                ReceiveError::UndeserializableMessage => SpamReason::UndeserializableMessage,
            };
            self.close_spammy_connection((from_peer_id, from_connection_id), spam_reason);
        }
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

impl<ObservationWindowClockProvider> NetworkBehaviour for Behaviour<ObservationWindowClockProvider>
where
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
        } else if self.current_session_info.0.contains(&peer_id) {
            tracing::trace!(
                target: LOG_TARGET,
                "Upgrading inbound connection {connection_id:?} with core peer {peer_id:?}."
            );
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
        } else if self.current_session_info.0.contains(&peer_id) {
            tracing::trace!(
                target: LOG_TARGET,
                "Upgrading outbound connection {connection_id:?} with core peer {peer_id:?}."
            );
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
                self.notify_about_connection_upgrade_failure(
                    peer_id,
                    ConnectionUpgradeFailure {
                        reason: ConnectionUpgradeFailureReason::ConnectionFailure,
                        remote_peer_role,
                    },
                );
                return;
            }

            let Entry::Occupied(peer_details_entry) = self.negotiated_peers.entry(peer_id) else {
                // This event was not meant for us.
                return;
            };

            let negotiated_connection_id = peer_details_entry.get().connection_id;

            if negotiated_connection_id == connection_id {
                let negotiated_peer_details = peer_details_entry.remove();
                self.message_cache.remove_peer_info(&peer_id);
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
                // TODO: Re-add logic once Blend observation window values calculation is fixed.
                ToBehaviour::SpammyPeer => {
                    // We do not explicitly close the connection here since the
                    // connection handler will already do
                    // that for us.
                    // self.set_connection_to_spammy(
                    //     (peer_id, connection_id),
                    //     SpamReason::TooManyMessages,
                    // );
                    tracing::debug!(
                        target: LOG_TARGET,
                        "Peer {peer_id:?} has been marked as spammy by its connection handler. NOT TAKING ANY ACTIONS ON THIS."
                    );
                }
                // TODO: Re-add logic once Blend observation window values calculation is fixed.
                ToBehaviour::UnhealthyPeer => {
                    // self.handle_unhealthy_connection((peer_id,
                    // connection_id));
                    tracing::trace!(
                        target: LOG_TARGET,
                        "Peer {peer_id:?} has been marked as unhealthy by its connection handler. NOT TAKING ANY ACTIONS ON THIS."
                    );
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
