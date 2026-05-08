use axum::response::{IntoResponse, Response};
use http::StatusCode;
use lb_api_service::http::DynError;

impl IntoResponse for BlocksStreamRequestError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.to_string()).into_response()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlocksStreamRequestError {
    #[error("invalid query: {0}")]
    Validation(#[from] validator::ValidationErrors),
    #[error("'slot_from' must be <= 'slot_to', got slot_from={slot_from}, slot_to={slot_to}")]
    InvalidSlotRange { slot_from: u64, slot_to: u64 },
}

/// Errors that can occur during resolving the blocks stream window from the
/// request and chain info.
#[derive(Debug, thiserror::Error)]
pub enum BlocksStreamWindowError {
    #[error("'slot_to' must be <= {anchor}, got slot_to={slot_to}, {anchor}={max_slot_to}")]
    SlotToAboveAnchor {
        anchor: &'static str,
        slot_to: u64,
        max_slot_to: u64,
    },
    #[error("'slot_from' must be <= 'slot_to', got slot_from={slot_from}, slot_to={slot_to}")]
    SlotFromAboveSlotTo { slot_from: u64, slot_to: u64 },
}

impl IntoResponse for BlocksStreamWindowError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.to_string()).into_response()
    }
}

/// Error type for blocks stream handler. We need a custom error type to
/// distinguish between different error cases and return appropriate HTTP status
/// codes.
#[derive(Debug, thiserror::Error)]
pub enum BlocksStreamHandlerError {
    #[error(transparent)]
    Query(#[from] BlocksStreamRequestError),
    #[error(transparent)]
    InvalidWindow(#[from] BlocksStreamWindowError),
    #[error(transparent)]
    Internal(#[from] DynError),
}

impl IntoResponse for BlocksStreamHandlerError {
    fn into_response(self) -> Response {
        match self {
            Self::Query(err) => err.into_response(),
            Self::InvalidWindow(err) => err.into_response(),
            Self::Internal(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        }
    }
}
