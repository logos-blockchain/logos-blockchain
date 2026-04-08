use core::{convert::Infallible, task::Waker};
use std::collections::VecDeque;

use either::Either;
use lb_blend_message::encap::validated::EncapsulatedMessageWithVerifiedSignature;
use lb_blend_scheduling::{
    deserialize_encapsulated_message, serialize_encapsulated_message_with_verified_signature,
};
use libp2p::{
    PeerId,
    swarm::{ConnectionId, NotifyHandler, ToSwarm},
};

use crate::core::with_core::{
    behaviour::{Event, handler::FromBehaviour, message_cache::MessageCache},
    error::{ReceiveError, SendError},
};

/// Forwards a message with a valid signature to the given peer connections, if
/// it hasn't been forwarded already.
///
/// The message cache is also updated accordingly to mark the sent message as
/// processed if it was sent to at least one peer, or to ignore it if it has
/// already been forwarded before.
pub fn forward_validated_message_and_update_cache<'session, PeerConnections>(
    message: &EncapsulatedMessageWithVerifiedSignature,
    peer_connections: PeerConnections,
    events_queue: &'session mut VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    message_cache: &'session mut MessageCache,
    waker: Option<Waker>,
) -> Result<(), SendError>
where
    PeerConnections: Iterator<Item = (&'session PeerId, &'session ConnectionId)>,
{
    if message_cache.is_message_forwarded(&message.clone().into()) {
        return Err(SendError::DuplicateMessage);
    }

    let mut peer_connections = peer_connections.peekable();
    if peer_connections.peek().is_none() {
        return Err(SendError::NoPeers);
    }

    let serialized_message = serialize_encapsulated_message_with_verified_signature(message);

    peer_connections.for_each(|(peer_id, connection_id)| {
        tracing::trace!("Notifying handler with peer {peer_id:?} on connection {connection_id:?} to deliver message.");
        events_queue.push_back(ToSwarm::NotifyHandler {
            peer_id: *peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(serialized_message.clone())),
        });
    });

    message_cache.mark_message_as_forwarded(message);
    if let Some(waker) = waker {
        waker.wake();
    }
    Ok(())
}

/// Validates the signature of a received message, and notifies the swarm about
/// it if it hasn't been processed already.
///
/// The message cache is updated accordingly to mark the message as processed if
/// it is valid and hasn't been processed before, or to ignore it if it has
/// already been processed before. If the message is a duplicate of a previously
/// received message from the same peer, it is also ignored and an error is
/// returned to avoid processing the same message multiple times from the same
/// peer, which could be a sign of a malicious peer.
pub fn handle_received_serialized_encapsulated_message_and_update_cache(
    serialized_message: &[u8],
    message_cache: &mut MessageCache,
    sender: PeerId,
    events_queue: &mut VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    waker: Option<Waker>,
    session_number: u64,
) -> Result<(), ReceiveError> {
    // Deserialize the message.
    let deserialized_encapsulated_message = deserialize_encapsulated_message(serialized_message)
        .map_err(|_| ReceiveError::UndeserializableMessage)?;

    // Add the message to the set of exchanged message identifiers with the sender,
    // returning `Err` if the message was already sent by this peer previously.
    if !message_cache.mark_message_as_seen_from_peer(&deserialized_encapsulated_message, sender) {
        return Err(ReceiveError::DuplicateMessageFromPeer(sender));
    }

    // Exit early if we've received this message already and we know it's a valid
    // one.
    if message_cache.is_message_processed(&deserialized_encapsulated_message) {
        return Ok(());
    }

    // Verify the message public header
    let validated_message = deserialized_encapsulated_message
        .verify_header_signature()
        .map_err(|_| ReceiveError::InvalidHeaderSignature)?;

    // Notify the swarm about the received message, so that it can be further
    // processed by the core protocol module.
    message_cache.mark_message_as_processed(&validated_message);
    events_queue.push_back(ToSwarm::GenerateEvent(Event::Message {
        message: Box::new(validated_message),
        sender,
        session: session_number,
    }));
    if let Some(waker) = waker {
        waker.wake();
    }

    Ok(())
}
