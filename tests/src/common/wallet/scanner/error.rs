use thiserror::Error;

#[derive(Debug, Error)]
/// Errors produced while streaming blocks for the wallet scanner.
pub enum ScannerError {
    /// Error returned by the node HTTP client.
    #[error(transparent)]
    Http(#[from] lb_common_http_client::Error),
    /// Scanner-local invariant or publication error.
    #[error("scanner logical error: {0}")]
    Logical(String),
}
