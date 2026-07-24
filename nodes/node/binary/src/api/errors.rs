use axum::{
    Json,
    response::{IntoResponse, Response},
};
use http::StatusCode;
use lb_api_service::http::DynError;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("Not found")]
    NotFoundEmpty,
    #[error("Internal server error")]
    InternalServerError,
    #[error(transparent)]
    Internal(#[from] DynError),
}

impl ApiError {
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }

    pub fn internal_message(message: impl Into<String>) -> Self {
        Self::Internal(DynError::from(message.into()))
    }
}

/// Body returned for every API error response, mirroring the
/// `{code, message}` envelope used by the Ethereum Beacon API.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    pub code: u16,
    pub message: String,
}

fn error_response(status: StatusCode, message: String) -> Response {
    (
        status,
        Json(ErrorBody {
            code: status.as_u16(),
            message,
        }),
    )
        .into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => error_response(StatusCode::BAD_REQUEST, message),
            Self::NotFound(message) => error_response(StatusCode::NOT_FOUND, message),
            error @ Self::NotFoundEmpty => error_response(StatusCode::NOT_FOUND, error.to_string()),
            error @ Self::InternalServerError => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            }
            Self::Internal(error) => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            }
        }
    }
}

pub fn json_response<T, E>(result: Result<T, E>) -> Response
where
    T: Serialize,
    E: Into<ApiError>,
{
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into().into_response(),
    }
}

impl IntoResponse for BlocksStreamRequestError {
    fn into_response(self) -> Response {
        ApiError::BadRequest(self.to_string()).into_response()
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
        ApiError::BadRequest(self.to_string()).into_response()
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
            Self::Internal(err) => ApiError::Internal(err).into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body;
    use http::header::CONTENT_TYPE;

    use super::*;

    #[tokio::test]
    async fn api_error_maps_variants_to_status_codes() {
        let cases = [
            (
                ApiError::BadRequest("bad request".into()),
                StatusCode::BAD_REQUEST,
            ),
            (
                ApiError::NotFound("not found".into()),
                StatusCode::NOT_FOUND,
            ),
            (ApiError::NotFoundEmpty, StatusCode::NOT_FOUND),
            (
                ApiError::InternalServerError,
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected_status) in cases {
            assert_eq!(error.into_response().status(), expected_status);
        }
    }

    async fn envelope_of(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");

        assert_eq!(content_type.as_deref(), Some("application/json"));
        let envelope = serde_json::from_slice(&body).expect("body should be valid JSON");
        (status, envelope)
    }

    #[tokio::test]
    async fn bad_request_returns_json_envelope() {
        let response = ApiError::BadRequest("invalid query".into()).into_response();
        let (status, envelope) = envelope_of(response).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            envelope,
            serde_json::json!({ "code": 400, "message": "invalid query" })
        );
    }

    #[tokio::test]
    async fn not_found_returns_json_envelope() {
        let response = ApiError::NotFound("Block not found".into()).into_response();
        let (status, envelope) = envelope_of(response).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            envelope,
            serde_json::json!({ "code": 404, "message": "Block not found" })
        );
    }

    #[tokio::test]
    async fn generic_internal_error_returns_json_envelope() {
        let response = ApiError::InternalServerError.into_response();
        let (status, envelope) = envelope_of(response).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            envelope,
            serde_json::json!({ "code": 500, "message": "Internal server error" })
        );
    }

    #[tokio::test]
    async fn internal_error_returns_json_envelope() {
        let response = ApiError::from(DynError::from("service unavailable")).into_response();
        let (status, envelope) = envelope_of(response).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            envelope,
            serde_json::json!({ "code": 500, "message": "service unavailable" })
        );
    }

    #[tokio::test]
    async fn empty_not_found_returns_json_envelope() {
        let response = ApiError::NotFoundEmpty.into_response();
        let (status, envelope) = envelope_of(response).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            envelope,
            serde_json::json!({ "code": 404, "message": "Not found" })
        );
    }

    #[tokio::test]
    async fn json_response_preserves_success_response() {
        let response = json_response::<_, ApiError>(Ok(vec![1, 2, 3]));
        let status = response.status();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "[1,2,3]");
    }
}
