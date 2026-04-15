use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    convert::Infallible,
    task::{Context, Poll, Waker},
};

use either::Either;
use lb_blend_message::encap::validated::{
    EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
};
use libp2p::{
    PeerId,
    swarm::{ConnectionId, NotifyHandler, ToSwarm},
};

use crate::core::with_core::{
    behaviour::{
        Event,
        handler::FromBehaviour,
        message_cache::MessageCache,
        utils::{
            forward_validated_message_and_update_cache,
            handle_received_serialized_encapsulated_message_and_update_cache,
        },
    },
    error::{ReceiveError, SendError},
};

const LOG_TARGET: &str = "blend::network::core::core::behaviour::old";

/// Defines behaviours for processing messages from the old session
/// until the session transition period has passed.
pub struct OldSession {
    negotiated_peers: HashMap<PeerId, ConnectionId>,
    events: VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    waker: Option<Waker>,
    message_cache: MessageCache,
    session_number: u64,
}

impl OldSession {
    #[must_use]
    pub const fn new(
        negotiated_peers: HashMap<PeerId, ConnectionId>,
        message_cache: MessageCache,
        session_number: u64,
    ) -> Self {
        Self {
            negotiated_peers,
            message_cache,
            events: VecDeque::new(),
            waker: None,
            session_number,
        }
    }

    /// Publish an encapsulated message with a validated public header to all
    /// negotiated peers.
    ///
    /// If the specified session does not match the current session, it returns
    /// an error without sending the message.
    pub(super) fn publish_message_with_validated_header(
        &mut self,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
        intended_session: u64,
    ) -> Result<(), SendError> {
        if self.session_number != intended_session {
            return Err(SendError::InvalidSession);
        }
        forward_validated_message_and_update_cache(
            &(message.into()),
            self.negotiated_peers.iter(),
            &mut self.events,
            &mut self.message_cache,
            self.waker.take(),
        )
    }

    /// Forward an encapsulated message with a validated signature to all
    /// negotiated peers, except the specified one.
    ///
    /// If the specified session does not match the current session, it returns
    /// an error without sending the message.
    pub(super) fn forward_message_with_validated_signature(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedSignature,
        except: PeerId,
        intended_session: u64,
    ) -> Result<(), SendError> {
        if self.session_number != intended_session {
            return Err(SendError::InvalidSession);
        }
        forward_validated_message_and_update_cache(
            message,
            self.negotiated_peers
                .iter()
                // Exclude sender
                .filter(|(peer_id, _)| **peer_id != except),
            &mut self.events,
            &mut self.message_cache,
            self.waker.take(),
        )
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub(super) fn force_send_serialized_message_to_peer_at_session(
        &mut self,
        serialized_message: Vec<u8>,
        peer_id: PeerId,
        session: u64,
    ) -> Result<(), SendError> {
        if session != self.session_number {
            return Err(SendError::InvalidSession);
        }

        let Some(connection_id) = self.negotiated_peers.get(&peer_id) else {
            return Err(SendError::NoPeers);
        };
        tracing::trace!(
            target: LOG_TARGET,
            "Notifying handler with peer {peer_id:?} on old session connection {connection_id:?} to deliver already-serialized message."
        );
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(serialized_message)),
        });
        self.try_wake();
        Ok(())
    }

    /// Handles a message received from a peer.
    ///
    /// # Returns
    /// - [`Ok(false)`] if the connection is not part of the session.
    /// - [`Ok(true)`] if the message was successfully processed and forwarded.
    /// - [`Err(Error)`] if the message is invalid or has already been
    ///   exchanged.
    pub(super) fn handle_received_serialized_encapsulated_message(
        &mut self,
        serialized_message: &[u8],
        (from_peer_id, from_connection_id): (PeerId, ConnectionId),
    ) -> Result<bool, ReceiveError> {
        if !self.is_negotiated(&(from_peer_id, from_connection_id)) {
            return Ok(false);
        }

        handle_received_serialized_encapsulated_message_and_update_cache(
            serialized_message,
            &mut self.message_cache,
            from_peer_id,
            &mut self.events,
            self.waker.take(),
            self.session_number,
        ).inspect_err(|receive_error| {
            tracing::debug!(target: LOG_TARGET, "Failed to handle message from the old session: {receive_error:?}. Closing connection with spammy peer.");
            self.events.push_back(ToSwarm::NotifyHandler {
                peer_id: from_peer_id,
                handler: NotifyHandler::One(from_connection_id),
                event: Either::Left(FromBehaviour::CloseSubstreams),
            });
            self.try_wake();
        })?;

        Ok(true)
    }

    /// Stops the old session by returning events to close all the substreams
    /// in the old session.
    ///
    /// It should be called once the session transition period has passed.
    pub fn stop(self) -> VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>> {
        let mut events = VecDeque::with_capacity(self.negotiated_peers.len());
        for (&peer_id, &connection_id) in &self.negotiated_peers {
            events.push_back(ToSwarm::NotifyHandler {
                peer_id,
                handler: NotifyHandler::One(connection_id),
                event: Either::Left(FromBehaviour::CloseSubstreams),
            });
        }
        events
    }

    /// Checks if the connection is part of the old session.
    #[must_use]
    pub fn is_negotiated(&self, (peer_id, connection_id): &(PeerId, ConnectionId)) -> bool {
        self.negotiated_peers
            .get(peer_id)
            .is_some_and(|&id| id == *connection_id)
    }

    /// Should be called when a connection is detected as closed.
    ///
    /// It removes the connection from the states and returns [`true`]
    /// if the connection was part of the old session.
    pub fn handle_closed_connection(
        &mut self,
        (peer_id, connection_id): &(PeerId, ConnectionId),
    ) -> bool {
        if let Entry::Occupied(entry) = self.negotiated_peers.entry(*peer_id)
            && entry.get() == connection_id
        {
            entry.remove();
            self.message_cache.remove_peer_info(peer_id);
            return true;
        }
        false
    }

    fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    pub fn poll(
        &mut self,
        cx: &Context<'_>,
    ) -> Poll<ToSwarm<Event, Either<FromBehaviour, Infallible>>> {
        if let Some(event) = self.events.pop_front() {
            Poll::Ready(event)
        } else {
            self.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}
