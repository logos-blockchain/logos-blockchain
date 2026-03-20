use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::ops::channel::{ChannelId, ChannelKeyIndex};
use lb_ledger::mantle::helpers::MantleOperationVerificationHelper;

pub async fn get_withdraw_threshold_for_channel(
    _client: &CommonHttpClient,
    _channel_id: &ChannelId,
) -> Result<ChannelKeyIndex, reqwest::Error> {
    todo!("The endpoint is not yet available.");
}


pub async fn get_operation_verification_helper() -> MantleOperationVerificationHelper<'static> {
    todo!("The endpoint is not yet available.");
}
