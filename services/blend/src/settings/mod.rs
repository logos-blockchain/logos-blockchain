use core::num::NonZeroU64;

mod derived;
pub use self::derived::RuntimeComponents;
mod orchestrator;
pub use self::orchestrator::{EraOrchestratorSettings, OrchestratorSettings};
mod timing;
pub use self::timing::EraTimingSettings;

/// `T_M`: the message traversal time, which is what a sender waits for its
/// payload to appear on the broadcasting channel before treating the message
/// carrying it as lost.
///
/// A message crosses `ß` blend nodes, each of which holds it for at most the
/// maximal blending delay `∆max`, and the network carries it for the absorption
/// `η` of one hop:
///
/// `T_M = ß · (∆max + η)`
#[must_use]
pub const fn max_data_message_delay_in_rounds(
    num_blend_layers: NonZeroU64,
    max_blend_delay_in_rounds: NonZeroU64,
    network_absorption_in_rounds: NonZeroU64,
) -> NonZeroU64 {
    match NonZeroU64::new(
        num_blend_layers.get().saturating_mul(
            max_blend_delay_in_rounds
                .get()
                .saturating_add(network_absorption_in_rounds.get()),
        ),
    ) {
        Some(delay) => delay,
        // Not `expect`, to keep this a `const fn`.
        None => panic!("Both factors of the delivery deadline are non-zero."),
    }
}
