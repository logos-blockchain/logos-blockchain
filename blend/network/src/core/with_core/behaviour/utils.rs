use core::{convert::Infallible, task::Waker};
use std::collections::VecDeque;

use either::Either;
use lb_blend_message::encap::{ProofsVerifier, encapsulated::EncapsulatedMessage};
use lb_blend_scheduling::{deserialize_encapsulated_message, serialize_encapsulated_message};
use libp2p::{
    PeerId,
    swarm::{ConnectionId, NotifyHandler, ToSwarm},
};

use crate::core::with_core::{
    behaviour::{Event, handler::FromBehaviour, message_cache::MessageCache},
    error::{ReceiveError, SendError},
};

pub fn validate_and_forward_message<'session, Verifier, PeerConnections>(
    message: EncapsulatedMessage,
    verifier: &Verifier,
    peer_connections: PeerConnections,
    events_queue: &'session mut VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    message_cache: &'session mut MessageCache,
    waker: Option<Waker>,
) -> Result<(), SendError>
where
    Verifier: ProofsVerifier,
    PeerConnections: Iterator<Item = (&'session PeerId, &'session ConnectionId)>,
{
    if message_cache.is_message_forwarded(&message) {
        return Err(SendError::DuplicateMessage);
    }

    let validated_message = message
        .verify_public_header(verifier)
        .map_err(|_| SendError::InvalidPublicHeader)?;
    let serialized_message = serialize_encapsulated_message(&validated_message);

    let mut at_least_one_receiver = false;
    peer_connections.for_each(|(peer_id, connection_id)| {
        events_queue.push_back(ToSwarm::NotifyHandler {
            peer_id: *peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(serialized_message.clone())),
        });
        at_least_one_receiver = true;
    });

    if at_least_one_receiver {
        // Mark the message as processed only if we were able to send it to at least one
        // of our peers.
        message_cache.mark_message_as_forwarded(&validated_message);
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    } else {
        Err(SendError::NoPeers)
    }
}

pub fn handle_received_serialized_encapsulated_message<Verifier>(
    serialized_message: &[u8],
    message_cache: &mut MessageCache,
    sender: (PeerId, ConnectionId),
    verifier: &Verifier,
    events_queue: &mut VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    waker: Option<Waker>,
) -> Result<(), ReceiveError>
where
    Verifier: ProofsVerifier,
{
    // Deserialize the message.
    let deserialized_encapsulated_message = deserialize_encapsulated_message(serialized_message)
        .map_err(|_| ReceiveError::UndeserializableMessage)?;

    // Add the message to the set of exchanged message identifiers with the sender,
    // returning `Err` if the message was already sent by this peer previously.
    if !message_cache.mark_message_as_seen_from_peer(&deserialized_encapsulated_message, sender.0) {
        return Err(ReceiveError::DuplicateMessageFromPeer(sender.0));
    }

    // Exit early if we've received this message already and we know it's a valid
    // one.
    if message_cache.is_message_processed(&deserialized_encapsulated_message) {
        return Ok(());
    }

    // Verify the message public header
    let validated_message = deserialized_encapsulated_message
        .verify_public_header(verifier)
        .map_err(|_| ReceiveError::InvalidPublicHeader)?;

    // Notify the swarm about the received message, so that it can be further
    // processed by the core protocol module.
    message_cache.mark_message_as_processed(&validated_message);
    events_queue.push_back(ToSwarm::GenerateEvent(Event::Message(
        Box::new(validated_message),
        sender,
    )));
    if let Some(waker) = waker {
        waker.wake();
    }

    Ok(())
}
