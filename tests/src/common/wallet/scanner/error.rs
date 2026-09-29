use thiserror::Error;

#[derive(Debug, Error)]
/// Errors produced while streaming blocks for the wallet scanner.
pub enum ScannerError {
    /// Error returned by the node HTTP client.
    #[error(transparent)]
    Http(#[from] lb_common_http_client::Error),
    /// A block was streamed but its ledger events were unavailable. Continuing
    /// without them could leave a finalized SDP note permanently locked.
    #[error("scanner logical error: block events missing for {0}")]
    MissingBlockEvents(lb_core::header::HeaderId),
    /// Scanner-local invariant or publication error.
    #[error("scanner logical error: {0}")]
    Logical(String),
}
