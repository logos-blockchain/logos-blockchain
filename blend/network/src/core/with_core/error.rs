#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Error {
    /// There were no peers to send a message to.
    NoPeers,
    InvalidMessage,
    MessageAlreadyExchanged,
}
