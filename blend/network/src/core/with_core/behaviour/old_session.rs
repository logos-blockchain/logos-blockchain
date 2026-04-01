use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    convert::Infallible,
    task::{Context, Poll, Waker},
};

use either::Either;
use lb_blend_message::encap::{self, encapsulated::EncapsulatedMessage};
use lb_blend_proofs::quota::inputs::prove::public::LeaderInputs;
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
            handle_received_serialized_encapsulated_message_and_update_cache,
            validate_forward_message_and_update_cache,
        },
    },
    error::{ReceiveError, SendError},
};

/// Defines behaviours for processing messages from the old session
/// until the session transition period has passed.
pub struct OldSession<ProofsVerifier> {
    negotiated_peers: HashMap<PeerId, ConnectionId>,
    events: VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    waker: Option<Waker>,
    message_cache: MessageCache,
    poq_verifier: ProofsVerifier,
}

impl<ProofsVerifier> OldSession<ProofsVerifier>
where
    ProofsVerifier: encap::ProofsVerifier,
{
    /// Validates the public header of an encapsulated message, and
    /// if valid, forwards it to all negotiated peers minus the sender.
    pub fn validate_and_forward_message(
        &mut self,
        message: EncapsulatedMessage,
        except: PeerId,
    ) -> Result<(), SendError> {
        tracing::trace!(
            "Forwarding message with id {:?} to old session peers. Negotiated peers: {:?}. Excluded peer: {except:?}",
            hex::encode(message.id()),
            self.negotiated_peers
        );

        validate_forward_message_and_update_cache(
            message,
            &self.poq_verifier,
            self.negotiated_peers
                .iter()
                // Exclude the peer the message was received from.
                .filter(|(peer_id, _)| except != **peer_id),
            &mut self.events,
            &mut self.message_cache,
            self.waker.take(),
        )
    }

    pub(super) fn start_new_epoch(&mut self, new_pol_inputs: LeaderInputs) {
        self.poq_verifier.start_epoch_transition(new_pol_inputs);
    }

    pub(super) fn finish_epoch_transition(&mut self) {
        self.poq_verifier.complete_epoch_transition();
    }

    /// Handles a message received from a peer.
    ///
    /// # Returns
    /// - [`Ok(false)`] if the connection is not part of the session.
    /// - [`Ok(true)`] if the message was successfully processed and forwarded.
    /// - [`Err(Error)`] if the message is invalid or has already been
    ///   exchanged.
    pub fn handle_received_serialized_encapsulated_message(
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
            (from_peer_id, from_connection_id),
            &self.poq_verifier,
            &mut self.events,
            self.waker.take(),
        )?;

        Ok(true)
    }
}

impl<ProofsVerifier> OldSession<ProofsVerifier> {
    #[must_use]
    pub const fn new(
        negotiated_peers: HashMap<PeerId, ConnectionId>,
        message_cache: MessageCache,
        poq_verifier: ProofsVerifier,
    ) -> Self {
        Self {
            negotiated_peers,
            message_cache,
            events: VecDeque::new(),
            waker: None,
            poq_verifier,
        }
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
