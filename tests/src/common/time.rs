use std::{
    ops::{Add as _, Mul as _},
    time::Duration,
};

use lb_node::config::{blend, deployment::DeploymentSettings};

/// Calculates the maximum time required for `num_blocks` blocks to be proposed
/// and fully propagated across the network.
///
/// `margin_factor` is a multiplier to add some margin to the calculated time.
#[must_use]
pub fn max_block_propagation_time(
    num_blocks: u32,
    blend_network_size: u64,
    deployment: &DeploymentSettings,
    margin_factor: f64,
) -> Duration {
    let proposal_interval = deployment
        .time
        .slot_duration
        .div_f64(deployment.cryptarchia.slot_activation_coeff.as_f64());

    let blend_latency = max_blend_latency_per_block(blend_network_size, &deployment.blend);

    let broadcast_latency = Duration::from_secs(1);

    proposal_interval
        .add(blend_latency)
        .add(broadcast_latency)
        .mul(num_blocks)
        .mul_f64(margin_factor)
}

/// Calculates the maximum time for a block to be fully blended.
/// This ignores the gossiping latency in the blend network.
fn max_blend_latency_per_block(
    network_size: u64,
    deployment: &blend::deployment::Settings,
) -> Duration {
    if network_size < deployment.common.minimum_network_size.get() {
        return Duration::ZERO;
    }

    deployment
        .common
        .timing
        .round_duration
        .mul(
            deployment
                .core
                .scheduler
                .delayer
                .maximum_release_delay_in_rounds
                .get()
                .try_into()
                .expect("should fit into u32"),
        )
        .mul(
            deployment
                .common
                .num_blend_layers
                .get()
                .try_into()
                .expect("should fit into u32"),
        )
}
