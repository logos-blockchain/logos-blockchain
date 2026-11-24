use lb_node::{RuntimeServiceId, generic_services::SdpService};
use lb_sdp_service::SdpMessage;
use overwatch::overwatch::OverwatchHandle;
use tokio::sync::oneshot;

use crate::{
    OperationStatus, OperationStatusCode, StatusResult, api::common::overwatch::ensure_ready,
};

type NodeSdpService = SdpService<RuntimeServiceId>;

pub(super) async fn make_sdp_request<Response>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    message_builder: impl FnOnce(oneshot::Sender<Response>) -> SdpMessage,
) -> StatusResult<Response> {
    ensure_ready::<NodeSdpService>(overwatch_handle).await?;
    let relay = overwatch_handle
        .relay::<NodeSdpService>()
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get SDP relay: {error}"),
            )
        })?;
    let (sender, receiver) = oneshot::channel();
    let message = message_builder(sender);
    relay.send(message).await.map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::ChannelSendError,
            format!("Failed to send to the SDP service: {error}"),
        )
    })?;
    receiver.await.map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::ChannelReceiveError,
            format!("Failed to receive from the SDP service: {error}"),
        )
    })
}
