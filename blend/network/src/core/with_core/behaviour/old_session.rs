use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    convert::Infallible,
    task::{Context, Poll, Waker},
    time::Instant,
};

use either::Either;
use lb_blend_message::{
    MessageIdentifier,
    encap::{
        self, encapsulated::EncapsulatedMessage,
        validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
};
use lb_blend_proofs::quota::inputs::prove::public::LeaderInputs;
use lb_blend_scheduling::{deserialize_encapsulated_message, serialize_encapsulated_message};
use libp2p::{
    PeerId,
    swarm::{ConnectionId, NotifyHandler, ToSwarm},
};

use crate::core::with_core::{
    behaviour::{Event, SENSITIVITY_INTERVAL_FOR_DUPLICATES, handler::FromBehaviour},
    error::Error,
};

/// Defines behaviours for processing messages from the old session
/// until the session transition period has passed.
pub struct OldSession<ProofsVerifier> {
    negotiated_peers: HashMap<PeerId, ConnectionId>,
    exchanged_message_identifiers: HashMap<PeerId, HashMap<MessageIdentifier, Instant>>,
    events: VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    waker: Option<Waker>,
    poq_verifier: ProofsVerifier,
}

impl<ProofsVerifier> OldSession<ProofsVerifier>
where
    ProofsVerifier: encap::ProofsVerifier,
{
    /// Validates the public header of an encapsulated message, and
    /// if valid, forwards it to all negotiated peers.
    pub fn validate_and_publish_message(
        &mut self,
        message: EncapsulatedMessage,
    ) -> Result<(), Error> {
        let validated_message = self.verify_encapsulated_message_public_header(message)?;
        self.forward_validated_message_and_maybe_exclude(&validated_message, None)
    }

    fn verify_encapsulated_message_public_header(
        &self,
        message: EncapsulatedMessage,
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        message
            .verify_public_header(&self.poq_verifier)
            .map_err(|_| Error::InvalidMessage)
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
    ) -> Result<bool, Error> {
        if !self.is_negotiated(&(from_peer_id, from_connection_id)) {
            return Ok(false);
        }

        // Deserialize the message.
        let deserialized_encapsulated_message =
            deserialize_encapsulated_message(serialized_message)
                .map_err(|_| Error::InvalidMessage)?;

        let message_identifier = deserialized_encapsulated_message.id();

        // Add the message to the set of exchanged message identifiers.
        check_and_update_message_cache(
            &mut self.exchanged_message_identifiers,
            &message_identifier,
            from_peer_id,
        )?;

        // Verify the message public header
        let validated_message =
            self.verify_encapsulated_message_public_header(deserialized_encapsulated_message)?;

        // Notify the swarm about the received message, so that it can be further
        // processed by the core protocol module.
        self.events.push_back(ToSwarm::GenerateEvent(Event::Message(
            Box::new(validated_message),
            (from_peer_id, from_connection_id),
        )));
        self.try_wake();
        Ok(true)
    }
}

impl<ProofsVerifier> OldSession<ProofsVerifier> {
    #[must_use]
    pub const fn new(
        negotiated_peers: HashMap<PeerId, ConnectionId>,
        exchanged_message_identifiers: HashMap<PeerId, HashMap<MessageIdentifier, Instant>>,
        poq_verifier: ProofsVerifier,
    ) -> Self {
        Self {
            negotiated_peers,
            exchanged_message_identifiers,
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

    /// Forwards a message to all connections except the [`except`] connection.
    ///
    /// Public header validation checks are skipped, since the message is
    /// assumed to have been properly formed.
    fn forward_validated_message_and_maybe_exclude(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
        except: Option<PeerId>,
    ) -> Result<(), Error> {
        let message_id = message.id();
        let serialized_message = serialize_encapsulated_message(message);
        let mut at_least_one_receiver = false;
        self.negotiated_peers
            .iter()
            .filter(|(peer_id, _)| Some(**peer_id) != except)
            .for_each(|(&peer_id, &connection_id)| {
                if check_and_update_message_cache(
                    &mut self.exchanged_message_identifiers,
                    &message_id,
                    peer_id,
                )
                .is_ok()
                {
                    self.events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: NotifyHandler::One(connection_id),
                        event: Either::Left(FromBehaviour::Message(serialized_message.clone())),
                    });
                    at_least_one_receiver = true;
                }
            });

        if at_least_one_receiver {
            self.try_wake();
            Ok(())
        } else {
            Err(Error::NoPeers)
        }
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
            self.exchanged_message_identifiers.remove(peer_id);
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

fn check_and_update_message_cache(
    exchanged_message_identifiers: &mut HashMap<PeerId, HashMap<MessageIdentifier, Instant>>,
    message_id: &MessageIdentifier,
    peer_id: PeerId,
) -> Result<(), Error> {
    match exchanged_message_identifiers
        .entry(peer_id)
        .or_default()
        .entry(*message_id)
    {
        Entry::Vacant(vacant_message_entry) => {
            vacant_message_entry.insert(Instant::now());
            Ok(())
        }
        Entry::Occupied(occupied_message_entry) => {
            let time_sent = occupied_message_entry.get();
            if Instant::now().duration_since(*time_sent) <= SENSITIVITY_INTERVAL_FOR_DUPLICATES {
                Ok(())
            } else {
                Err(Error::MessageAlreadyExchanged)
            }
        }
    }
}
