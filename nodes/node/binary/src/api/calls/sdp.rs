use std::collections::HashMap;

use lb_core::sdp::{ActivityMetadata, Declaration, DeclarationId, DeclarationMessage, ServiceType};
use lb_sdp_service::{SdpMessage, SdpSubmission};

use super::{Error, Handle, cryptarchia::chain_api, request};
use crate::{RuntimeServiceId, generic_services};

type SdpService = generic_services::SdpService<RuntimeServiceId>;

pub async fn submit_declaration(
    handle: &Handle,
    declaration: DeclarationMessage,
) -> Result<SdpSubmission, Error> {
    Ok(
        request::<SdpService, _>(handle, |reply_channel| SdpMessage::PostDeclaration {
            declaration: Box::new(declaration),
            reply_channel,
        })
        .await??,
    )
}

pub async fn submit_activity(
    handle: &Handle,
    metadata: ActivityMetadata,
) -> Result<SdpSubmission, Error> {
    Ok(
        request::<SdpService, _>(handle, |reply_channel| SdpMessage::PostActivity {
            metadata,
            reply_channel: Some(reply_channel),
        })
        .await??,
    )
}

pub async fn submit_withdrawal(
    handle: &Handle,
    declaration_id: DeclarationId,
) -> Result<SdpSubmission, Error> {
    Ok(
        request::<SdpService, _>(handle, |reply_channel| SdpMessage::PostWithdrawal {
            declaration_id,
            reply_channel,
        })
        .await??,
    )
}

pub async fn set_declaration_id(
    handle: &Handle,
    declaration_id: Option<DeclarationId>,
) -> Result<(), Error> {
    request::<SdpService, _>(handle, |reply_channel| {
        SdpMessage::SetCurrentDeclarationId {
            declaration_id,
            reply_channel,
        }
    })
    .await?
    .map_err(Error::internal)
}

pub async fn get_declarations(
    handle: &Handle,
) -> Result<HashMap<DeclarationId, Declaration>, Error> {
    Ok(chain_api(handle).await.get_sdp_declarations().await?)
}

pub async fn get_snapshot(handle: &Handle) -> Result<HashMap<DeclarationId, Declaration>, Error> {
    Ok(chain_api(handle).await.get_sdp_snapshot().await?)
}

/// One declaration from finalized (LIB) SDP state.
pub async fn get_finalized_declaration(
    handle: &Handle,
    declaration_id: DeclarationId,
) -> Result<Declaration, Error> {
    chain_api(handle)
        .await
        .get_finalized_sdp_declaration(declaration_id)
        .await?
        .ok_or_else(|| Error::NotFound("Declaration not found in finalized state".into()))
}

/// All declarations of one service from finalized (LIB) SDP state.
pub async fn get_finalized_declarations(
    handle: &Handle,
    service_type: ServiceType,
) -> Result<HashMap<DeclarationId, Declaration>, Error> {
    chain_api(handle)
        .await
        .get_finalized_sdp_declarations(service_type)
        .await?
        .ok_or_else(|| Error::NotFound("Service absent from finalized state".into()))
}
