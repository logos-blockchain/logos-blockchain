//! Crate error type.

/// Errors returned by the `λSQL` library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A participant state file or directory could not be accessed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The local `SQLite` database could not be opened or written.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// Local draft serialization failed.
    #[error("encoding error: {0}")]
    Encoding(#[from] bincode::Error),

    /// The zone sequencer reported an error.
    #[error("sequencer error: {0}")]
    Sequencer(#[from] lb_zone_sdk::sequencer::Error),

    /// A submitted transaction is outside the currently supported shape.
    #[error("invalid transaction: {0}")]
    InvalidTransaction(&'static str),

    /// A bound parameter is malformed.
    #[error("invalid SQL parameter: {0}")]
    InvalidParameter(&'static str),

    /// A bound parameter uses a `SQLite` representation that cannot be
    /// replayed.
    #[error("SQL parameter representation is not supported by \u{3bb}SQL")]
    UnsupportedParameter,

    /// Participant-local bookkeeping is missing or malformed.
    #[error("invalid local state: {0}")]
    InvalidLocalState(&'static str),

    /// An idempotency key was reused for different transaction content.
    #[error("idempotency key was already used for a different transaction")]
    IdempotencyConflict,

    /// A local write is committed but has not yet been accepted by `ZoneSDK`.
    #[error("a committed write is still waiting for ZoneSDK to accept it")]
    PublishPending,

    /// The encoded transaction does not conform to the `λSQL` protocol.
    #[error("invalid \u{3bb}SQL payload: {0}")]
    InvalidPayload(&'static str),

    /// The inscription uses a protocol version this library cannot execute.
    #[error("unsupported \u{3bb}SQL protocol version {0}")]
    UnsupportedProtocolVersion(u16),

    /// The encoded payload exceeds the inscription limit.
    #[error("transaction is too large for one inscription")]
    InscriptionTooLarge,

    /// No Tokio runtime is active on the calling thread.
    #[error("LogosSql::start must be called from within a Tokio runtime")]
    RuntimeUnavailable,

    /// The sequencer has not completed backfill and cannot accept writes yet.
    #[error("the zone sequencer is not ready to accept writes")]
    SequencerNotReady,

    /// Chain application has halted and new local writes are unsafe.
    #[error("\u{3bb}SQL runtime is halted while applying channel history")]
    RuntimeHalted,

    /// The participant runtime stopped before it could process a command.
    #[error("\u{3bb}SQL runtime is not running")]
    RuntimeStopped,

    /// The runtime task could not be joined.
    #[error("runtime task failed: {0}")]
    RuntimeJoin(#[from] tokio::task::JoinError),
}
