use std::num::NonZeroU64;

use lb_blend_proofs::quota::Quota;
use lb_utils::math::PositiveF64;

/// `Q_C`: the messaging allowance of a single core node for one epoch.
///
/// The spec requires the parameters to satisfy `C * ß_c > 0`, which the
/// argument types carry: `message_frequency_per_round` is positive and
/// `num_blend_layers` is non-zero, so `Q_C >= 1` for every epoch.
#[must_use]
pub fn core_quota(
    rounds_per_epoch: NonZeroU64,
    message_frequency_per_round: PositiveF64,
    num_blend_layers: NonZeroU64,
    membership_size: usize,
) -> Quota {
    // `C`: Expected number of cover messages that are generated during an epoch by
    // the core nodes.
    let expected_number_of_epoch_messages =
        rounds_per_epoch.get() as f64 * message_frequency_per_round.get();

    // `Q_c`: Messaging allowance that can be used by a core node during a single
    // epoch. We assume `R_c` to be `0` for now, hence `Q_c = ceil(C * (ß_c
    // + 0 * ß_c)) / N = ceil(C * ß_c) / N`.
    let quota_integer = NonZeroU64::try_from(
        ((expected_number_of_epoch_messages * num_blend_layers.get() as f64)
            / membership_size as f64)
            .ceil() as u64,
    )
    .expect("Core Quota cannot be zero, if `message_frequency_per_round` is greater than zero and `num_blend_layers` is non-zero.");
    quota_integer
        .get()
        .try_into()
        .expect("Core Quota must fit within the width the `PoQ` circuit allows.")
}
