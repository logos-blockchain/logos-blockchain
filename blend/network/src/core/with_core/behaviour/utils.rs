use core::convert::Infallible;
use std::collections::VecDeque;

use either::Either;
use lb_blend_message::encap::{
    encapsulated::EncapsulatedMessage, validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_blend_scheduling::serialize_encapsulated_message;
use libp2p::{
    PeerId,
    swarm::{ConnectionId, NotifyHandler, ToSwarm},
};

use crate::core::with_core::{
    behaviour::{Event, handler::FromBehaviour, message_cache::MessageCache},
    error::SendError,
};

pub fn validate_and_forward_message<'session, ValidateMessageFn, PeerConnections, WakeFn>(
    message: EncapsulatedMessage,
    validate_message_fn: ValidateMessageFn,
    peer_connections: PeerConnections,
    events_queue: &'session mut VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    message_cache: &'session mut MessageCache,
    mut wake_fn: WakeFn,
) -> Result<(), SendError>
where
    ValidateMessageFn:
        FnOnce(EncapsulatedMessage) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, ()>,
    PeerConnections: Iterator<Item = (&'session PeerId, &'session ConnectionId)>,
    WakeFn: FnMut(),
{
    if message_cache.is_message_forwarded(&message) {
        return Err(SendError::DuplicateMessage);
    }

    let validated_message = validate_message_fn(message).map_err(|()| SendError::InvalidMessage)?;
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
        wake_fn();
        Ok(())
    } else {
        Err(SendError::NoPeers)
    }
}
