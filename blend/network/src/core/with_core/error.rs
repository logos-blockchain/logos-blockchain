use libp2p::PeerId;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SendError {
    /// There were no peers to send a message to.
    NoPeers,
    /// The message being sent has an invalid public header.
    InvalidPublicHeader,
    /// The message being sent is a duplicate of a previous sent message.
    DuplicateMessage,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReceiveError {
    /// The received payload is not deserializable into an
    /// `EncapsulatedMessage`.
    UndeserializableMessage,
    /// The message being received has an invalid public header.
    InvalidPublicHeader,
    /// The message being received is a duplicate of a previous received
    /// message.
    DuplicateMessageFromPeer(PeerId),
}
