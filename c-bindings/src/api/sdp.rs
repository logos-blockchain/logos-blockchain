use lb_core::sdp::{self, DeclarationMessage};
use lb_node::{RuntimeServiceId, generic_services::SdpService};
use lb_sdp_service::SdpServiceApi;

use crate::{LogosBlockchainNode, OperationStatus};

pub(crate) fn post_declaration_sync(
    node: &LogosBlockchainNode,
    declaration: DeclarationMessage,
) -> Result<sdp::DeclarationId, OperationStatus> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        let api = SdpServiceApi::<SdpService<RuntimeServiceId>>::from_overwatch_handle(
            node.get_overwatch_handle(),
        )
        .await;
        api.post_declaration(declaration).await.map_err(|error| {
            log::error!("[blend_join_as_core_node] Failed to post declaration: {error}");
            OperationStatus::RelayError
        })
    })
}
