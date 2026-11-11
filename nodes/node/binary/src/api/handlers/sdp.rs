use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse as _, Response},
};
use lb_api_common::paths;
use lb_core::sdp::{DeclarationId, ServiceType};

use crate::api::{
    calls::{Handle, sdp},
    errors::{ApiError, ErrorBody},
};

#[utoipa::path(
    post,
    path = paths::SDP_POST_DECLARATION,
    responses(
        (status = 200, description = "Post declaration to SDP service", body = lb_sdp_service::SdpSubmission),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_declaration(
    State(handle): State<Handle>,
    Json(declaration): Json<lb_core::sdp::DeclarationMessage>,
) -> Response {
    make_request_and_return_response!(sdp::submit_declaration(&handle, declaration))
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_ACTIVITY,
    responses(
        (status = 200, description = "Post activity to SDP service", body = lb_sdp_service::SdpSubmission),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_activity(
    State(handle): State<Handle>,
    Json(metadata): Json<lb_core::sdp::ActivityMetadata>,
) -> Response {
    make_request_and_return_response!(sdp::submit_activity(&handle, metadata))
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_WITHDRAWAL,
    responses(
        (status = 200, description = "Post withdrawal to SDP service", body = lb_sdp_service::SdpSubmission),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_withdrawal(
    State(handle): State<Handle>,
    Json(declaration_id): Json<DeclarationId>,
) -> Response {
    make_request_and_return_response!(sdp::submit_withdrawal(&handle, declaration_id))
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_SET_DECLARATION_ID,
    responses(
        (status = 200, description = "Post declaration to SDP service to be set as current", body = DeclarationId),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_set_declaration_id(
    State(handle): State<Handle>,
    Json(declaration): Json<Option<DeclarationId>>,
) -> Response {
    make_request_and_return_response!(sdp::set_declaration_id(&handle, declaration))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_SDP_DECLARATIONS,
    responses(
        (status = 200, description = "Get current SDP declarations keyed by declaration id", body = std::collections::HashMap<DeclarationId, Object>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_sdp_declarations(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(sdp::get_declarations(&handle))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_SDP_SNAPSHOT,
    responses(
        (status = 200, description = "Get the SDP snapshot for the current epoch keyed by declaration id", body = std::collections::HashMap<DeclarationId, Object>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_sdp_snapshot(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(sdp::get_snapshot(&handle))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_SDP_FINALIZED_DECLARATION,
    params(
        ("declaration_id" = String, Path, description = "Declaration ID as 64 hex digits, optionally prefixed with 0x")
    ),
    responses(
        (status = 200, description = "Declaration from finalized SDP state", body = Object),
        (status = 400, description = "Invalid declaration ID", body = ErrorBody),
        (status = 404, description = "Declaration not found in finalized state", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_finalized_sdp_declaration(
    State(handle): State<Handle>,
    Path(declaration_id): Path<String>,
) -> Response {
    let declaration_id = match declaration_id.parse::<DeclarationId>() {
        Ok(id) => id,
        Err(error) => return ApiError::BadRequest(error).into_response(),
    };

    make_request_and_return_response!(sdp::get_finalized_declaration(&handle, declaration_id))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_SDP_FINALIZED_DECLARATIONS,
    params(
        ("service_type" = u8, Path, description = "Canonical one-byte ServiceType discriminant")
    ),
    responses(
        (status = 200, description = "Declarations of the service from finalized SDP state", body = std::collections::HashMap<DeclarationId, Object>),
        (status = 400, description = "Unknown service type", body = ErrorBody),
        (status = 404, description = "Service is absent from finalized state", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_finalized_sdp_declarations(
    State(handle): State<Handle>,
    Path(service_discriminant): Path<u8>,
) -> Response {
    let Ok(service_type) = ServiceType::try_from(service_discriminant) else {
        return ApiError::BadRequest("Unknown service type".into()).into_response();
    };

    make_request_and_return_response!(sdp::get_finalized_declarations(&handle, service_type))
}
