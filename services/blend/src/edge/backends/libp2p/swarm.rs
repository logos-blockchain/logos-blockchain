use core::num::{NonZeroU64, NonZeroUsize};
use std::{io, time::Duration};

use futures::{AsyncWriteExt as _, StreamExt as _};
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
use libp2p_stream::OpenStreamError;
use rand::RngCore;
use tokio::sync::mpsc;
use tracing::{debug, error, trace, warn};

use super::settings::Libp2pBlendBackendSettings;
use crate::edge::backends::libp2p::{
    LOG_TARGET,
    dials::{DialAttempt, Error, OngoingDials},
};

pub(super) struct BlendSwarm<Rng>
where
    Rng: RngCore + 'static,
{
    swarm: Swarm<libp2p_stream::Behaviour>,
    stream_control: libp2p_stream::Control,
    command_receiver: mpsc::Receiver<Command>,
    membership: Membership<PeerId>,
    rng: Rng,
    ongoing_dials: OngoingDials,
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
            ongoing_dials: OngoingDials::new(settings.max_dial_attempts_per_peer_per_message),
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
            ongoing_dials: OngoingDials::new(max_dial_attempts_per_connection),
            rng,
            stream_control: inner_swarm.behaviour().new_control(),
            swarm: inner_swarm,
            protocol_name,
            replication_factor,
        }
    }

    #[cfg(test)]
    pub fn pending_dials(&self) -> impl Iterator<Item = (&(PeerId, ConnectionId), &DialAttempt)> {
        self.ongoing_dials
            .active()
            .iter()
            .map(|(connection, (dial_attempt, _))| (connection, dial_attempt))
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::SendMessage(msg) => {
                self.dial_and_schedule_message_except(&msg, None);
            }
        }
    }

    /// Schedule a dial with retries for a given message.
    ///
    /// The peer to send the message to is chosen at random, except the provided
    /// peer, if specified.
    fn dial_and_schedule_message_except(
        &mut self,
        msg: &EncapsulatedMessageWithVerifiedPublicHeader,
        except: Option<PeerId>,
    ) {
        let peers = self.choose_peers_except(except);
        if peers.is_empty() {
            error!(target: LOG_TARGET, "No peers available to send the message to");
            return;
        }
        for node in peers {
            let (peer_id, address) = (node.id, node.address);
            let opts = dial_opts(peer_id, address.clone());
            let connection_id = opts.connection_id();

            self.ongoing_dials
                .schedule((peer_id, connection_id), (address, msg.clone()))
                .unwrap();
        }
    }

    fn choose_peers_except(&mut self, except: Option<PeerId>) -> Vec<Node<PeerId>> {
        let peers_to_choose = self.membership.size().min(self.replication_factor.get());
        self.membership
            .filter_and_choose_remote_nodes(
                &mut self.rng,
                peers_to_choose,
                &except.into_iter().collect(),
            )
            .cloned()
            .collect()
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<()>) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                ..
            } => {
                self.handle_connection_established(peer_id, connection_id)
                    .await;
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

    async fn handle_connection_established(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) {
        debug!(target: LOG_TARGET, "Connection established: peer_id: {peer_id}, connection_id: {connection_id}");

        // We need to clone so we can access `&mut self` below.
        let message = self
            .ongoing_dials
            .get(&(peer_id, connection_id))
            .unwrap()
            .message
            .clone();

        match self
            .stream_control
            .open_stream(peer_id, self.protocol_name.clone())
            .await
        {
            Ok(stream) => {
                self.handle_open_stream_success(stream, &message, (peer_id, connection_id))
                    .await;
            }
            Err(e) => self.handle_open_stream_failure(&e, (peer_id, connection_id)),
        }
    }

    async fn handle_open_stream_success(
        &mut self,
        stream: libp2p::Stream,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        (peer_id, connection_id): (PeerId, ConnectionId),
    ) {
        match send_msg(
            stream,
            serialize_encapsulated_message_with_verified_public_header(message),
        )
        .await
        {
            Ok(stream) => {
                self.handle_send_message_success(stream, (peer_id, connection_id))
                    .await;
            }
            Err(e) => self.handle_send_message_failure(&e, (peer_id, connection_id)),
        }
    }

    async fn handle_send_message_success(
        &mut self,
        stream: libp2p::Stream,
        (peer_id, connection_id): (PeerId, ConnectionId),
    ) {
        debug!(target: LOG_TARGET, "Message sent successfully to peer {peer_id:?} on connection {connection_id:?}.");
        close_stream(stream, peer_id, connection_id).await;
        // Regardless of the result of closing the stream, the message was sent so we
        // can remove the pending dial info.
        self.ongoing_dials.remove(&(peer_id, connection_id));
    }

    fn handle_send_message_failure(
        &mut self,
        error: &io::Error,
        (peer_id, connection_id): (PeerId, ConnectionId),
    ) {
        error!(target: LOG_TARGET, "Failed to send message: {error} to peer {peer_id:?} on connection {connection_id:?}.");
        // If the maximum attempt count was reached for this peer, try to schedule the
        // message for a different peer.
        if let Some(DialAttempt { message, .. }) = self
            .ongoing_dials
            .reschedule(peer_id, connection_id)
            .unwrap()
        {
            self.dial_and_schedule_message_except(&message, Some(peer_id));
        }
    }

    fn handle_open_stream_failure(
        &mut self,
        error: &OpenStreamError,
        (peer_id, connection_id): (PeerId, ConnectionId),
    ) {
        error!(target: LOG_TARGET, "Failed to open stream to {peer_id}: {error}");
        // If the maximum attempt count was reached for this peer, try to schedule the
        // message for a different peer.
        if let Some(DialAttempt { message, .. }) = self
            .ongoing_dials
            .reschedule(peer_id, connection_id)
            .unwrap()
        {
            self.dial_and_schedule_message_except(&message, Some(peer_id));
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

        // If the maximum attempt count was reached for this peer, try to schedule the
        // message for a different peer.
        if let Some(DialAttempt { message, .. }) = self
            .ongoing_dials
            .reschedule(peer_id, connection_id)
            .unwrap()
        {
            self.dial_and_schedule_message_except(&message, Some(peer_id));
        }
    }

    #[cfg(test)]
    pub fn send_message(&mut self, msg: &EncapsulatedMessageWithVerifiedPublicHeader) {
        self.dial_and_schedule_message_except(msg, None);
    }

    #[cfg(test)]
    pub fn send_message_to_anyone_except(
        &mut self,
        peer_id: PeerId,
        msg: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) {
        self.dial_and_schedule_message_except(msg, Some(peer_id));
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
                self.handle_swarm_event(event).await;
                predicate_matched
            }
            Some(command) = self.command_receiver.recv() => {
                self.handle_command(command);
                false
            }
            Some((peer_id, dial_attempt)) = self.ongoing_dials.next() => {
                let opts = dial_opts(peer_id, dial_attempt.address);
                let connection_id = opts.connection_id();

                if let Err(e) = self.swarm.dial(opts) {
                    error!(target: LOG_TARGET, "Failed to redial peer {peer_id:?}: {e:?}");
                    if let Some(DialAttempt { message, .. }) = self.ongoing_dials.reschedule(peer_id, connection_id).unwrap() {
                        self.dial_and_schedule_message_except(&message, Some(peer_id));
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
