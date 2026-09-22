use core::{
    fmt::{self, Display, Formatter},
    mem::{self},
    num::{NonZeroU64, NonZeroU128, NonZeroUsize},
    time::Duration,
};
use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    convert::Infallible,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use either::Either;
use futures::StreamExt as _;
use lb_blend_membership::Membership;
use lb_blend_message::encap::{
    ProofsVerifier as ProofsVerifierTrait, encapsulated_message_encoded_size,
    validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_blend_primitives::time::{Round, RoundClock, RoundCount};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
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
    CommonConfig,
    poq_verification::{PendingPoQVerifications, PoQVerificationOutcome},
    with_core::{
        behaviour::{
            blacklist::{BlacklistReason, PeerBlacklist},
            handler::{ConnectionHandler, FromBehaviour, ToBehaviour},
            liveness::PeerLivenessMap,
            message_cache::MessageCache,
            old_epoch::OldEpoch,
            utils::{
                forward_validated_message_and_update_cache,
                handle_received_serialized_encapsulated_message,
            },
        },
        error::SendError,
    },
};

pub mod blacklist;

pub(crate) mod liveness;

mod handler;
mod message_cache;
mod old_epoch;
mod utils;

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::network::core::core::BEHAVIOUR;

/// The blacklist holds `2·Φ_CC` peers: enough to exclude a whole peering's
/// worth of offenders twice over.
const BLACKLIST_TARGET_PEERING_DEGREE_MULTIPLIER: NonZeroUsize = NonZeroUsize::new(2).unwrap();

#[derive(Debug)]
pub struct Config {
    /// `Φ_CC`: the peering degree of this node.
    pub target_peering_degree: NonZeroUsize,
    /// `W`: the observation window, in rounds. A connection whose neighbour has
    /// delivered nothing within the trailing window is closed.
    pub liveness_window_in_rounds: NonZeroU128,
    /// `r₁`: the messages a core connection may carry in one round, in each
    /// direction.
    pub connection_share_per_round: NonZeroU64,
    /// `η`: how long a message may wait for a connection before that
    /// connection gives up on it.
    pub send_deadline_in_rounds: RoundCount,
    /// `T_H`: how long a handshake with a core node is given to complete
    /// before the connection is abandoned and its degree slot released.
    pub handshake_deadline_in_rounds: RoundCount,
}

/// A connection established but not yet negotiated for the Blend protocol.
#[derive(Debug, Clone, Copy)]
struct PendingUpgrade {
    /// Which side opened it.
    direction: ConnectionDirection,
    /// The round the handshake began, which `T_H` is measured from.
    started_at: Round,
}

/// How long libp2p is given to complete a substream upgrade.
///
/// It's derived from `T_H` plus a few rounds on top to ensure our logic always
/// fires first, and we don't let libp2p handle this instead, as we need to keep
/// track of stale handshakes.
fn handshake_upgrade_timeout(
    round_duration_in_seconds: NonZeroU64,
    handshake_deadline: RoundCount,
) -> Duration {
    const ROUNDS_BEYOND_THE_DEADLINE: u64 = 5;

    let deadline_in_rounds = u64::try_from(handshake_deadline.get()).unwrap_or(u64::MAX);
    Duration::from_secs(
        deadline_in_rounds
            .saturating_add(ROUNDS_BEYOND_THE_DEADLINE)
            .saturating_mul(round_duration_in_seconds.get()),
    )
}

/// Who opened a connection, from this node's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    /// This node dialed the peer.
    Outgoing,
    /// The peer dialed this node.
    Incoming,
}

impl ConnectionDirection {
    /// The one place libp2p's convention is decoded: the swarm reports the
    /// endpoint of an established connection from the *local* side.
    #[must_use]
    const fn from_local_endpoint(local: Endpoint) -> Self {
        match local {
            Endpoint::Dialer => Self::Outgoing,
            Endpoint::Listener => Self::Incoming,
        }
    }

    #[must_use]
    const fn is_outgoing(self) -> bool {
        matches!(self, Self::Outgoing)
    }

    #[must_use]
    const fn is_incoming(self) -> bool {
        matches!(self, Self::Incoming)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RemotePeerConnectionDetails {
    /// Which side opened this connection.
    direction: ConnectionDirection,
    /// The ID of the connection with the peer.
    connection_id: ConnectionId,
}

impl RemotePeerConnectionDetails {
    #[must_use]
    pub const fn direction(&self) -> ConnectionDirection {
        self.direction
    }

    #[must_use]
    pub const fn connection_id(&self) -> ConnectionId {
        self.connection_id
    }
}

/// A [`NetworkBehaviour`] that processes incoming Blend messages, and
/// propagates messages from the Blend service to the rest of the Blend network.
///
/// The public header signature and uniqueness of incoming messages is validated according to the [Blend specification](https://lip.logos.co/blockchain/raw/blend-protocol.html) before the message is propagated to the swarm and to the Blend service.
pub struct Behaviour<ProofsVerifier> {
    /// Tracks connections between this node and other core nodes.
    ///
    /// Only connections with other core nodes that are established before the
    /// specified connection limit is reached will be upgraded and the state of
    /// the peer negotiated and reported to the swarm.
    negotiated_peers: HashMap<PeerId, RemotePeerConnectionDetails>,
    /// The set of connections established but not yet upgraded.
    ///
    /// We use this to keep track of the connection direction (outgoing or
    /// incoming), to be used when deciding which connection to close when a
    /// duplicate connection to the same peer is detected.
    connections_waiting_upgrade: HashMap<(PeerId, ConnectionId), PendingUpgrade>,
    /// Queue of events to yield to the swarm.
    events: VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    /// Waker that handles polling
    waker: Option<Waker>,
    /// Cache of the messages that have been processed/forwarded by this node,
    /// to avoid processing the same message multiple times and being marked
    /// as malicious by our peers.
    message_cache: MessageCache,
    current_epoch_info: (Membership<PeerId>, Epoch),
    /// Verifier for the `PoQ`s of the messages received in the current epoch.
    ///
    /// Shared rather than owned because a handle to it is passed to the
    /// blocking pool for every message received.
    proofs_verifier: Arc<ProofsVerifier>,
    /// `PoQ` verifications currently running on the blocking pool, for messages
    /// of either the current or the outgoing epoch.
    pending_poq_verifications: PendingPoQVerifications,
    /// `Φ_CC`: the peering degree this node maintains with other core nodes.
    target_peering_degree: NonZeroUsize,
    local_peer_id: PeerId,
    protocol_name: StreamProtocol,
    /// The minimum Blend network size for messages to be relayed between peers.
    minimum_network_size: NonZeroUsize,
    /// `ß_c`: the fixed number of encapsulation layers every well-formed Blend
    /// message carries.
    num_blend_layers: NonZeroU64,
    /// States for processing messages from the old epoch
    /// before the transition period has passed.
    old_epoch: Option<OldEpoch<ProofsVerifier>>,
    /// The clock every window and deadline of connectivity maintenance is
    /// measured against.
    round_clock: RoundClock,
    /// The round this behaviour is currently in, read from the clock at the top
    /// of every poll.
    current_round: Round,
    /// `r₁`: what every connection of this node may carry in a round.
    connection_share_per_round: NonZeroU64,
    /// `η`: how long a message may wait for a connection.
    send_deadline: RoundCount,
    /// `T_H`: how long a handshake is given to complete.
    handshake_deadline: RoundCount,
    /// The outer bound libp2p puts on a substream upgrade, derived from `T_H`
    /// so that the sweep above is always the one to act first.
    handshake_upgrade_timeout: Duration,
    /// Which neighbours are still delivering messages.
    liveness: PeerLivenessMap,
    /// The peers this node refuses to exchange Blend messages with, for a
    /// while, because they sent something no honest node would have.
    blacklist: PeerBlacklist,
    /// When this node last dropped below the connections the spec asks it to
    /// hold, if it is still below them.
    below_target_degree_since: Option<Round>,
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
    /// The handshake did not complete within `T_H`, so the connection was
    /// abandoned and the degree slot it held released.
    HandshakeTimedOut,
    /// A failure happened during the connection upgrade that is not covered by
    /// any of the above cases.
    ConnectionFailure,
    /// This node declined to speak Blend on the connection: the peer is
    /// blacklisted, or it is not a core node of the current epoch, or the
    /// network is too small for this node to peer at all. Dialing the same
    /// peer again does not fix any of them.
    Refused,
}

/// Why this node is dropping a connection.
#[derive(Debug, Clone, Copy)]
enum CloseReason {
    /// The epoch the connection belonged to is over.
    EpochOver,
    /// The neighbour delivered nothing within the observation window.
    NotLive,
    /// The handshake did not finish within `T_H`.
    HandshakeDeadlineMissed,
    /// This node is already holding as many connections as it may.
    NoRoomLeft,
    /// A second connection in the same direction with a peer this node is
    /// already connected to.
    AlreadyConnected,
    /// The connection with this peer in the other direction is the one to keep.
    ReverseDirectionPreferred,
    /// This node refuses to exchange Blend messages with the peer.
    PeerBlacklisted,
}

impl Display for CloseReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EpochOver => "the epoch it belongs to is over",
            Self::NotLive => "the neighbour delivered nothing within the observation window",
            Self::HandshakeDeadlineMissed => "the handshake did not finish within `T_H`",
            Self::NoRoomLeft => "this node is already at its peering degree",
            Self::AlreadyConnected => {
                "there is already a connection with this peer in the same direction"
            }
            Self::ReverseDirectionPreferred => {
                "the connection with this peer in the other direction is the one to keep"
            }
            Self::PeerBlacklisted => "the peer is blacklisted",
        })
    }
}

#[derive(Debug)]
struct ConnectionUpgradeFailure {
    direction: ConnectionDirection,
    reason: ConnectionUpgradeFailureReason,
}

#[derive(Debug)]
pub enum Event {
    /// A message received from one of the core peers, after its whole public
    /// header — signature and `PoQ` — has been verified.
    Message {
        message: Box<EncapsulatedMessageWithVerifiedPublicHeader>,
        sender: PeerId,
        epoch: Epoch,
    },
    /// A connection with a peer has dropped.
    PeerDisconnected(PeerId),
    /// A malicious peer has been detected and blacklisted.
    PeerBlacklisted {
        peer: PeerId,
        reason: BlacklistReason,
    },
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

impl<ProofsVerifier> Behaviour<ProofsVerifier> {
    #[must_use]
    pub fn new(
        (common_config, core_config): (&CommonConfig, &Config),
        epoch_info: (Membership<PeerId>, Epoch),
        proofs_verifier: ProofsVerifier,
        local_peer_id: PeerId,
        round_clock: RoundClock,
        protocol_name: StreamProtocol,
    ) -> Self {
        let current_round = round_clock.current_round();
        Self {
            negotiated_peers: HashMap::with_capacity(core_config.target_peering_degree.get() + 1),
            events: VecDeque::new(),
            waker: None,
            message_cache: MessageCache::new(),
            current_epoch_info: epoch_info,
            proofs_verifier: Arc::new(proofs_verifier),
            pending_poq_verifications: PendingPoQVerifications::new(),
            target_peering_degree: core_config.target_peering_degree,
            connections_waiting_upgrade: HashMap::new(),
            local_peer_id,
            protocol_name,
            minimum_network_size: common_config.minimum_network_size,
            num_blend_layers: common_config.num_blend_layers,
            old_epoch: None,
            round_clock,
            current_round,
            connection_share_per_round: core_config.connection_share_per_round,
            send_deadline: core_config.send_deadline_in_rounds,
            handshake_deadline: core_config.handshake_deadline_in_rounds,
            handshake_upgrade_timeout: handshake_upgrade_timeout(
                common_config.round_duration_in_seconds,
                core_config.handshake_deadline_in_rounds,
            ),
            liveness: PeerLivenessMap::new(RoundCount::new(core_config.liveness_window_in_rounds)),
            below_target_degree_since: None,
            blacklist: PeerBlacklist::new(
                core_config
                    .target_peering_degree
                    .checked_mul(BLACKLIST_TARGET_PEERING_DEGREE_MULTIPLIER)
                    .expect("Blacklist capacity overflowed `usize`."),
                RoundCount::new(core_config.liveness_window_in_rounds),
            ),
        }
    }

    pub(crate) fn start_new_epoch(
        &mut self,
        new_epoch_info: (Membership<PeerId>, Epoch),
        new_proofs_verifier: ProofsVerifier,
    ) {
        let current_epoch_number = self.current_epoch_info.1;

        // Close any connections that were still waiting to be upgraded: they
        // belong to the epoch we are leaving and must not be carried over. A
        // `FullyNegotiated` event for one of these may still be in flight from
        // its handler; `handle_negotiated_connection` ignores such stale events
        // since the entry is no longer pending here.
        let pending_upgrades = mem::take(&mut self.connections_waiting_upgrade);
        for (connection, _) in pending_upgrades {
            self.close_connection(connection, CloseReason::EpochOver);
        }
        self.current_epoch_info = new_epoch_info;
        let current_epoch_proofs_verifier =
            mem::replace(&mut self.proofs_verifier, Arc::new(new_proofs_verifier));

        self.stop_old_epoch();

        self.old_epoch = Some(OldEpoch::new(
            mem::take(&mut self.negotiated_peers)
                .into_iter()
                .map(|(peer_id, details)| (peer_id, details.connection_id))
                .collect(),
            mem::take(&mut self.message_cache),
            current_epoch_number,
            self.num_blend_layers,
            current_epoch_proofs_verifier,
        ));

        // The observations were collected against the membership of the epoch
        // that just ended, so they do not carry over.
        self.liveness.clear();

        tracing::debug!(target: LOG_TARGET, "Started a new epoch by passing negotiated peers and exchanged message IDs to the old epoch. Now, no negotiated peers in the current epoch.");
    }

    pub(crate) fn finish_epoch_transition(&mut self) {
        self.stop_old_epoch();
    }

    fn stop_old_epoch(&mut self) {
        if let Some(old_epoch) = self.old_epoch.take() {
            let mut events = old_epoch.stop();
            let num_events = events.len();
            self.events.append(&mut events);
            if num_events > 0 {
                self.try_wake();
            }
        }
    }

    /// `Φ_CC - 1`: the fewest live connections the node settles for.
    #[must_use]
    const fn minimum_live_peers(&self) -> usize {
        self.target_peering_degree.get().saturating_sub(1)
    }

    /// `Φ_CC - 2`: the fewest live connections the node must have opened
    /// itself.
    #[must_use]
    const fn minimum_live_dialed_peers(&self) -> usize {
        self.target_peering_degree.get().saturating_sub(2)
    }

    /// `Φ_CC + 1`: the most connections with core nodes the node holds at once.
    #[must_use]
    pub const fn maximum_peers(&self) -> usize {
        self.target_peering_degree.get().saturating_add(1)
    }

    /// `(Φ_CC + 1) - (Φ_CC - 2)`: the most connections the node accepts.
    #[must_use]
    const fn maximum_accepted_peers(&self) -> usize {
        self.maximum_peers() - self.minimum_live_dialed_peers()
    }

    fn live_peers(&self) -> impl Iterator<Item = (&PeerId, &RemotePeerConnectionDetails)> {
        self.negotiated_peers
            .iter()
            .filter(move |(peer_id, _)| !self.liveness.is_connection_unhealthy(peer_id))
    }

    pub fn num_live_peers(&self) -> usize {
        self.live_peers().count()
    }

    /// Of the live connections, the ones this node opened itself.
    #[must_use]
    fn num_live_dialed_peers(&self) -> usize {
        self.live_peers()
            .filter(|(_, details)| details.direction.is_outgoing())
            .count()
    }

    /// The connections other nodes opened to this one, live or not, and
    /// counting those still shaking hands.
    ///
    /// A handshake in progress holds a slot exactly as a negotiated connection
    /// does, so leaving pending ones out here would let peers fill every slot
    /// with handshakes they never complete. The node would then be at its
    /// maximum and open nothing itself, which is what the floor of `Φ_CC - 2`
    /// self-opened connections exists to prevent.
    #[must_use]
    fn num_total_accepted_peers(&self) -> usize {
        let negotiated = self
            .negotiated_peers
            .values()
            .filter(|details| details.direction.is_incoming())
            .count();
        let waiting_upgrade = self
            .connections_waiting_upgrade
            .values()
            .filter(|pending| pending.direction.is_incoming())
            .count();

        negotiated.saturating_add(waiting_upgrade)
    }

    #[must_use]
    pub fn num_negotiated_peers(&self) -> usize {
        self.negotiated_peers.len()
    }

    /// How many connections the node should open right now.
    ///
    /// The node opens while it holds fewer than `Φ_CC - 1` live connections,
    /// **or** fewer than `Φ_CC - 2` live ones that it opened itself, as per the
    /// spec.
    #[must_use]
    pub fn connections_to_open(&self) -> usize {
        let live_shortfall = self
            .minimum_live_peers()
            .saturating_sub(self.num_live_peers());
        let dialed_shortfall = self
            .minimum_live_dialed_peers()
            .saturating_sub(self.num_live_dialed_peers());

        live_shortfall
            .max(dialed_shortfall)
            .min(self.available_connection_slots())
    }

    /// The connections the node could still hold before reaching `Φ_CC + 1`.
    /// How many more connections with core nodes this node has room for.
    ///
    /// A handshake in progress holds a slot just as a negotiated connection
    /// does.
    #[must_use]
    pub fn available_connection_slots(&self) -> usize {
        self.maximum_peers()
            .saturating_sub(self.negotiated_peers.len())
            .saturating_sub(self.connections_waiting_upgrade.len())
    }

    #[must_use]
    fn can_accept_connection(&self) -> bool {
        self.available_connection_slots() > 0
            && self.num_total_accepted_peers() < self.maximum_accepted_peers()
    }

    /// Force send a message to a peer, as long as the peer is connected, no
    /// matter the state the connection is in.
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn force_send_message_to_current_epoch_peer(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        peer_id: PeerId,
    ) -> Result<(), SendError> {
        self.force_send_message_to_peer_at_epoch(message, peer_id, self.current_epoch_info.1)
    }

    /// Force send a message to a peer, as long as the peer is connected, no
    /// matter the state the connection is in.
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    fn force_send_message_to_peer_at_epoch(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        peer_id: PeerId,
        epoch: Epoch,
    ) -> Result<(), SendError> {
        let serialized_message =
            lb_blend_message::serialize_encapsulated_message_with_verified_public_header(message);
        self.force_send_serialized_message_to_peer_at_epoch(&serialized_message, peer_id, epoch)
    }

    /// Force send a serialized message to a peer (without trying to deserialize
    /// nor validating it first), as long as the peer is connected, no
    /// matter the state the connection is in.
    #[cfg(test)]
    fn force_send_serialized_message_to_current_epoch_peer(
        &mut self,
        serialized_message: &[u8],
        peer_id: PeerId,
    ) -> Result<(), SendError> {
        self.force_send_serialized_message_to_peer_at_epoch(
            serialized_message,
            peer_id,
            self.current_epoch_info.1,
        )
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn force_send_serialized_message_to_peer_at_epoch(
        &mut self,
        serialized_message: &[u8],
        peer_id: PeerId,
        epoch: Epoch,
    ) -> Result<(), SendError> {
        if epoch != self.current_epoch_info.1 {
            let Some(old_epoch) = &mut self.old_epoch else {
                return Err(SendError::InvalidEpoch);
            };
            return old_epoch.force_send_serialized_message_to_peer_at_epoch(
                serialized_message,
                peer_id,
                epoch,
            );
        }

        let Some(RemotePeerConnectionDetails { connection_id, .. }) =
            self.negotiated_peers.get(&peer_id)
        else {
            return Err(SendError::NoPeers);
        };

        tracing::trace!(
            target: LOG_TARGET,
            "Notifying handler with peer {peer_id:?} on current epoch connection {connection_id:?} to deliver already-serialized message."
        );
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(crate::OutgoingMessage::from_bytes(
                serialized_message,
            ))),
        });
        self.try_wake();
        Ok(())
    }

    pub const fn negotiated_peers(&self) -> &HashMap<PeerId, RemotePeerConnectionDetails> {
        &self.negotiated_peers
    }

    /// Returns the peer IDs of the old epoch's negotiated peers, if an
    /// epoch transition is in progress.
    pub fn old_epoch_peer_ids(&self) -> Option<impl Iterator<Item = &PeerId> + '_> {
        self.old_epoch.as_ref().map(OldEpoch::negotiated_peer_ids)
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
    fn close_connection(
        &mut self,
        (peer_id, connection_id): (PeerId, ConnectionId),
        reason: CloseReason,
    ) {
        tracing::debug!(
            target: LOG_TARGET,
            "Closing connection {connection_id:?} with peer {peer_id:?}: {reason}."
        );
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: Either::Left(FromBehaviour::CloseSubstreams),
        });
        self.try_wake();
    }

    /// Refuses a connection this node opened itself, and tells the swarm so.
    fn refuse_outgoing_connection_attempt(
        &mut self,
        peer_id: PeerId,
        reason: ConnectionUpgradeFailureReason,
    ) -> Either<ConnectionHandler, DummyConnectionHandler> {
        self.notify_about_connection_upgrade_failure(
            peer_id,
            ConnectionUpgradeFailure {
                reason,
                direction: ConnectionDirection::Outgoing,
            },
        );
        Either::Right(DummyConnectionHandler)
    }

    fn notify_about_connection_upgrade_failure(
        &mut self,
        peer_id: PeerId,
        ConnectionUpgradeFailure { reason, direction }: ConnectionUpgradeFailure,
    ) {
        let event = if direction.is_incoming() {
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
        direction: ConnectionDirection,
    ) {
        self.events
            .push_back(ToSwarm::GenerateEvent(if direction.is_outgoing() {
                Event::OutboundConnectionUpgradeSucceeded(peer_id)
            } else {
                Event::InboundConnectionUpgradeSucceeded(peer_id)
            }));
        self.try_wake();
    }

    fn is_network_large_enough(&self) -> bool {
        self.current_epoch_info.0.size() >= self.minimum_network_size.get()
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
    /// The handler emits [`ToBehaviour::FullyNegotiated`] at most once per
    /// connection and only for connections we chose to upgrade (i.e. handlers
    /// returned from the `Either::Left` branch of
    /// [`Self::handle_established_inbound_connection`]
    /// [`Self::handle_established_outbound_connection`]).
    ///
    /// Handler -> behaviour events are delivered asynchronously, so a handler
    /// can emit `FullyNegotiated` for a pending connection just before
    /// [`Self::start_new_epoch`] clears `connections_waiting_upgrade`, with the
    /// event delivered just after. In that case the entry is no longer pending
    /// (the connection is being closed by the epoch transition), so we simply
    /// ignore the stale event rather than acting on a connection that no longer
    /// belongs to the current epoch.
    fn handle_negotiated_connection(&mut self, (peer_id, connection_id): (PeerId, ConnectionId)) {
        let Some(PendingUpgrade {
            direction: new_connection_direction,
            ..
        }) = self
            .connections_waiting_upgrade
            .remove(&(peer_id, connection_id))
        else {
            tracing::debug!(
                target: LOG_TARGET,
                "Ignoring FullyNegotiated for connection ({peer_id:?}, {connection_id:?}) no longer pending upgrade (likely raced an epoch transition)."
            );
            return;
        };

        // A handshake that finished just as the peer was blacklisted must not be
        // taken up. The blacklisting already asked every connection with the peer
        // to close, but this one was past that point, so it has to be refused
        // here.
        if let Some(reason) = self.blacklisted_reason(&peer_id) {
            tracing::debug!(
                target: LOG_TARGET,
                "Connection {connection_id:?} finished negotiating with peer {peer_id:?} after it was blacklisted for {reason:?}."
            );
            self.close_connection((peer_id, connection_id), CloseReason::PeerBlacklisted);
            self.notify_about_connection_upgrade_failure(
                peer_id,
                ConnectionUpgradeFailure {
                    reason: ConnectionUpgradeFailureReason::Refused,
                    direction: new_connection_direction,
                },
            );
            return;
        }

        if self.negotiated_peers.contains_key(&peer_id) {
            self.handle_negotiated_connection_for_existing_peer(
                (peer_id, connection_id),
                new_connection_direction,
            );
        } else {
            self.handle_negotiated_connection_for_new_peer(
                (peer_id, connection_id),
                new_connection_direction,
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
        direction: ConnectionDirection,
    ) {
        // We need to check if we still have available connection slots, as it is
        // possible, especially upon epoch transition, that more than the maximum
        // allowed number of peers are trying to connect to us. So once the stream is
        // actually upgraded, we downgrade it again if we do not have space left for it.
        // By not adding the new connection to the map of negotiated peers, the swarm
        // will not be notified about this dropped connection, which is what we want.
        // Only an accepted connection can have lost its room since it was
        // admitted: this node opens one only when a slot is free, and holds
        // that slot for as long as the handshake runs.
        let has_room = match direction {
            ConnectionDirection::Incoming => self.can_accept_connection(),
            ConnectionDirection::Outgoing => true,
        };
        if !has_room {
            self.close_connection((peer_id, connection_id), CloseReason::NoRoomLeft);
            self.notify_about_connection_upgrade_failure(
                peer_id,
                ConnectionUpgradeFailure {
                    reason: ConnectionUpgradeFailureReason::MaximumPeeringDegreeReached,
                    direction,
                },
            );
            return;
        }
        tracing::trace!(
            target: LOG_TARGET,
            "Connection {connection_id:?} with peer {peer_id:?} has been negotiated."
        );
        self.negotiated_peers.insert(
            peer_id,
            RemotePeerConnectionDetails {
                direction,
                connection_id,
            },
        );
        self.liveness.start_or_resume_observing(peer_id);
        // Notify the Swarm about the successful negotiation.
        self.notify_about_connection_upgrade_success(peer_id, direction);
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
        new_direction: ConnectionDirection,
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
        if existing_connection.direction == new_direction {
            // Same connection direction (in case it was not caught at connection
            // establishment time), we ignore the new connection.
            self.handle_connected_peer_duplicate_connection(
                (peer_id, new_connection_id),
                new_direction,
            );
        } else {
            self.handle_connected_peer_reverse_connection(
                (peer_id, new_connection_id),
                new_direction,
            );
        }
    }

    /// Close the new connection since there is already an established one in
    /// the same direction.
    fn handle_connected_peer_duplicate_connection(
        &mut self,
        (peer_id, new_connection_id): (PeerId, ConnectionId),
        new_direction: ConnectionDirection,
    ) {
        self.close_connection((peer_id, new_connection_id), CloseReason::AlreadyConnected);
        self.notify_about_connection_upgrade_failure(
            peer_id,
            ConnectionUpgradeFailure {
                reason: ConnectionUpgradeFailureReason::DuplicateConnection,
                direction: new_direction,
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
        new_direction: ConnectionDirection,
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
        let should_close_established = if existing_connection_details.direction.is_incoming() {
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
            replace_connection(
                existing_connection_details,
                new_connection_id,
                new_direction,
            );
            // After the old connection details have been updated with the new
            // ones, notify the Swarm that the new connection has been upgraded.
            let existing_connection_direction = existing_connection_details.direction;
            self.close_connection(existing_connection, CloseReason::ReverseDirectionPreferred);
            self.notify_about_connection_upgrade_success(peer_id, existing_connection_direction);
        } else {
            // Notify the new connection handler to drop the substreams, and we do not
            // alter the storage.
            self.close_connection(
                (peer_id, new_connection_id),
                CloseReason::ReverseDirectionPreferred,
            );
            self.notify_about_connection_upgrade_failure(
                peer_id,
                ConnectionUpgradeFailure {
                    reason: ConnectionUpgradeFailureReason::ReverseDirectionPreferred,
                    direction: new_direction,
                },
            );
        }
    }

    /// Give up on every handshake that has taken longer than `T_H`.
    fn abandon_stale_handshakes(&mut self) {
        let deadline = self.handshake_deadline.get();
        let current_round = self.current_round;
        let stale_handshakes = self
            .connections_waiting_upgrade
            .iter()
            .filter_map(|(connection, pending_handshake)| {
                (current_round.rounds_since(pending_handshake.started_at) >= deadline)
                    .then_some((*connection, pending_handshake.direction))
            })
            .collect::<Vec<_>>();

        for ((peer_id, connection_id), direction) in stale_handshakes {
            self.connections_waiting_upgrade
                .remove(&(peer_id, connection_id));
            self.close_connection(
                (peer_id, connection_id),
                CloseReason::HandshakeDeadlineMissed,
            );
            self.notify_about_connection_upgrade_failure(
                peer_id,
                ConnectionUpgradeFailure {
                    reason: ConnectionUpgradeFailureReason::HandshakeTimedOut,
                    direction,
                },
            );
        }
    }

    /// Close the connection with every neighbour that has stopped delivering
    /// messages.
    fn close_unhealthy_connections(&mut self) {
        let unhealthy_connections = self
            .negotiated_peers
            .iter()
            .filter(|(peer_id, _)| self.liveness.is_connection_unhealthy(peer_id))
            .map(|(peer_id, details)| (*peer_id, details.connection_id))
            .collect::<Vec<_>>();

        for (peer_id, connection_id) in unhealthy_connections {
            self.close_connection((peer_id, connection_id), CloseReason::NotLive);
        }
    }

    /// Reports the peers this node has stopped refusing to deal with.
    fn prune_expired_blacklist_entries(&mut self) {
        drop(self.blacklist.prune_expired_entries(self.current_round));
    }

    /// Reports the node crossing into or out of holding fewer connections than
    /// the spec asks it to.
    fn check_and_report_low_peering_degree(&mut self) {
        let live = self.num_live_peers();
        let dialed = self.num_live_dialed_peers();
        let below_live = live < self.minimum_live_peers();
        let below_dialed = dialed < self.minimum_live_dialed_peers();

        match (self.below_target_degree_since, below_live || below_dialed) {
            (None, true) => {
                self.below_target_degree_since = Some(self.current_round);
                tracing::warn!(
                    target: LOG_TARGET,
                    "Holding fewer connections than the protocol asks for: {live} live of {} wanted, {dialed} of {} opened by this node.",
                    self.minimum_live_peers(),
                    self.minimum_live_dialed_peers()
                );
            }
            (Some(since), false) => {
                self.below_target_degree_since = None;
                tracing::info!(
                    target: LOG_TARGET,
                    "Back to the connections the protocol asks for after {} round(s): {live} live, {dialed} opened by this node.",
                    self.current_round.rounds_since(since)
                );
            }
            _ => {}
        }
    }

    /// Blacklists the sender of a message.
    fn blacklist_peer(&mut self, peer_id: PeerId, reason: BlacklistReason) {
        let outcome = self
            .blacklist
            .insert_or_extend(peer_id, reason, self.current_round);
        self.close_every_connection_with(&peer_id);

        // We need to not re-report the peer as blacklisted if it's just an extension of
        // an existing entry.
        if !outcome.is_first_offence() {
            return;
        }

        self.events
            .push_back(ToSwarm::GenerateEvent(Event::PeerBlacklisted {
                peer: peer_id,
                reason,
            }));
        self.try_wake();
    }

    /// Drops every connection this node holds with a peer: the negotiated one,
    /// any still shaking hands, and any left over from the previous epoch.
    fn close_every_connection_with(&mut self, peer_id: &PeerId) {
        let negotiated = self
            .negotiated_peers
            .get(peer_id)
            .map(|details| details.connection_id);
        let waiting_upgrade = self
            .connections_waiting_upgrade
            .keys()
            .filter(|(pending_peer, _)| pending_peer == peer_id)
            .map(|(_, connection_id)| *connection_id)
            .collect::<Vec<_>>();

        for connection_id in negotiated.into_iter().chain(waiting_upgrade) {
            self.close_connection((*peer_id, connection_id), CloseReason::PeerBlacklisted);
        }

        if let Some(old_epoch) = &mut self.old_epoch {
            old_epoch.close_connection_with_peer(peer_id);
            self.try_wake();
        }
    }

    /// The peers this node currently refuses to exchange Blend messages with.
    pub fn blacklisted_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.blacklist
            .entries(self.current_round)
            .map(|entry| &entry.peer)
    }

    /// The peers this node is part way through a handshake with, in either
    /// direction.
    ///
    /// The spec counts such a peer as a neighbour already, which is what keeps
    /// the node from drawing it at random and dialing it while it is being
    /// accepted. Two connections with one peer then have to be resolved by
    /// comparing identities, and until they are both hold a degree slot.
    pub fn peers_with_handshake_in_progress(&self) -> impl Iterator<Item = &PeerId> {
        self.connections_waiting_upgrade
            .keys()
            .map(|(peer_id, _)| peer_id)
    }

    /// Why this node currently refuses to deal with the peer, if it does.
    fn blacklisted_reason(&self, peer: &PeerId) -> Option<BlacklistReason> {
        self.blacklist.reason(peer, self.current_round)
    }

    /// Whether this node currently refuses to deal with the peer.
    #[must_use]
    pub fn is_peer_blacklisted(&self, peer: &PeerId) -> bool {
        self.blacklisted_reason(peer).is_some()
    }

    #[must_use]
    pub fn is_peer_unhealthy(&self, peer: &PeerId) -> bool {
        self.liveness.is_connection_unhealthy(peer)
    }

    /// Return `True` if this node has an established (negotiated or not)
    /// connection with the specified peer in the given direction.
    fn has_connection_with_peer(
        &self,
        remote_peer: &PeerId,
        direction: ConnectionDirection,
    ) -> bool {
        self.negotiated_peers
            .get(remote_peer)
            .is_some_and(|remote| remote.direction == direction)
            || self
                .connections_waiting_upgrade
                .iter()
                .any(|((peer_id, _), pending)| {
                    peer_id == remote_peer && pending.direction == direction
                })
    }

    /// Publish an already-encapsulated and validated message to all connected
    /// peers in the specified epoch.
    pub fn publish_message_with_validated_header(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        intended_epoch: Epoch,
    ) -> Result<(), SendError> {
        if self.current_epoch_info.1 != intended_epoch {
            let Some(old_epoch) = &mut self.old_epoch else {
                return Err(SendError::InvalidEpoch);
            };
            return old_epoch.publish_message_with_validated_header(message, intended_epoch);
        }
        self.forward_maybe_excluding(message, None)
    }

    /// Publish an already-encapsulated and validated message to all connected
    /// peers in the current epoch.
    pub fn publish_message_with_validated_header_to_current_epoch(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Result<(), SendError> {
        self.publish_message_with_validated_header(message, self.current_epoch_info.1)
    }

    /// Forwards a message with a verified public header to every neighbour in
    /// the specified epoch, other than `except` and any that are blacklisted.
    ///
    /// If the epoch is the previous epoch, the message is forwarded to the
    /// peers in the old epoch. Otherwise, it is forwarded to the peers in
    /// the current epoch.
    ///
    /// The input type is [`EncapsulatedMessageWithVerifiedPublicHeader`]
    /// because a message received from a peer is relayed only after the Blend
    /// service has verified its `PoQ`. The behaviour itself only verifies the
    /// public header signature, so it cannot produce such a value on its own.
    ///
    /// Returns [`SendError::NoPeers`] if there are no connected peers that
    /// support the blend protocol, and [`SendError::InvalidEpoch`] if the
    /// provided epoch matches neither the current epoch nor the old epoch.
    pub fn forward_message_with_verified_public_header(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        except: PeerId,
        intended_epoch: Epoch,
    ) -> Result<(), SendError> {
        if self.current_epoch_info.1 != intended_epoch {
            let Some(old_epoch) = &mut self.old_epoch else {
                return Err(SendError::InvalidEpoch);
            };
            return old_epoch.forward_message_with_verified_public_header(
                message,
                except,
                intended_epoch,
            );
        }

        self.forward_maybe_excluding(message, Some(except))
    }

    fn forward_maybe_excluding(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        excluded_peer: Option<PeerId>,
    ) -> Result<(), SendError> {
        let current_round = self.current_round;
        tracing::trace!(
            target: LOG_TARGET,
            "Forwarding message with id {:?} to current epoch peers. Negotiated peers: {:?}. Excluded peer: {excluded_peer:?}",
            hex::encode(fr_to_bytes(&message.id())),
            self.negotiated_peers()
        );

        forward_validated_message_and_update_cache(
            message,
            self.negotiated_peers
                .iter()
                // Exclude the peer the message was received from.
                .filter(|(peer_id, _)| excluded_peer != Some(**peer_id))
                // Exclude blacklisted peers.
                .filter(|(peer_id, _)| !self.blacklist.contains(peer_id, current_round))
                // Take only the connection ID, which the inner function requires.
                .map(
                    |(peer_id, RemotePeerConnectionDetails { connection_id, .. })| {
                        (peer_id, connection_id)
                    },
                ),
            &mut self.events,
            &mut self.message_cache,
            &mut self.waker,
        )
    }

    /// Acts on a completed `PoQ` verification: a message that verified is
    /// reported to the swarm, and a peer that could not prove its quota is
    /// blacklisted and disconnected, exactly like any other malicious peer this
    /// behaviour detects.
    fn handle_poq_verification_outcome(&mut self, outcome: PoQVerificationOutcome) {
        match outcome {
            PoQVerificationOutcome::Verified {
                message,
                sender,
                epoch,
            } => {
                // Only now that the `PoQ` has verified may the message claim its
                // nullifier in the cache, so a copy arriving later is not verified
                // again. It goes into the cache of the epoch it verified against.
                if epoch == self.current_epoch_info.1 {
                    self.message_cache.mark_message_as_processed(&message);
                } else if let Some(old_epoch) = &mut self.old_epoch
                    && epoch == old_epoch.epoch()
                {
                    old_epoch.mark_message_as_processed(&message);
                }
                self.events
                    .push_back(ToSwarm::GenerateEvent(Event::Message {
                        message,
                        sender,
                        epoch,
                    }));
            }
            // The connection it came in on is not singled out: blacklisting
            // drops every connection this node holds with the peer.
            PoQVerificationOutcome::Failed { sender, .. } => {
                self.blacklist_peer(sender, BlacklistReason::InvalidProofOfQuota);
            }
        }
    }
}

/// The part of the behaviour that needs to verify the `PoQ` of the messages it
/// receives, and so requires a usable verifier.
impl<ProofsVerifier> Behaviour<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + Send + Sync + 'static,
{
    /// Runs the relay checks on a frame received from a peer, and reports
    /// whether it counts toward that peer's liveness: only if the message was
    /// received from a connection of the current epoch, which is what the
    /// liveness map is keyed to.
    fn handle_received_serialized_encapsulated_message(
        &mut self,
        serialized_message: &[u8],
        (from_peer_id, from_connection_id): (PeerId, ConnectionId),
    ) -> bool {
        // First, try to handle the message in the context of the old epoch.
        // If it is not part of the old epoch, try with the current epoch.
        if let Some(old_epoch) = &mut self.old_epoch {
            match old_epoch.handle_received_serialized_encapsulated_message(
                serialized_message,
                (from_peer_id, from_connection_id),
                &self.pending_poq_verifications,
            ) {
                Ok(handled) => {
                    if handled {
                        return false;
                    }
                }
                // The old epoch closes the offending connection itself but does not interact with
                // the blacklist. We do that here, which also drops whatever
                // else this node holds with the peer.
                Err(receive_error) => {
                    self.blacklist_peer(from_peer_id, receive_error.into());
                    return false;
                }
            }
        }

        if let Err(receive_error) = handle_received_serialized_encapsulated_message(
            serialized_message,
            &self.message_cache,
            (from_peer_id, from_connection_id),
            &self.pending_poq_verifications,
            &mut self.waker,
            self.current_epoch_info.1,
            self.num_blend_layers,
            &self.proofs_verifier,
        ) {
            tracing::debug!(target: LOG_TARGET, "Failed to handle message from the current epoch: {receive_error:?}");
            // No matter what error it is, we can attribute it to the sender, so
            // we blacklist it.
            self.blacklist_peer(from_peer_id, receive_error.into());
            // Nevertheless, bytes that did not amount to a message are not a delivery: a
            // neighbour cannot hold its slot by sending garbage.
            return false;
        }

        true
    }
}

/// Point a peer's record at the connection that replaced the one it held,
/// which is the one in the other direction.
const fn replace_connection(
    existing_connection: &mut RemotePeerConnectionDetails,
    new_connection_id: ConnectionId,
    new_direction: ConnectionDirection,
) {
    existing_connection.direction = new_direction;
    existing_connection.connection_id = new_connection_id;
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
        peer_id: PeerId,
        _: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if let Some(blacklist_reason) = self.blacklisted_reason(&peer_id) {
            tracing::debug!(target: LOG_TARGET, "Inbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} will not be upgraded: the peer is blacklisted ({blacklist_reason:?}).");
            // We don't return a dummy handler, which relies on the swarm timeout
            // configuration to close the connection. We prevent the connection from being
            // established at all.
            return Err(ConnectionDenied::new(blacklist_reason));
        }

        // A connection offered above either bound is refused: the maximum the node
        // holds at all, and the share of that maximum it lets other nodes fill.
        if !self.can_accept_connection() {
            tracing::trace!(target: LOG_TARGET, "Inbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} will not be upgraded since we are already holding as many connections as we accept.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        // If there is already an established or pending inbound connection with
        // the given peer, do not try to upgrade the new one as we already have an
        // inbound connection. Otherwise, we let the connection upgrade, and we will
        // close one of the two connections depending on the comparison result of
        // local and remote peer IDs.
        if self.has_connection_with_peer(&peer_id, ConnectionDirection::Incoming) {
            tracing::trace!(target: LOG_TARGET, "Inbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} will not be upgraded since there is already an inbound connection established or pending.");
            return Ok(Either::Right(DummyConnectionHandler));
        }

        Ok(if !self.is_network_large_enough() {
            tracing::debug!(target: LOG_TARGET, "Denying inbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} because membership size is too small.");
            Either::Right(DummyConnectionHandler)
        } else if self.current_epoch_info.0.contains(&peer_id) {
            tracing::trace!(
                target: LOG_TARGET,
                "Upgrading inbound connection {connection_id:?} with core peer {peer_id:?} with addr {remote_addr:?}."
            );
            self.connections_waiting_upgrade.insert(
                (peer_id, connection_id),
                PendingUpgrade {
                    direction: ConnectionDirection::Incoming,
                    started_at: self.current_round,
                },
            );
            Either::Left(ConnectionHandler::new(
                self.protocol_name.clone(),
                (peer_id, connection_id),
                // Aligned with this node's other connections, so they all agree
                // on where a round boundary falls.
                self.round_clock.clone(),
                self.connection_share_per_round,
                self.send_deadline,
                encapsulated_message_encoded_size(self.num_blend_layers),
                self.handshake_upgrade_timeout,
            ))
        } else {
            tracing::trace!(target: LOG_TARGET, "Denying inbound connection {connection_id:?} with edge peer {peer_id:?} with addr {remote_addr:?}.");
            Either::Right(DummyConnectionHandler)
        })
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        remote_addr: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if let Some(blacklist_reason) = self.blacklisted_reason(&peer_id) {
            tracing::debug!(target: LOG_TARGET, "Outbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} will not be upgraded: the peer is blacklisted ({blacklist_reason:?}).");
            // We don't return a dummy handler, which relies on the swarm timeout
            // configuration to close the connection. We prevent the connection from being
            // established at all.
            return Err(ConnectionDenied::new(blacklist_reason));
        }

        // Only the overall maximum applies to a connection the node opened itself:
        // the share reserved for accepted connections exists to protect this
        // direction, not to limit it.
        if self.available_connection_slots() == 0 {
            tracing::trace!(target: LOG_TARGET, "Outbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} will not be upgraded since we are already at maximum peering capacity.");
            return Ok(self.refuse_outgoing_connection_attempt(
                peer_id,
                ConnectionUpgradeFailureReason::MaximumPeeringDegreeReached,
            ));
        }

        // If there is already an established outbound connection with the given peer,
        // do not try to upgrade the new one as we already have an outbound connection.
        // Otherwise, we let the connection upgrade, and we will close one of the two
        // connections depending on the comparison result of local and remote peer IDs.
        if self.has_connection_with_peer(&peer_id, ConnectionDirection::Outgoing) {
            tracing::trace!(target: LOG_TARGET, "Outbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} will not be upgraded since there is already an outbound connection established.");
            return Ok(self.refuse_outgoing_connection_attempt(
                peer_id,
                ConnectionUpgradeFailureReason::DuplicateConnection,
            ));
        }

        Ok(if !self.is_network_large_enough() {
            tracing::debug!(target: LOG_TARGET, "Denying outbound connection {connection_id:?} with peer {peer_id:?} with addr {remote_addr:?} because membership size is too small.");
            self.refuse_outgoing_connection_attempt(
                peer_id,
                ConnectionUpgradeFailureReason::Refused,
            )
        } else if self.current_epoch_info.0.contains(&peer_id) {
            tracing::trace!(
                target: LOG_TARGET,
                "Upgrading outbound connection {connection_id:?} with core peer {peer_id:?} with addr {remote_addr:?}."
            );
            self.connections_waiting_upgrade.insert(
                (peer_id, connection_id),
                PendingUpgrade {
                    direction: ConnectionDirection::Outgoing,
                    started_at: self.current_round,
                },
            );
            Either::Left(ConnectionHandler::new(
                self.protocol_name.clone(),
                (peer_id, connection_id),
                // Aligned with this node's other connections, so they all agree
                // on where a round boundary falls.
                self.round_clock.clone(),
                self.connection_share_per_round,
                self.send_deadline,
                encapsulated_message_encoded_size(self.num_blend_layers),
                self.handshake_upgrade_timeout,
            ))
        } else {
            tracing::debug!(target: LOG_TARGET, "Denying outbound connection {connection_id:?} with edge peer {peer_id:?} with addr {remote_addr:?}.");
            self.refuse_outgoing_connection_attempt(
                peer_id,
                ConnectionUpgradeFailureReason::Refused,
            )
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
            // Try to close the connection if it exists in the old epoch.
            if let Some(old_epoch) = &mut self.old_epoch
                && old_epoch.handle_closed_connection(&(peer_id, connection_id))
            {
                return;
            }

            // We notify the swarm of any connection that failed to be upgraded.
            if let Some(PendingUpgrade {
                direction: connection_direction,
                ..
            }) = self
                .connections_waiting_upgrade
                .remove(&(peer_id, connection_id))
            {
                debug_assert!(
                    ConnectionDirection::from_local_endpoint(local_endpoint.to_endpoint())
                        == connection_direction,
                    "Connection direction provided by the event and the one stored do not match."
                );
                // Notify the swarm about the negotiation failure.
                self.notify_about_connection_upgrade_failure(
                    peer_id,
                    ConnectionUpgradeFailure {
                        reason: ConnectionUpgradeFailureReason::ConnectionFailure,
                        direction: connection_direction,
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
                peer_details_entry.remove();
                self.events
                    .push_back(ToSwarm::GenerateEvent(Event::PeerDisconnected(peer_id)));
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
                    let message_counts_against_peer_total = self
                        .handle_received_serialized_encapsulated_message(
                            message.as_ref(),
                            (peer_id, connection_id),
                        );
                    if message_counts_against_peer_total
                        && self
                            .negotiated_peers
                            .get(&peer_id)
                            .is_some_and(|details| details.connection_id == connection_id)
                    {
                        self.liveness.record_message_from_neighbour(peer_id);
                    }
                }
                // The connection was fully negotiated by the peer, which means that
                // the peer supports the blend protocol. We consider them healthy by
                // default. The handler emits this event at most once per connection.
                ToBehaviour::FullyNegotiated => {
                    self.handle_negotiated_connection((peer_id, connection_id));
                }
                ToBehaviour::IOError(e) => {
                    tracing::trace!(target: LOG_TARGET, "IO error {e:?} with peer {peer_id:?} on connection {connection_id:?}");
                }
            },
        }
    }

    /// Polls for things that swarm should do.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Polled first and unconditionally: this is the only thing that keeps
        // the task scheduled when nothing else is happening, and the liveness
        // window it drives is what closes connections that have gone silent.
        let current_round = self.round_clock.poll_current(cx);
        if current_round > self.current_round {
            self.current_round = current_round;
            self.liveness
                .enter_new_round_with_peers(self.negotiated_peers.keys());
            self.abandon_stale_handshakes();
            self.close_unhealthy_connections();
            self.prune_expired_blacklist_entries();
            self.check_and_report_low_peering_degree();
        }

        if let Some(old_epoch) = &mut self.old_epoch
            && let Poll::Ready(event) = old_epoch.poll(cx)
        {
            return Poll::Ready(event);
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        // Verifications complete off this task, so their outcome is picked up
        // here: this is where a message becomes visible to the swarm, and hence
        // relayable, and where a peer that failed to prove its quota is dropped.
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
