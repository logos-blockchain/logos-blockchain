//! Why an uncle carried by a block makes the block invalid.

use lb_core::block::HeaderError;

/// Why an uncle carried by a block fails the uncle validity rules,
/// making the block itself invalid.
#[derive(Debug, thiserror::Error)]
pub enum UncleError {
    #[error("not strictly older than the block")]
    NotStrictlyOlder,
    #[error("of another era than the block")]
    OtherEra,
    #[error("parent not on the chain that the block is extending, within the window")]
    ParentNotOnChain,
    #[error("on the chain that the block is extending")]
    OnChain,
    #[error("invalid uncle header")]
    InvalidHeader(#[from] HeaderError),
    #[error("invalid header signature")]
    InvalidSignature,
    #[error("invalid proof of leadership")]
    InvalidProof,
    #[error(transparent)]
    Other(lb_core::block::Error),
}

impl From<lb_core::block::Error> for UncleError {
    fn from(error: lb_core::block::Error) -> Self {
        match error {
            lb_core::block::Error::Header(e) => Self::InvalidHeader(e),
            lb_core::block::Error::Signature => Self::InvalidSignature,
            _ => Self::Other(error),
        }
    }
}
