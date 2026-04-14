use core::{
    num::NonZeroUsize,
    ops::{Deref, RangeInclusive},
};
use std::{
    collections::{HashSet, hash_map::Entry},
    time::Duration,
};

use futures::StreamExt as _;
use lb_blend::{
    message::encap::validated::{
        EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
    },
    network::core::{
        NetworkBehaviourEvent,
        with_core::{
            behaviour::{
                ConnectionUpgradeFailureReason, Event as CoreToCoreEvent, IntervalStreamProvider,
                NegotiatedPeerState,
            },
            error::SendError,
        },
        with_edge::behaviour::Event as CoreToEdgeEvent,
    },
    scheduling::membership::Membership,
};
use lb_libp2p::{DialOpts, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, SwarmBuilder, swarm::dial_opts::PeerCondition};
use rand::RngCore;
use tokio::sync::{broadcast, mpsc};

use crate::{
    core::{
        backends::{
            PublicInfo, SessionInfo,
            libp2p::{
                LOG_TARGET, Libp2pBlendBackendSettings,
                behaviour::{BlendBehaviour, BlendBehaviourEvent},
                dials::{DialAttempt, OngoingDials, SessionDialAttempt},
            },
        },
        settings::RunningBlendConfig as BlendConfig,
    },
    metrics,
};

#[derive(Debug)]
pub enum BlendSwarmMessage {
    Publish {
        message: Box<EncapsulatedMessageWithVerifiedPublicHeader>,
        session: u64,
    },
    StartNewSession(SessionInfo<PeerId>),
    CompleteSessionTransition,
}

pub struct BlendSwarm<Rng, ObservationWindowProvider>
where
    ObservationWindowProvider: IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>
        + 'static,
{
    swarm: Swarm<BlendBehaviour<ObservationWindowProvider>>,
    swarm_messages_receiver: mpsc::Receiver<BlendSwarmMessage>,
    incoming_message_sender: broadcast::Sender<(EncapsulatedMessageWithVerifiedSignature, u64)>,
    public_info: PublicInfo<PeerId>,
    rng: Rng,
    ongoing_dials: OngoingDials,
    minimum_network_size: NonZeroUsize,
}

pub struct SwarmParams<'config, Rng> {
    pub config: &'config BlendConfig<Libp2pBlendBackendSettings>,
    pub current_public_info: PublicInfo<PeerId>,
    pub rng: Rng,
    pub swarm_message_receiver: mpsc::Receiver<BlendSwarmMessage>,
    pub incoming_message_sender: broadcast::Sender<(EncapsulatedMessageWithVerifiedSignature, u64)>,
    pub minimum_network_size: NonZeroUsize,
}

impl<Rng, ObservationWindowProvider> BlendSwarm<Rng, ObservationWindowProvider>
where
    Rng: RngCore,
    ObservationWindowProvider: IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>
        + for<'c> From<(
            &'c BlendConfig<Libp2pBlendBackendSettings>,
            &'c Membership<PeerId>,
        )> + 'static,
{
    pub(super) fn new(
        SwarmParams {
            config,
            current_public_info,
            rng,
            swarm_message_receiver: swarm_messages_receiver,
            incoming_message_sender,
            minimum_network_size,
        }: SwarmParams<Rng>,
    ) -> Self {
        let listening_address = config.backend.listening_address.clone();
        let mut swarm = SwarmBuilder::with_existing_identity(config.keypair())
            .with_tokio()
            .with_quic()
            .with_dns()
            .expect("DNS transport should be supported")
            .with_behaviour(|_| {
                BlendBehaviour::new(
                    config,
                    (
                        current_public_info.session.membership.clone(),
                        current_public_info.session.session_number,
                    ),
                )
            })
            .expect("Blend Behaviour should be built")
            .with_swarm_config(|cfg| {
                // The idle timeout starts ticking once there are no active streams on a
                // connection. We want the connection to be closed as soon as
                // all streams are dropped.
                cfg.with_idle_connection_timeout(Duration::ZERO)
            })
            .build();

        swarm.listen_on(listening_address).unwrap_or_else(|e| {
            panic!("Failed to listen on Blend network: {e:?}");
        });

        let mut self_instance = Self {
            swarm,
            swarm_messages_receiver,
            incoming_message_sender,
            public_info: current_public_info,
            rng,
            ongoing_dials: OngoingDials::new(
                config.backend.max_dial_attempts_per_peer,
                *config.backend.core_peering_degree.start() as usize,
            ),
            minimum_network_size,
        };

        self_instance.check_and_dial_new_peers_except(None);

        self_instance
    }
}

impl<Rng, ObservationWindowProvider> BlendSwarm<Rng, ObservationWindowProvider>
where
    Rng: RngCore,
    ObservationWindowProvider:
        IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>,
{
    /// Dial random peers from the membership list,
    /// excluding the peers with a negotiated connection in the ongoing session,
    /// the peers that we are already trying to dial, and the blocked peers.
    fn dial_random_peers_except(&mut self, amount: usize, except: Option<PeerId>) {
        let negotiated_peers = self.behaviour().blend.with_core().negotiated_peers().keys();
        let exclude_peers: HashSet<PeerId> = negotiated_peers
            .chain(self.swarm.behaviour().blocked_peers.blocked_peers())
            .chain(self.ongoing_dials.keys())
            .chain(except.iter())
            .copied()
            .collect();

        tracing::trace!(target: LOG_TARGET, amount, ?except, ?exclude_peers, "Dialing random peers");

        // We need to clone else we would not be able to call `self.dial` inside which
        // requires access to `&mut self`.
        let current_membership = self.public_info.session.membership.clone();
        current_membership
            .filter_and_choose_remote_nodes(&mut self.rng, amount, &exclude_peers)
            .for_each(|peer| {
                let peer_address = peer.address.clone();
                let peer_id = peer.id;
                self.dial(peer_id, peer_address);
            });
    }

    /// Dial new peers, if necessary, to maintain the peering degree.
    /// We aim to have at least the peering degree number of "healthy" peers.
    fn check_and_dial_new_peers_except(&mut self, except: Option<PeerId>) {
        tracing::trace!(target: LOG_TARGET, ?except, "Checking if we need to dial new peers");

        let membership_size = self.public_info.session.membership.size();
        if membership_size < self.minimum_network_size.get() {
            tracing::warn!(target: LOG_TARGET, "Not dialing any peers because set of core nodes is smaller than the minimum network size. {membership_size} < {}", self.minimum_network_size.get());
            return;
        }
        let num_new_conns_needed = self
            .minimum_healthy_peering_degree()
            .saturating_sub(self.num_healthy_peers());
        let available_connection_slots = self.available_connection_slots();
        if num_new_conns_needed > available_connection_slots {
            tracing::trace!(target: LOG_TARGET, "To maintain the minimum healthy peering degree the node would need to create {num_new_conns_needed} new connections, but only {available_connection_slots} slots are available.");
        }
        let connections_to_establish = num_new_conns_needed.min(available_connection_slots);
        self.dial_random_peers_except(connections_to_establish, except);
    }

    fn handle_disconnected_peer(&mut self, peer_id: PeerId, peer_state: NegotiatedPeerState) {
        tracing::trace!(target: LOG_TARGET, "Peer {peer_id} disconnected with state {peer_state:?}.");
        if peer_state.is_spammy() {
            self.swarm.behaviour_mut().blocked_peers.block_peer(peer_id);
        }
        self.check_and_dial_new_peers_except(Some(peer_id));
    }

    fn handle_unhealthy_peer(&mut self, peer_id: PeerId) {
        tracing::trace!(target: LOG_TARGET, "Peer {peer_id} is unhealthy");
        self.check_and_dial_new_peers_except(Some(peer_id));
    }

    fn handle_blend_core_behaviour_event(&mut self, blend_event: CoreToCoreEvent) {
        match blend_event {
            lb_blend::network::core::with_core::behaviour::Event::Message { message, sender, session } => {
                // Forward message received from node to all other core nodes.
                self.forward_received_core_message(&message, sender, session);
                // Bubble up to service for decapsulation and delaying.
                self.report_message_to_service(*message, session, metrics::InboundMessageType::Core);
            }
            lb_blend::network::core::with_core::behaviour::Event::UnhealthyPeer(peer_id) => {
                self.handle_unhealthy_peer(peer_id);
            }
            lb_blend::network::core::with_core::behaviour::Event::HealthyPeer(peer_id) => {
                Self::handle_healthy_peer(peer_id);
            }
            lb_blend::network::core::with_core::behaviour::Event::PeerDisconnected(
                peer_id,
                peer_state,
            ) => {
                self.handle_disconnected_peer(peer_id, peer_state);
            }
            lb_blend::network::core::with_core::behaviour::Event::OutboundConnectionUpgradeFailed { peer, reason } => {
                match reason {
                    ConnectionUpgradeFailureReason::ConnectionFailure => {
                        // If we ran out of dial attempts, we try to connect to another random peer that we are not yet connected to, if the dial attempt was performed in the current session.
                        let SessionDialAttempt::OngoingSession(Some(_)) = self.schedule_retry(peer) else {
                            return;
                        };
                        self.check_and_dial_new_peers_except(Some(peer));
                    }
                    upgrade_error @ (ConnectionUpgradeFailureReason::DuplicateConnection | ConnectionUpgradeFailureReason::MaximumPeeringDegreeReached | ConnectionUpgradeFailureReason::ReverseDirectionPreferred) => {
                        tracing::trace!(target: LOG_TARGET, "Outbound connection upgrade somewhat expectedly failed for {peer:?}. Reason: {upgrade_error:?}. Trying with a different peer if necessary.");
                        self.ongoing_dials.remove(&peer);
                        self.check_and_dial_new_peers_except(Some(peer));
                    }
                }
            }
            lb_blend::network::core::with_core::behaviour::Event::OutboundConnectionUpgradeSucceeded(peer_id) => {
                assert!(self.ongoing_dials.remove(&peer_id).is_some(), "Peer ID for a successfully upgraded connection must be present in storage");
            }
            lb_blend::network::core::with_core::behaviour::Event::InboundConnectionUpgradeFailed { peer, reason } => {
                tracing::trace!(target: LOG_TARGET, "Inbound connection upgrade expectedly failed for {peer:?} with reason {reason:?}");
            }
            lb_blend::network::core::with_core::behaviour::Event::InboundConnectionUpgradeSucceeded(peer_id) => {
                tracing::trace!(target: LOG_TARGET, "Inbound connection upgrade succeeded for {peer_id:?}");
            }
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    fn handle_event(&mut self, event: SwarmEvent<BlendBehaviourEvent<ObservationWindowProvider>>) {
        match event {
            SwarmEvent::ConnectionEstablished { .. } | SwarmEvent::ConnectionClosed { .. } => {
                let connected_count = self.swarm.connected_peers().count();
                metrics::peers_connected(connected_count);
            }
            SwarmEvent::Behaviour(BlendBehaviourEvent::Blend(NetworkBehaviourEvent::WithCore(
                e,
            ))) => {
                self.handle_blend_core_behaviour_event(e);
            }
            SwarmEvent::Behaviour(BlendBehaviourEvent::Blend(NetworkBehaviourEvent::WithEdge(
                e,
            ))) => {
                self.handle_blend_edge_behaviour_event(e);
            }
            // In case we fail to dial a peer, we retry. If the maximum number of trials is reached,
            // we re-evaluate the healthy connections and open a new one if needed, ignoring the
            // peer that we just failed to dial.
            SwarmEvent::OutgoingConnectionError {
                peer_id,
                connection_id,
                error,
            } => {
                tracing::warn!(
                    target: LOG_TARGET,
                    "Dialing error for peer: {peer_id:?} on connection: {connection_id:?}. Error: {error:?}"
                );
                // We don't retry if `peer_id` is `None` or if we've achieved the maximum number
                // of retries for this peer.
                let Some(peer_id) = peer_id else {
                    self.check_and_dial_new_peers_except(None);
                    return;
                };

                match self.schedule_retry(peer_id) {
                    SessionDialAttempt::PreviousSession => {
                        tracing::debug!(target: LOG_TARGET, "Received a dial error for peer {peer_id:?} that is not being tracked. This means that a new session has cleared the map of pending dials. No retry will be performed.");
                    }
                    SessionDialAttempt::OngoingSession(Some(_)) => {
                        self.check_and_dial_new_peers_except(Some(peer_id));
                    }
                    // Retry in progress.
                    SessionDialAttempt::OngoingSession(None) => {}
                }
            }
            _ => {
                tracing::trace!(target: LOG_TARGET, "Received event from blend network that will be ignored.");
                tracing::trace!(counter.ignored_event = 1);
            }
        }
    }

    fn handle_swarm_message(&mut self, msg: BlendSwarmMessage) {
        match msg {
            BlendSwarmMessage::Publish { message, session } => {
                self.handle_publish_swarm_message(*message, session);
            }
            BlendSwarmMessage::StartNewSession(new_session_info) => {
                self.public_info.session = new_session_info;
                self.swarm.behaviour_mut().blend.start_new_session((
                    self.public_info.session.membership.clone(),
                    self.public_info.session.session_number,
                ));
                self.ongoing_dials.clear();
                self.check_and_dial_new_peers_except(None);
            }
            BlendSwarmMessage::CompleteSessionTransition => {
                self.swarm.behaviour_mut().blend.finish_session_transition();
            }
        }
    }

    pub(crate) async fn run(mut self) {
        loop {
            self.poll_next_internal().await;
        }
    }

    async fn poll_next_internal(&mut self) {
        self.poll_next_and_match(|_| false).await;
    }

    async fn poll_next_and_match<Predicate>(
        &mut self,
        swarm_event_match_predicate: Predicate,
    ) -> bool
    where
        Predicate: Fn(&SwarmEvent<BlendBehaviourEvent<ObservationWindowProvider>>) -> bool,
    {
        tokio::select! {
            Some(msg) = self.swarm_messages_receiver.recv() => {
                self.handle_swarm_message(msg);
                false
            }
            Some(event) = self.swarm.next() => {
                let predicate_matched = swarm_event_match_predicate(&event);
                self.handle_event(event);
                predicate_matched
            }
            Some((peer_id, dial_attempt)) = self.ongoing_dials.next() => {
                self.execute_retry(peer_id, dial_attempt);
                false
            }
        }
    }

    #[cfg(test)]
    pub async fn poll_next(&mut self) {
        self.poll_next_internal().await;
    }

    #[cfg(test)]
    pub async fn poll_next_until<Predicate>(&mut self, swarm_event_match_predicate: Predicate)
    where
        Predicate: Fn(&SwarmEvent<BlendBehaviourEvent<ObservationWindowProvider>>) -> bool + Copy,
    {
        loop {
            if self.poll_next_and_match(swarm_event_match_predicate).await {
                break;
            }
        }
    }
}

impl<Rng, ObservationWindowProvider> BlendSwarm<Rng, ObservationWindowProvider>
where
    ObservationWindowProvider:
        IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>,
{
    /// It tries to dial the specified peer, by setting or increasing the
    /// counter of attempted dials towards the peer.
    ///
    /// This function always tries to dial and update the counter of attempted
    /// dials. Any checks about the maximum allowed dials must be performed in
    /// the context of the calling function.
    fn dial(&mut self, peer_id: PeerId, address: Multiaddr) {
        tracing::trace!(target: LOG_TARGET, "Dialing peer {peer_id:?} at address {address:?}.");
        match self.ongoing_dials.entry(peer_id) {
            Entry::Vacant(empty_entry) => {
                empty_entry.insert(DialAttempt::new(address.clone()));
            }
            Entry::Occupied(mut existing_entry) => {
                let last_attempt_number = existing_entry.get_mut().attempt_number_mut();
                *last_attempt_number = last_attempt_number.checked_add(1).unwrap();
            }
        }

        if let Err(e) = self.swarm.dial(
            DialOpts::peer_id(peer_id)
                .addresses(vec![address])
                // We use `Always` since we want to be able to dial a peer even if we already have
                // an established connection with it that belongs to the previous session.
                .condition(PeerCondition::Always)
                .build(),
        ) {
            tracing::error!(target: LOG_TARGET, "Failed to dial peer {peer_id:?}: {e:?}");
            self.schedule_retry(peer_id);
        }
    }

    #[cfg(test)]
    pub fn dial_peer_at_addr(&mut self, peer_id: PeerId, address: Multiaddr) {
        self.dial(peer_id, address);
    }

    #[cfg(test)]
    pub const fn ongoing_dials(&self) -> &OngoingDials {
        &self.ongoing_dials
    }

    /// Delegate to [`OngoingDials::schedule_retry`] for exponential-backoff
    /// redial scheduling.
    fn schedule_retry(&mut self, peer_id: PeerId) -> SessionDialAttempt {
        self.ongoing_dials.schedule_retry(peer_id)
    }

    /// Called when a pending retry fires. Re-checks peering degree before
    /// actually dialing, so we don't waste a slot on a peer we no longer need.
    fn execute_retry(&mut self, peer_id: PeerId, dial_attempt: DialAttempt) {
        let num_new_conns_needed = self
            .minimum_healthy_peering_degree()
            .saturating_sub(self.num_healthy_peers());
        if num_new_conns_needed == 0 {
            tracing::debug!(
                target: LOG_TARGET,
                "Skipping retry for peer {peer_id:?}: peering degree already satisfied."
            );
            return;
        }
        tracing::trace!(
            target: LOG_TARGET,
            "Executing backoff retry for peer {peer_id:?}."
        );
        let address = dial_attempt.address().clone();
        self.ongoing_dials.insert(peer_id, dial_attempt);
        if let Err(e) = self.swarm.dial(
            DialOpts::peer_id(peer_id)
                .addresses(vec![address])
                .condition(PeerCondition::Always)
                .build(),
        ) {
            tracing::error!(target: LOG_TARGET, "Failed to redial peer {peer_id:?}: {e:?}");
            self.schedule_retry(peer_id);
        }
    }

    fn publish_received_edge_message(&mut self, msg: &EncapsulatedMessageWithVerifiedSignature) {
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .blend
            .with_core_mut()
            .publish_message_with_validated_signature_to_current_session(msg)
        {
            tracing::error!(target: LOG_TARGET, "Failed to publish message to blend network: {e:?}");
            metrics::outbound_publish_err();
        } else {
            metrics::outbound_publish_ok();
        }
    }

    fn forward_received_core_message(
        &mut self,
        msg: &EncapsulatedMessageWithVerifiedSignature,
        except: PeerId,
        session: u64,
    ) {
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .blend
            .with_core_mut()
            .forward_message_with_validated_signature(msg, except, session)
        {
            // If we have a single connection, then we will always hit the `NoPeers` error.
            // In this case it's ok not to log such error, since this function is only
            // called on FORWARDED messages, not on PUBLISHED ones, for which we want to
            // know if that is the issue.
            if !matches!(e, SendError::NoPeers) {
                tracing::error!(target: LOG_TARGET, "Failed to forward message to blend network: {e:?}");
                metrics::outbound_forward_err();
            }
        } else {
            metrics::outbound_forward_ok();
        }
    }

    fn report_message_to_service(
        &self,
        msg: EncapsulatedMessageWithVerifiedSignature,
        session: u64,
        message_type: metrics::InboundMessageType,
    ) {
        tracing::trace!("Received message from a peer: {msg:?}");

        if self.incoming_message_sender.send((msg, session)).is_err() {
            tracing::trace!(target: LOG_TARGET, "Failed to send incoming message to channel. No active listeners yet.");
            metrics::inbound_message_err(message_type);
        } else {
            metrics::inbound_message_ok();
        }
    }

    fn minimum_healthy_peering_degree(&self) -> usize {
        self.swarm
            .behaviour()
            .blend
            .with_core()
            .minimum_healthy_peering_degree()
    }

    fn num_healthy_peers(&self) -> usize {
        self.swarm.behaviour().blend.with_core().num_healthy_peers()
    }

    fn available_connection_slots(&self) -> usize {
        self.swarm
            .behaviour()
            .blend
            .with_core()
            .available_connection_slots()
    }

    fn handle_healthy_peer(peer_id: PeerId) {
        tracing::trace!(target: LOG_TARGET, "Peer {peer_id} is healthy again");
    }

    fn handle_blend_edge_behaviour_event(&mut self, blend_event: CoreToEdgeEvent) {
        match blend_event {
            lb_blend::network::core::with_edge::behaviour::Event::Message(msg) => {
                // Forward message received from edge node to all the core nodes.
                self.publish_received_edge_message(&msg);
                // Bubble up to service for decapsulation and delaying.
                self.report_message_to_service(
                    msg,
                    self.public_info.session.session_number,
                    metrics::InboundMessageType::Edge,
                );
            }
        }
    }

    fn handle_publish_swarm_message(
        &mut self,
        msg: EncapsulatedMessageWithVerifiedPublicHeader,
        intended_session: u64,
    ) {
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .blend
            .with_core_mut()
            .publish_message_with_validated_header(msg, intended_session)
        {
            tracing::error!(target: LOG_TARGET, "Failed to publish message to blend network: {e:?}");
            metrics::outbound_publish_err();
        } else {
            metrics::outbound_publish_ok();
        }
    }
}

impl<Rng, ObservationWindowProvider> BlendSwarm<Rng, ObservationWindowProvider>
where
    Rng: RngCore,
    ObservationWindowProvider: IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>
        + 'static,
{
    #[cfg(test)]
    #[expect(clippy::too_many_arguments, reason = "necessary for testing")]
    pub fn new_test<BehaviourConstructor>(
        identity: &libp2p::identity::Keypair,
        behaviour_constructor: BehaviourConstructor,
        swarm_messages_receiver: mpsc::Receiver<BlendSwarmMessage>,
        incoming_message_sender: broadcast::Sender<(EncapsulatedMessageWithVerifiedSignature, u64)>,
        current_public_info: PublicInfo<PeerId>,
        rng: Rng,
        max_dial_attempts_per_connection: core::num::NonZeroU64,
        minimum_network_size: NonZeroUsize,
    ) -> Self
    where
        BehaviourConstructor:
            FnOnce(PeerId, Membership<PeerId>) -> BlendBehaviour<ObservationWindowProvider>,
    {
        use crate::test_utils::memory_test_swarm;

        let membership = current_public_info.session.membership.clone();
        Self {
            incoming_message_sender,
            public_info: current_public_info,
            ongoing_dials: OngoingDials::new(max_dial_attempts_per_connection, 0),
            rng,
            swarm: memory_test_swarm(
                identity,
                membership,
                Duration::from_secs(1),
                behaviour_constructor,
            ),
            swarm_messages_receiver,
            minimum_network_size,
        }
    }
}

// We implement `Deref` so we are able to call swarm methods on our own swarm.
impl<Rng, ObservationWindowProvider> Deref for BlendSwarm<Rng, ObservationWindowProvider>
where
    ObservationWindowProvider: IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>
        + 'static,
{
    type Target = Swarm<BlendBehaviour<ObservationWindowProvider>>;

    fn deref(&self) -> &Self::Target {
        &self.swarm
    }
}

#[cfg(test)]
// We implement `DerefMut` only for tests, since we do not want to give people a
// chance to bypass our API.
impl<Rng, ObservationWindowProvider> core::ops::DerefMut
    for BlendSwarm<Rng, ObservationWindowProvider>
where
    ObservationWindowProvider: IntervalStreamProvider<IntervalStream: Unpin + Send, IntervalItem = RangeInclusive<u64>>
        + 'static,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.swarm
    }
}
