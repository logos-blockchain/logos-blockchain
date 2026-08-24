use core::{convert::Infallible, num::NonZeroU64, task::Waker};
use std::{collections::VecDeque, sync::Arc};

use either::Either;
use lb_blend_message::encap::{
    ProofsVerifier, validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_blend_scheduling::{
    deserialize_encapsulated_message, serialize_encapsulated_message_with_verified_public_header,
};
use lb_cryptarchia_engine::Epoch;
use libp2p::{
    PeerId,
    swarm::{ConnectionId, NotifyHandler, ToSwarm},
};

use crate::core::{
    poq_verification::{PendingPoQVerifications, spawn_poq_verification},
    with_core::{
        behaviour::{Event, handler::FromBehaviour, message_cache::MessageCache},
        error::{ReceiveError, SendError},
    },
};

/// Forwards a message with a verified public header to the given peer
/// connections, if it hasn't been forwarded already.
///
/// The message cache is also updated accordingly to mark the sent message as
/// processed if it was sent to at least one peer, or to ignore it if it has
/// already been forwarded before.
///
/// The input type is [`EncapsulatedMessageWithVerifiedPublicHeader`] because a
/// message is relayed to the rest of the network only after its `PoQ` has been
/// verified, which happens in the Blend service.
pub fn forward_validated_message_and_update_cache<'epoch, PeerConnections>(
    message: &EncapsulatedMessageWithVerifiedPublicHeader,
    peer_connections: PeerConnections,
    events_queue: &'epoch mut VecDeque<ToSwarm<Event, Either<FromBehaviour, Infallible>>>,
    message_cache: &'epoch mut MessageCache,
    waker: &mut Option<Waker>,
) -> Result<(), SendError>
where
    PeerConnections: Iterator<Item = (&'epoch PeerId, &'epoch ConnectionId)>,
{
    if message_cache.is_message_forwarded(&message.clone().into()) {
        return Err(SendError::DuplicateMessage);
    }

    let mut peer_connections = peer_connections.peekable();
    if peer_connections.peek().is_none() {
        return Err(SendError::NoPeers);
    }

    let serialized_message = serialize_encapsulated_message_with_verified_public_header(message);

    peer_connections.for_each(|(peer_id, connection_id)| {
        tracing::trace!("Notifying handler with peer {peer_id:?} on connection {connection_id:?} to deliver message.");
        events_queue.push_back(ToSwarm::NotifyHandler {
            peer_id: *peer_id,
            handler: NotifyHandler::One(*connection_id),
            event: Either::Left(FromBehaviour::Message(serialized_message.clone())),
        });
    });

    message_cache.mark_message_as_forwarded(message);
    if let Some(waker) = waker.take() {
        waker.wake();
    }
    Ok(())
}

/// Validates the signature of a received message and dispatches the
/// verification of its `PoQ`, if it hasn't been processed already.
///
/// The message cache is updated accordingly to mark the message as processed if
/// it is valid and hasn't been processed before, or to ignore it if it has
/// already been processed before. If the message is a duplicate of a previously
/// received message from the same peer, it is also ignored and an error is
/// returned to avoid processing the same message multiple times from the same
/// peer, which could be a sign of a malicious peer.
///
/// The message is only reported to the swarm — and only entered into the
/// message cache — once its `PoQ` verifies, which happens off the task polling
/// this behaviour. Entering it any earlier would let anyone claim a nullifier
/// by replaying someone else's `PoQ` under their own signing key, suppressing
/// the genuine message that carries it.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
pub fn handle_received_serialized_encapsulated_message_and_update_cache<Verifier>(
    serialized_message: &[u8],
    message_cache: &mut MessageCache,
    (sender, connection_id): (PeerId, ConnectionId),
    pending_verifications: &PendingPoQVerifications,
    waker: &mut Option<Waker>,
    epoch: Epoch,
    num_blend_layers: NonZeroU64,
    proofs_verifier: &Arc<Verifier>,
) -> Result<(), ReceiveError>
where
    Verifier: ProofsVerifier + Send + Sync + 'static,
{
    // Deserialize the message.
    let deserialized_encapsulated_message =
        deserialize_encapsulated_message(serialized_message, &num_blend_layers)
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

    // Verify the message signature
    let validated_message = deserialized_encapsulated_message
        .verify_header_signature()
        .map_err(|_| ReceiveError::InvalidHeaderSignature)?;

    // Verify the `PoQ` before the message is reported to the swarm, entered into
    // the cache, and hence before it can be relayed any further.
    spawn_poq_verification(
        pending_verifications,
        validated_message,
        (sender, connection_id),
        epoch,
        proofs_verifier,
        waker,
    );

    Ok(())
}
