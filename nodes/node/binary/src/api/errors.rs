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
    #[error("Internal server error")]
    InternalJson(serde_json::Value),
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

    pub const fn internal_json(body: serde_json::Value) -> Self {
        Self::InternalJson(body)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message).into_response(),
            Self::NotFoundEmpty => (StatusCode::NOT_FOUND,).into_response(),
            error @ Self::InternalServerError => {
                (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
            }
            Self::InternalJson(body) => {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
            }
            Self::Internal(error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
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

    #[tokio::test]
    async fn generic_internal_error_preserves_existing_response() {
        let response = ApiError::InternalServerError.into_response();
        let status = response.status();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "Internal server error");
    }

    #[tokio::test]
    async fn internal_error_preserves_existing_response() {
        let response = ApiError::from(DynError::from("service unavailable")).into_response();
        let status = response.status();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "service unavailable");
    }

    #[tokio::test]
    async fn empty_not_found_preserves_existing_response() {
        let response = ApiError::NotFoundEmpty.into_response();
        let status = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type, None);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn json_error_preserves_existing_response() {
        let response =
            ApiError::internal_json(serde_json::json!({ "error": "failure" })).into_response();
        let status = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            content_type.as_ref().and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(body, "{\"error\":\"failure\"}");
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
