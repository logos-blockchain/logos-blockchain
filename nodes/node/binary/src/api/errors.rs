use axum::{
    Json,
    response::{IntoResponse, Response},
};
use http::StatusCode;
use serde::Serialize;

use crate::api::calls::{self, DynError};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error(transparent)]
    Internal(#[from] DynError),
}

impl From<calls::Error> for ApiError {
    fn from(error: calls::Error) -> Self {
        match error {
            calls::Error::NotFound(message) => Self::NotFound(message),
            calls::Error::InvalidInput(message) => Self::BadRequest(message),
            calls::Error::Unavailable(error) | calls::Error::Internal(error) => {
                Self::Internal(error)
            }
        }
    }
}

impl IntoResponse for calls::Error {
    fn into_response(self) -> Response {
        ApiError::from(self).into_response()
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
            Self::Forbidden(message) => error_response(StatusCode::FORBIDDEN, message),
            Self::NotFound(message) => error_response(StatusCode::NOT_FOUND, message),
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
                ApiError::Forbidden("forbidden".into()),
                StatusCode::FORBIDDEN,
            ),
            (
                ApiError::NotFound("not found".into()),
                StatusCode::NOT_FOUND,
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
