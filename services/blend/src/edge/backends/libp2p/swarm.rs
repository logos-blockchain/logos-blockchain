use core::{
    num::{NonZeroU64, NonZeroUsize},
    pin::Pin,
};
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    time::Duration,
};

use futures::{AsyncWriteExt as _, StreamExt as _, stream::FuturesUnordered};
use lb_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    network::send_msg,
    scheduling::{
        membership::{Membership, Node},
        serialize_encapsulated_message_with_verified_public_header,
    },
};
use lb_libp2p::{DialError, DialOpts, SwarmEvent};
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
    identity::Keypair,
    swarm::{ConnectionId, dial_opts::PeerCondition},
};
use rand::RngCore;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use super::settings::Libp2pBlendBackendSettings;
use crate::edge::backends::libp2p::LOG_TARGET;

#[derive(Debug)]
pub struct DialAttempt {
    /// Address of peer being dialed.
    address: Multiaddr,
    /// The latest (ongoing) attempt number.
    attempt_number: NonZeroU64,
    /// The message to send once the peer is successfully dialed.
    message: EncapsulatedMessageWithVerifiedPublicHeader,
}

#[cfg(test)]
impl DialAttempt {
    pub const fn address(&self) -> &Multiaddr {
        &self.address
    }

    pub const fn attempt_number(&self) -> NonZeroU64 {
        self.attempt_number
    }

    pub const fn message(&self) -> &EncapsulatedMessageWithVerifiedPublicHeader {
        &self.message
    }
}

type PendingEvents = FuturesUnordered<Pin<Box<dyn Future<Output = PendingEvent> + Send>>>;

/// An event produced by a future in [`BlendSwarm::pending`], applied back to
/// swarm state once the future resolves. Retry timers and in-flight sends share
/// a single queue, so the `select!` loop has one place to drain them.
enum PendingEvent {
    /// A retry's backoff has elapsed; the peer should be redialed.
    RetryReady {
        peer_id: PeerId,
        dial_attempt: Box<DialAttempt>,
    },
    /// An in-flight stream-open/send completed.
    Send(SendOutcome),
}

/// Outcome of an in-flight attempt to open a stream and send a message,
/// applied back to [`BlendSwarm`] state once the future completes.
enum SendOutcome {
    /// The message was sent (the stream close result is best-effort). The
    /// pending dial for this connection can be removed.
    Sent {
        peer_id: PeerId,
        connection_id: ConnectionId,
    },
    /// Opening the stream or sending the message failed. The dial should be
    /// retried with backoff.
    Failed {
        peer_id: PeerId,
        connection_id: ConnectionId,
    },
}

pub(super) struct BlendSwarm<Rng>
where
    Rng: RngCore + 'static,
{
    swarm: Swarm<libp2p_stream::Behaviour>,
    stream_control: libp2p_stream::Control,
    command_receiver: mpsc::Receiver<Command>,
    membership: Membership<PeerId>,
    rng: Rng,
    max_dial_attempts_per_connection: NonZeroU64,
    pending_dials: HashMap<(PeerId, ConnectionId), DialAttempt>,
    /// Pending retry timers and in-flight stream-open/send futures. Both are
    /// polled off the main `select!` loop so that one unresponsive peer cannot
    /// stall command handling.
    pending_events: PendingEvents,
    protocol_name: StreamProtocol,
    replication_factor: NonZeroUsize,
}

#[derive(Debug)]
pub enum Command {
    SendMessage(EncapsulatedMessageWithVerifiedPublicHeader),
}

impl<Rng> BlendSwarm<Rng>
where
    Rng: RngCore + 'static,
{
    pub(super) fn new(
        settings: Libp2pBlendBackendSettings,
        membership: Membership<PeerId>,
        rng: Rng,
        command_receiver: mpsc::Receiver<Command>,
        identity: Keypair,
    ) -> Self {
        let swarm = SwarmBuilder::with_existing_identity(identity)
            .with_tokio()
            .with_quic()
            .with_dns()
            .expect("DNS transport should be supported")
            .with_behaviour(|_| libp2p_stream::Behaviour::new())
            .expect("Behaviour should be built")
            .with_swarm_config(|cfg| {
                // We cannot use zero as that would immediately close a connection with an edge
                // node before they have a chance to upgrade the stream and send the message.
                cfg.with_idle_connection_timeout(Duration::from_secs(1))
            })
            .build();

        tracing::info!(target: LOG_TARGET, "Blend edge swarm started with local peer id: {:?}.", swarm.local_peer_id());

        let stream_control = swarm.behaviour().new_control();

        let replication_factor: NonZeroUsize = settings.replication_factor.try_into().unwrap();
        let membership_size = membership.size();

        if membership_size < replication_factor.get() {
            warn!(target: LOG_TARGET, "Replication factor configured to {replication_factor} but only {membership_size} peers are available.");
        }

        Self {
            swarm,
            stream_control,
            command_receiver,
            membership,
            rng,
            pending_dials: HashMap::new(),
            pending_events: FuturesUnordered::new(),
            max_dial_attempts_per_connection: settings.max_dial_attempts_per_peer_per_message,
            protocol_name: settings.protocol_name.into_inner(),
            replication_factor,
        }
    }

    #[cfg(test)]
    pub fn new_test(
        identity: &Keypair,
        membership: Membership<PeerId>,
        command_receiver: mpsc::Receiver<Command>,
        max_dial_attempts_per_connection: NonZeroU64,
        rng: Rng,
        protocol_name: StreamProtocol,
        replication_factor: NonZeroUsize,
    ) -> Self {
        use crate::test_utils::memory_test_swarm;

        let inner_swarm = memory_test_swarm(
            identity,
            membership.clone(),
            Duration::from_secs(1),
            |_, _| libp2p_stream::Behaviour::new(),
        );

        Self {
            command_receiver,
            membership,
            max_dial_attempts_per_connection,
            pending_dials: HashMap::new(),
            pending_events: FuturesUnordered::new(),
            rng,
            stream_control: inner_swarm.behaviour().new_control(),
            swarm: inner_swarm,
            protocol_name,
            replication_factor,
        }
    }

    #[cfg(test)]
    pub const fn pending_dials(&self) -> &HashMap<(PeerId, ConnectionId), DialAttempt> {
        &self.pending_dials
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::SendMessage(msg) => {
                self.handle_send_message_command(&msg);
            }
        }
    }

    fn handle_send_message_command(&mut self, msg: &EncapsulatedMessageWithVerifiedPublicHeader) {
        self.dial_and_schedule_message(msg);
    }

    /// Schedule a dial with retries for a given message.
    ///
    /// A single set of `replication_factor` peers is chosen at random for the
    /// message. Each chosen peer is then retried with exponential backoff (see
    /// [`Self::schedule_retry`]); if a peer is still unreachable after all
    /// attempts, the message is dropped for that peer.
    fn dial_and_schedule_message(&mut self, msg: &EncapsulatedMessageWithVerifiedPublicHeader) {
        let peers = self.choose_peers();
        if peers.is_empty() {
            error!(target: LOG_TARGET, "No peers available to send the message to");
            return;
        }
        for node in peers {
            let (peer_id, address) = (node.id, node.address);
            let opts = dial_opts(peer_id, address.clone());
            let connection_id = opts.connection_id();

            let Entry::Vacant(empty_entry) = self.pending_dials.entry((peer_id, connection_id))
            else {
                panic!(
                    "Dial attempt for peer {peer_id:?} and connection {connection_id:?} should not be present in storage."
                );
            };
            empty_entry.insert(DialAttempt {
                address,
                attempt_number: 1.try_into().unwrap(),
                message: msg.clone(),
            });

            if let Err(e) = self.swarm.dial(opts) {
                error!(target: LOG_TARGET, "Failed to dial peer {peer_id:?} on connection {connection_id:?}: {e:?}");
                self.schedule_retry(peer_id, connection_id);
            }
        }
    }

    /// Schedule a retry for a failed dial attempt with exponential backoff.
    ///
    /// The dial attempt is removed from `pending_dials`. If the maximum number
    /// of attempts has not been reached, a delayed future is pushed into
    /// `pending_events`; when it fires, the dial is re-attempted in
    /// `poll_next_and_match`. Once all attempts are exhausted the message is
    /// dropped: we do not fall back to a different peer.
    fn schedule_retry(&mut self, peer_id: PeerId, connection_id: ConnectionId) {
        let dial_attempt = self
            .pending_dials
            .remove(&(peer_id, connection_id))
            .unwrap();
        let new_dial_attempt_number = dial_attempt.attempt_number.checked_add(1).unwrap();
        if new_dial_attempt_number > self.max_dial_attempts_per_connection {
            error!(
                target: LOG_TARGET,
                "Giving up on message delivery: peer {peer_id:?} was not reachable after {} attempts. Dropping the message.",
                self.max_dial_attempts_per_connection
            );
            return;
        }
        let delay = Duration::from_secs(
            1u64.checked_shl((new_dial_attempt_number.get() - 1) as u32)
                .unwrap_or_else(|| {
                    tracing::warn!(target: LOG_TARGET, "Shift overflow when calculating delay for peer {peer_id:?}. Using maximum delay.");
                    u64::MAX
                }),
        );
        debug!(
            target: LOG_TARGET,
            "Scheduling retry {new_dial_attempt_number} for peer {peer_id:?} in {} seconds",
            delay.as_secs()
        );
        self.pending_events.push(Box::pin(async move {
            tokio::time::sleep(delay).await;
            PendingEvent::RetryReady {
                peer_id,
                dial_attempt: Box::new(DialAttempt {
                    attempt_number: new_dial_attempt_number,
                    ..dial_attempt
                }),
            }
        }));
    }

    fn choose_peers(&mut self) -> Vec<Node<PeerId>> {
        let peers_to_choose = self.membership.size().min(self.replication_factor.get());
        self.membership
            .filter_and_choose_remote_nodes(&mut self.rng, peers_to_choose, &HashSet::new())
            .cloned()
            .collect()
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<()>) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                ..
            } => {
                self.handle_connection_established(peer_id, connection_id);
            }
            SwarmEvent::OutgoingConnectionError {
                connection_id,
                peer_id,
                error,
            } => {
                self.handle_outgoing_connection_error(peer_id, connection_id, &error);
            }
            _ => {
                trace!(target: LOG_TARGET, "Unhandled swarm event: {event:?}");
            }
        }
    }

    /// On a newly established connection, kick off opening a stream and sending
    /// the pending message.
    ///
    /// The open/send/close chain can block indefinitely on a peer that
    /// completes the QUIC handshake but never negotiates the stream protocol,
    /// so instead of awaiting it inline (which would stall the whole `select!`
    /// loop), we push it into `pending` and apply the resulting [`SendOutcome`]
    /// back to our state once it completes.
    fn handle_connection_established(&self, peer_id: PeerId, connection_id: ConnectionId) {
        debug!(target: LOG_TARGET, "Connection established: peer_id: {peer_id}, connection_id: {connection_id}");

        // We clone the message so the send future can own it independently of
        // `self`; the pending dial stays in place until the send completes.
        let message = self
            .pending_dials
            .get(&(peer_id, connection_id))
            .map(|entry| entry.message.clone())
            .unwrap();

        let stream_control = self.stream_control.clone();
        let protocol_name = self.protocol_name.clone();
        self.pending_events.push(Box::pin(async move {
            PendingEvent::Send(
                send_message_over_new_stream(
                    stream_control,
                    protocol_name,
                    message,
                    peer_id,
                    connection_id,
                )
                .await,
            )
        }));
    }

    /// Apply the outcome of a completed [`pending send`](Self::pending_sends)
    /// to our state.
    fn handle_send_outcome(&mut self, outcome: &SendOutcome) {
        match outcome {
            SendOutcome::Sent {
                peer_id,
                connection_id,
            } => {
                self.pending_dials.remove(&(*peer_id, *connection_id));
            }
            SendOutcome::Failed {
                peer_id,
                connection_id,
            } => {
                self.schedule_retry(*peer_id, *connection_id);
            }
        }
    }

    /// Redial a peer whose retry backoff has elapsed.
    fn handle_new_dial_retry(&mut self, peer_id: PeerId, dial_attempt: DialAttempt) {
        let opts = dial_opts(peer_id, dial_attempt.address.clone());
        let connection_id = opts.connection_id();
        self.pending_dials
            .insert((peer_id, connection_id), dial_attempt);

        if let Err(e) = self.swarm.dial(opts) {
            error!(target: LOG_TARGET, "Failed to redial peer {peer_id:?}: {e:?}");
            self.schedule_retry(peer_id, connection_id);
        }
    }

    fn handle_outgoing_connection_error(
        &mut self,
        peer_id: Option<PeerId>,
        connection_id: ConnectionId,
        error: &DialError,
    ) {
        error!(target: LOG_TARGET, "Outgoing connection error: peer_id:{peer_id:?}, connection_id:{connection_id}: {error}");

        let Some(peer_id) = peer_id else {
            debug!(target: LOG_TARGET, "No PeerId set. Ignoring: peer_id:{peer_id:?}, connection_id:{connection_id}");
            return;
        };

        self.schedule_retry(peer_id, connection_id);
    }

    #[cfg(test)]
    pub fn send_message(&mut self, msg: &EncapsulatedMessageWithVerifiedPublicHeader) {
        self.dial_and_schedule_message(msg);
    }

    /// Push a send future that never completes into the pending-events queue,
    /// simulating a peer that accepted the connection but never lets the
    /// stream-open/send finish. Tests use this to assert the event loop keeps
    /// servicing commands and retries instead of head-of-line blocking on it.
    #[cfg(test)]
    pub fn push_stalled_send(&self) {
        self.pending_events
            .push(Box::pin(core::future::pending::<PendingEvent>()));
    }

    /// Drive a single iteration of the event loop.
    #[cfg(test)]
    pub async fn poll_next(&mut self) {
        self.poll_next_internal().await;
    }

    pub(super) async fn run(mut self) {
        loop {
            self.poll_next_internal().await;
        }
    }

    async fn poll_next_internal(&mut self) {
        self.poll_next_and_match(|_| false).await;
    }

    async fn poll_next_and_match<Predicate>(&mut self, predicate: Predicate) -> bool
    where
        Predicate: Fn(&SwarmEvent<()>) -> bool,
    {
        tokio::select! {
            Some(event) = self.swarm.next() => {
                let predicate_matched = predicate(&event);
                self.handle_swarm_event(event);
                predicate_matched
            }
            Some(command) = self.command_receiver.recv() => {
                self.handle_command(command);
                false
            }
            Some(event) = self.pending_events.next() => {
                match event {
                    PendingEvent::RetryReady { peer_id, dial_attempt } => {
                        self.handle_new_dial_retry(peer_id, *dial_attempt);
                    }
                    PendingEvent::Send(outcome) => {
                        self.handle_send_outcome(&outcome);
                    }
                }
                false
            }
        }
    }

    #[cfg(test)]
    pub async fn poll_next_until<Predicate>(&mut self, predicate: Predicate)
    where
        Predicate: Fn(&SwarmEvent<()>) -> bool + Copy,
    {
        loop {
            if self.poll_next_and_match(predicate).await {
                break;
            }
        }
    }
}

/// Open a stream to `peer_id`, send `message` over it, and close it.
///
/// Runs detached from the main `select!` loop (see
/// [`BlendSwarm::handle_connection_established`]) so that a peer which stalls
/// stream negotiation only delays its own send rather than wedging the whole
/// event loop.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: Address this at some point."
)]
async fn send_message_over_new_stream(
    mut stream_control: libp2p_stream::Control,
    protocol_name: StreamProtocol,
    message: EncapsulatedMessageWithVerifiedPublicHeader,
    peer_id: PeerId,
    connection_id: ConnectionId,
) -> SendOutcome {
    let stream = match stream_control.open_stream(peer_id, protocol_name).await {
        Ok(stream) => stream,
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to open stream to {peer_id}: {e}");
            return SendOutcome::Failed {
                peer_id,
                connection_id,
            };
        }
    };

    let stream = match send_msg(
        stream,
        serialize_encapsulated_message_with_verified_public_header(&message),
    )
    .await
    {
        Ok(stream) => stream,
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to send message: {e} to peer {peer_id:?} on connection {connection_id:?}.");
            return SendOutcome::Failed {
                peer_id,
                connection_id,
            };
        }
    };

    info!(target: LOG_TARGET, "Message sent successfully to peer {peer_id:?} on connection {connection_id:?}.");
    close_stream(stream, peer_id, connection_id).await;
    SendOutcome::Sent {
        peer_id,
        connection_id,
    }
}

async fn close_stream(mut stream: libp2p::Stream, peer_id: PeerId, connection_id: ConnectionId) {
    if let Err(e) = stream.close().await {
        error!(target: LOG_TARGET, "Failed to close stream: {e} with peer {peer_id:?} on connection {connection_id:?}.");
    }
}

fn dial_opts(peer_id: PeerId, address: Multiaddr) -> DialOpts {
    DialOpts::peer_id(peer_id)
        .addresses(vec![address])
        .condition(PeerCondition::Always)
        .build()
}
