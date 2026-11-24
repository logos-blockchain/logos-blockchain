use std::num::{NonZeroU64, NonZeroU128};

use lb_blend_proofs::quota::Quota;
use lb_groth16::CircuitIntegerOutOfRange;
use lb_utils::math::{NonNegativeRatio, PositiveRatio};

/// `Q_C`: the messaging allowance of a single core node for one epoch, or why
/// it does not fit the width the `PoQ` circuit allows.
///
/// The spec requires the parameters to satisfy `C * ß_c > 0`, which the
/// argument types carry: `message_frequency_per_round` is positive and
/// `num_blend_layers` is non-zero, so `Q_C >= 1` for every epoch.
pub fn core_quota(
    rounds_per_epoch: NonZeroU64,
    message_frequency_per_round: PositiveRatio,
    num_blend_layers: NonZeroU64,
    cover_replication_factor: u64,
    membership_size: usize,
) -> Result<Quota, CircuitIntegerOutOfRange> {
    let NonNegativeRatio {
        numerator,
        denominator,
    } = message_frequency_per_round.get();

    // `C * (ß_c + R_c * ß_c)`: the blending operations of the cover messages
    // the core nodes are expected to generate during an epoch, `C` being the
    // rounds of the epoch times `F_c`, replications included. Kept over the
    // denominator of `F_c`, so that `C` is exact: the ceiling below would add
    // one to a product a rounding pushed past the whole number it equals.
    let blending_operations = NonZeroU128::from(rounds_per_epoch)
        .get()
        .checked_mul(u128::from(numerator))
        .and_then(|operations| operations.checked_mul(NonZeroU128::from(num_blend_layers).get()))
        .and_then(|operations| operations.checked_mul(u128::from(cover_replication_factor) + 1));
    let shares = NonZeroU128::new(u128::from(denominator.get()) * membership_size as u128);

    // `Q_c = ceil(C * (ß_c + R_c * ß_c) / N)`. Operations past a `u128`, or no
    // node to share them, leave a quota no circuit holds.
    let quota = blending_operations
        .zip(shares)
        .and_then(|(operations, shares)| u64::try_from(operations.div_ceil(shares.get())).ok())
        .unwrap_or(u64::MAX);
    quota.try_into()
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use lb_blend_proofs::quota::Quota;
    use lb_utils::math::PositiveRatio;

    use super::core_quota;

    /// The quota of `rounds_per_epoch` rounds of one cover message each, over
    /// 3 blending operations, shared by 10 nodes.
    fn quota_of(rounds_per_epoch: u64, cover_replication_factor: u64) -> Option<u64> {
        core_quota(
            NonZeroU64::new(rounds_per_epoch).unwrap(),
            PositiveRatio::new(1, NonZeroU32::MIN).unwrap(),
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

    /// 100 rounds at 7 cover messages per 100 rounds are 7 messages, which a
    /// double makes 7.000000000000001, and its ceiling 8.
    #[test]
    fn the_cover_messages_of_an_epoch_are_counted_exactly() {
        let quota = core_quota(
            NonZeroU64::new(100).unwrap(),
            PositiveRatio::new(7, NonZeroU32::new(100).unwrap()).unwrap(),
            NonZeroU64::MIN,
            0,
            1,
        )
        .ok()
        .map(Quota::get);

        assert_eq!(quota, Some(7));
    }
}
