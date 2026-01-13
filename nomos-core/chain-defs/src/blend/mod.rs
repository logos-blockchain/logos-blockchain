use std::num::NonZeroU64;

use nomos_utils::math::NonNegativeF64;

#[must_use]
pub fn core_quota(
    rounds_per_session: NonZeroU64,
    message_frequency_per_round: NonNegativeF64,
    num_blend_layers: NonZeroU64,
    membership_size: usize,
) -> u64 {
    // `C`: Expected number of cover messages that are generated during a session by
    // the core nodes.
    let expected_number_of_session_messages =
        rounds_per_session.get() as f64 * message_frequency_per_round.get();

    // `Q_c`: Messaging allowance that can be used by a core node during a single
    // session. We assume `R_c` to be `0` for now, hence `Q_c = ceil(C * (ß_c
    // + 0 * ß_c)) / N = ceil(C * ß_c) / N`.
    ((expected_number_of_session_messages * num_blend_layers.get() as f64) / membership_size as f64)
        .ceil() as u64
}
