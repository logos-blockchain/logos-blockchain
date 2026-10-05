mod encoding;
mod keys;
mod sign;

pub use keys::{SigningKeyIds, SigningKeyRole};
pub use sign::{FfiSignedMessageResult, SignedMessage, free_signed_message, sign_message};
