use lb_core::{
    mantle::NoteId,
    sdp,
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::{RuntimeServiceId, generic_services::SdpService};
use lb_sdp_service::SdpServiceApi;

use crate::{LogosBlockchainNode, OperationStatus};

pub(crate) fn post_declaration_sync(
    node: &LogosBlockchainNode,
    provider_id: ProviderId,
    zk_id: ZkPublicKey,
    locked_note_id: NoteId,
    locators: Vec<Locator>,
) -> Result<sdp::DeclarationId, OperationStatus> {
    let declaration = DeclarationMessage {
        service_type: ServiceType::BlendNetwork,
        locators,
        provider_id,
        zk_id,
        locked_note_id,
    };
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
