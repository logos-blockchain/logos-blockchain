#![allow(
    clippy::redundant_pub_crate,
    reason = "Imported shared config modules expose pub(crate) constants."
)]

use std::time::Duration;

pub use lb_config::GeneralConfig;
pub(crate) use lb_config::{api, blend, consensus, network, sdp, time, tracing};
use lb_core::block::genesis::GenesisBlock;
use network::NetworkParams;

const PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(5);

#[must_use]
pub fn create_general_configs_from_ids(
    ids: &[[u8; 32]],
    blend_ports: &[u16],
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
    test_context: Option<&str>,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    lb_config::create_general_configs_from_ids(
        ids,
        blend_ports,
        n_blend_core_nodes,
        network_params,
        PROLONGED_BOOTSTRAP_PERIOD,
        test_context,
    )
}
