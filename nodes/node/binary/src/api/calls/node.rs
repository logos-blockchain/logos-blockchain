use lb_core::mantle::transactions::genesis_tx::ChainId;
use lb_tracing::filter::envfilter::EnvFilterConfig;
use lb_tracing_service::{TracingMessage, TracingOpenMetricsError};

use super::{Error, Handle, request};
use crate::TracingService;

/// The version and build provenance of the node.
#[must_use]
pub fn version() -> lb_version::BuildVersionInfo {
    lb_version::build_version_info()
}

/// The chain the node runs on.
#[must_use]
pub fn chain_id(handle: &Handle) -> ChainId {
    handle.chain_id.clone()
}

pub async fn reload_tracing_filter(handle: &Handle, filter: EnvFilterConfig) -> Result<(), Error> {
    request::<TracingService, _>(handle, |reply_channel| TracingMessage::ReloadFilter {
        filter,
        reply_channel,
    })
    .await?
    .map_err(Error::internal)
}

/// The node's metrics in the `OpenMetrics` text format.
pub async fn open_metrics(handle: &Handle) -> Result<String, Error> {
    request::<TracingService, _>(handle, |reply_channel| TracingMessage::ReadOpenMetrics {
        reply_channel,
    })
    .await?
    .map_err(|error| match error {
        TracingOpenMetricsError::NotEnabled => Error::NotFound(error.to_string()),
        TracingOpenMetricsError::Read(_) => Error::internal(error.to_string()),
    })
}
