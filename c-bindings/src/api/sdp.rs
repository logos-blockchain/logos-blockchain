use lb_core::sdp::{self, DeclarationMessage};
use lb_node::{RuntimeServiceId, generic_services::SdpService};
use lb_sdp_service::SdpServiceApi;

use crate::{
    LogosBlockchainNode, OperationStatus, errors::OperationStatusCode, result::StatusResult,
};

pub(crate) fn post_declaration_sync(
    node: &LogosBlockchainNode,
    declaration: DeclarationMessage,
) -> StatusResult<sdp::DeclarationId> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        let api = SdpServiceApi::<SdpService<RuntimeServiceId>>::from_overwatch_handle(
            node.get_overwatch_handle(),
        )
        .await;
        api.post_declaration(declaration).await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to post declaration: {error}"),
            )
        })
    })
}
