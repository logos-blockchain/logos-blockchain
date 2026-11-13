use std::num::NonZeroU64;

use lb_blend_proofs::quota::Quota;
use lb_groth16::CircuitIntegerOutOfRange;
use lb_utils::math::PositiveF64;

/// `Q_C`: the messaging allowance of a single core node for one epoch, or why
/// it does not fit the width the `PoQ` circuit allows.
///
/// The spec requires the parameters to satisfy `C * ß_c > 0`, which the
/// argument types carry: `message_frequency_per_round` is positive and
/// `num_blend_layers` is non-zero, so `Q_C >= 1` for every epoch.
pub fn core_quota(
    rounds_per_epoch: NonZeroU64,
    message_frequency_per_round: PositiveF64,
    num_blend_layers: NonZeroU64,
    cover_replication_factor: u64,
    membership_size: usize,
) -> Result<Quota, CircuitIntegerOutOfRange> {
    // `C`: Expected number of cover messages that are generated during an epoch
    // by the core nodes.
    let expected_number_of_epoch_messages =
        rounds_per_epoch.get() as f64 * message_frequency_per_round.get();

    // `ß_c + R_c * ß_c`: the blending operations of a cover message, its
    // replications included.
    let blending_operations_per_message =
        num_blend_layers.get() as f64 * (1.0 + cover_replication_factor as f64);

    // `Q_c`: Messaging allowance that can be used by a core node during a
    // single epoch, `Q_c = ceil(C * (ß_c + R_c * ß_c) / N)`.
    let quota_integer = NonZeroU64::try_from(
        ((expected_number_of_epoch_messages * blending_operations_per_message)
            / membership_size as f64)
            .ceil() as u64,
    )
    .expect("Core Quota cannot be zero, if `message_frequency_per_round` is greater than zero and `num_blend_layers` is non-zero.");
    quota_integer.get().try_into()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use lb_blend_proofs::quota::Quota;
    use lb_utils::math::PositiveF64;

    use super::core_quota;

    /// The quota of `rounds_per_epoch` rounds of one cover message each, over
    /// 3 blending operations, shared by 10 nodes.
    fn quota_of(rounds_per_epoch: u64, cover_replication_factor: u64) -> Option<u64> {
        core_quota(
            NonZeroU64::new(rounds_per_epoch).unwrap(),
            PositiveF64::try_from(1.0).unwrap(),
            NonZeroU64::new(3).unwrap(),
            cover_replication_factor,
            10,
        )
        .ok()
        .map(Quota::get)
    }

    #[test]
    fn each_replication_adds_the_quota_of_the_cover_messages_again() {
        assert_eq!(quota_of(100, 0), Some(30));
        assert_eq!(quota_of(100, 1), Some(60));
        assert_eq!(quota_of(100, 2), Some(90));
    }

    #[test]
    fn a_quota_past_the_width_of_the_poq_circuit_is_refused() {
        // The most rounds whose quota fits: 3 of it for every 10 rounds.
        let rounds_per_epoch = Quota::MAX / 3 * 10;

        assert_eq!(quota_of(rounds_per_epoch, 0), Some(Quota::MAX));
        assert_eq!(quota_of(rounds_per_epoch, 1), None);
    }
}
