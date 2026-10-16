use thiserror::Error;

#[derive(Debug, Error)]
/// Errors produced while streaming blocks for the wallet scanner.
pub enum ScannerError {
    /// Error returned by the node HTTP client.
    #[error(transparent)]
    Http(#[from] lb_common_http_client::Error),
    /// A streamed block's ledger events are unavailable. The scanner needs
    /// those events to apply finalized SDP note unlocks.
    #[error("scanner logical error: block events missing for {0}")]
    MissingBlockEvents(lb_core::header::HeaderId),
    /// Scanner-local invariant or publication error.
    #[error("scanner logical error: {0}")]
    Logical(String),
}
