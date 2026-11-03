use lb_core::mantle::Value;

/// T, the blocks of the moving average.
pub const POOLED_FEES_WINDOW: usize = 120;
/// `B_0`, the rewards reserve at genesis.
pub const REWARDS_RESERVE_GENESIS: Value = 1_000_000_000_000_000_000;
/// Denominator of `A_t`.
const A_SCALE: i128 = 120_000_000_000_000_000;
/// `D_0_target`.
const STAKE_TARGET: i128 = 3_000_000_000_000_000_000;
/// `alpha_a / (I_max * D_1_target * Delta_t * T)`, over `A_SCALE`.
const FEE_AVG_NUMERATOR: i128 = 10_512;
/// Numerator of `I_max * S_cap * Delta_t`.
const RELEASE_NUMERATOR: i128 = 62_500_000_000_000;
/// Denominator of `I_max * S_cap * Delta_t`.
const RELEASE_DENOMINATOR: i128 = 657;

#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum BlockRewardsError {
    #[error("block reward arithmetic is out of the range of a Value")]
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRewardShares {
    pub leader: Value,
    pub blend: Value,
}

/// The reserve, the pool, the pending Blend share and the window of Block
/// Rewards, in units of `10^-9` LGO.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BlockRewards {
    rewards_reserve: Value,
    rewards_pool: Value,
    blend_rewards_pending: Value,
    #[serde(with = "serde_arrays")]
    pooled_fees_window: [Value; POOLED_FEES_WINDOW],
}

impl BlockRewards {
    #[must_use]
    pub const fn genesis() -> Self {
        Self {
            rewards_reserve: REWARDS_RESERVE_GENESIS,
            rewards_pool: 0,
            blend_rewards_pending: 0,
            pooled_fees_window: [0; POOLED_FEES_WINDOW],
        }
    }

    pub fn distribute_block_reward(
        self,
        pooled_fees: Value,
        total_stake: Value,
    ) -> Result<(Self, BlockRewardShares), BlockRewardsError> {
        // The pooled fees of the block enter the window, this block last.
        let mut pooled_fees_window = self.pooled_fees_window;
        pooled_fees_window.rotate_left(1);
        pooled_fees_window[POOLED_FEES_WINDOW - 1] = pooled_fees;
        let (release, payout) =
            block_reward(total_stake, &pooled_fees_window, self.rewards_reserve)?;
        let reward = release
            .checked_add(payout)
            .ok_or(BlockRewardsError::OutOfRange)?;
        let leader = leader_share(reward);
        let blend = reward
            .checked_sub(leader)
            .ok_or(BlockRewardsError::OutOfRange)?;
        let rewards_reserve = self
            .rewards_reserve
            .checked_sub(release)
            .ok_or(BlockRewardsError::OutOfRange)?;
        let rewards_pool = self
            .rewards_pool
            .checked_add(pooled_fees)
            .and_then(|pool| pool.checked_sub(payout))
            .ok_or(BlockRewardsError::OutOfRange)?;
        let blend_rewards_pending = self
            .blend_rewards_pending
            .checked_add(blend)
            .ok_or(BlockRewardsError::OutOfRange)?;
        Ok((
            Self {
                rewards_reserve,
                rewards_pool,
                blend_rewards_pending,
                pooled_fees_window,
            },
            BlockRewardShares { leader, blend },
        ))
    }

    pub fn settle_blend_rewards(
        self,
        settled: Value,
        paid: Value,
    ) -> Result<Self, BlockRewardsError> {
        // The Blend reward of the settled epochs leaves the pending share, what the
        // notes do not use returns to the pool.
        let blend_rewards_pending = self
            .blend_rewards_pending
            .checked_sub(settled)
            .ok_or(BlockRewardsError::OutOfRange)?;
        let rewards_pool = settled
            .checked_sub(paid)
            .and_then(|unused| self.rewards_pool.checked_add(unused))
            .ok_or(BlockRewardsError::OutOfRange)?;
        Ok(Self {
            rewards_pool,
            blend_rewards_pending,
            ..self
        })
    }

    #[must_use]
    pub const fn rewards_reserve(&self) -> Value {
        self.rewards_reserve
    }

    #[must_use]
    pub const fn rewards_pool(&self) -> Value {
        self.rewards_pool
    }

    #[must_use]
    pub const fn blend_rewards_pending(&self) -> Value {
        self.blend_rewards_pending
    }

    #[must_use]
    pub const fn pooled_fees_window(&self) -> &[Value; POOLED_FEES_WINDOW] {
        &self.pooled_fees_window
    }
}

/// The reserve release and the pool payout of a block, their sum is its block
/// reward.
pub fn block_reward(
    total_stake: Value,
    pooled_fees_window: &[Value; POOLED_FEES_WINDOW],
    reserve: Value,
) -> Result<(Value, Value), BlockRewardsError> {
    // The pooled fees of a block never exceed the supply cap, so every product
    // stays below 2^127.
    let sum_fees = pooled_fees_window
        .iter()
        .try_fold(0i128, |sum, &fee| sum.checked_add(i128::from(fee)))
        .ok_or(BlockRewardsError::OutOfRange)?;
    let a = FEE_AVG_NUMERATOR
        .checked_mul(sum_fees)
        .and_then(|fees| {
            STAKE_TARGET
                .checked_sub(i128::from(total_stake))?
                .checked_add(fees)
        })
        .ok_or(BlockRewardsError::OutOfRange)?
        .clamp(0, A_SCALE);
    // The release cannot exceed what the reserve still holds.
    let release = RELEASE_NUMERATOR
        .checked_mul(a)
        .and_then(|numerator| numerator.checked_div(RELEASE_DENOMINATOR * A_SCALE))
        .ok_or(BlockRewardsError::OutOfRange)?
        .min(i128::from(reserve));
    let payout = A_SCALE
        .checked_sub(a)
        .and_then(|weight| weight.checked_mul(sum_fees))
        .and_then(|numerator| numerator.checked_div(POOLED_FEES_WINDOW as i128 * A_SCALE))
        .ok_or(BlockRewardsError::OutOfRange)?;
    Ok((
        Value::try_from(release).map_err(|_| BlockRewardsError::OutOfRange)?,
        Value::try_from(payout).map_err(|_| BlockRewardsError::OutOfRange)?,
    ))
}

#[must_use]
pub fn leader_share(block_reward: Value) -> Value {
    Value::try_from(u128::from(block_reward) * 2 / 5).expect("two fifths of a Value fit in a Value")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAKE_AT_TARGET: Value = 3_000_000_000_000_000_000;

    fn window_with_last(fee: Value) -> [Value; POOLED_FEES_WINDOW] {
        let mut window = [0; POOLED_FEES_WINDOW];
        window[POOLED_FEES_WINDOW - 1] = fee;
        window
    }

    #[test]
    fn block_reward_matches_the_integer_reference() {
        // No stake, no fees: A_t = 1, the maximum release 62_500 * 10^9 / 657 rounded
        // down.
        assert_eq!(
            block_reward(0, &[0; POOLED_FEES_WINDOW], REWARDS_RESERVE_GENESIS),
            Ok((95_129_375_951, 0))
        );
        // Stake at target, one block pooled 10^12: A_t' = 10_512 * 10^12.
        assert_eq!(
            block_reward(
                STAKE_AT_TARGET,
                &window_with_last(1_000_000_000_000),
                REWARDS_RESERVE_GENESIS
            ),
            Ok((8_333_333_333, 7_603_333_333))
        );
        // Stake above target clamps A_t' to 0: no release, the average pooled fee is
        // paid.
        assert_eq!(
            block_reward(
                4_000_000_000_000_000_000,
                &[10_000_000_000; POOLED_FEES_WINDOW],
                REWARDS_RESERVE_GENESIS
            ),
            Ok((0, 10_000_000_000))
        );
        // The release is capped at the reserve.
        assert_eq!(block_reward(0, &[0; POOLED_FEES_WINDOW], 5), Ok((5, 0)));
    }

    #[test]
    fn leader_share_rounds_down_and_blend_takes_the_remainder() {
        assert_eq!(leader_share(7), 2);
        assert_eq!(leader_share(95_129_375_951), 38_051_750_380);
        assert_eq!(leader_share(Value::MAX), 7_378_697_629_483_820_646);
        let (_, shares) = BlockRewards::genesis()
            .distribute_block_reward(0, 0)
            .unwrap();
        assert_eq!(
            shares,
            BlockRewardShares {
                leader: 38_051_750_380,
                blend: 57_077_625_571
            }
        );
    }

    #[test]
    fn pooled_fee_is_paid_over_exactly_120_blocks() {
        let mut state = BlockRewards::genesis();
        for block in 0..POOLED_FEES_WINDOW {
            let pooled_fees = if block == 0 { 1_000_000_000_000 } else { 0 };
            let shares;
            (state, shares) = state
                .distribute_block_reward(pooled_fees, STAKE_AT_TARGET)
                .unwrap();
            assert_eq!(
                shares,
                BlockRewardShares {
                    leader: 6_374_666_666,
                    blend: 9_562_000_000
                }
            );
        }
        assert_eq!(state.rewards_reserve(), 999_999_000_000_000_040);
        assert_eq!(state.rewards_pool(), 87_600_000_040);
        assert_eq!(state.blend_rewards_pending(), 1_147_440_000_000);

        // The fee has left the window: with the stake at target and no fees, nothing is
        // released or paid.
        let (state, shares) = state.distribute_block_reward(0, STAKE_AT_TARGET).unwrap();
        assert_eq!(
            shares,
            BlockRewardShares {
                leader: 0,
                blend: 0
            }
        );
        assert_eq!(state.rewards_pool(), 87_600_000_040);
        assert_eq!(state.pooled_fees_window(), &[0; POOLED_FEES_WINDOW]);
    }

    #[test]
    fn block_reward_moves_tokens_between_stocks_only() {
        let (state, shares) = BlockRewards::genesis()
            .distribute_block_reward(1_000_000_000_000, STAKE_AT_TARGET)
            .unwrap();
        assert_eq!(state.rewards_reserve(), 999_999_991_666_666_667);
        assert_eq!(state.rewards_pool(), 992_396_666_667);
        assert_eq!(
            state.pooled_fees_window(),
            &window_with_last(1_000_000_000_000)
        );
        // reserve + pool + pending + what the leader took = B_0 + the fees routed to
        // the pool
        assert_eq!(
            u128::from(state.rewards_reserve())
                + u128::from(state.rewards_pool())
                + u128::from(state.blend_rewards_pending())
                + u128::from(shares.leader),
            u128::from(REWARDS_RESERVE_GENESIS) + 1_000_000_000_000
        );
    }

    #[test]
    fn settle_blend_rewards_returns_what_the_notes_do_not_use_to_the_pool() {
        let (state, _) = BlockRewards::genesis()
            .distribute_block_reward(1_000_000_000_000, STAKE_AT_TARGET)
            .unwrap();
        let settled = state
            .clone()
            .settle_blend_rewards(9_562_000_000, 9_000_000_000)
            .unwrap();
        assert_eq!(settled.blend_rewards_pending(), 0);
        assert_eq!(settled.rewards_pool(), 992_958_666_667);
        assert_eq!(settled.rewards_reserve(), state.rewards_reserve());
        assert_eq!(
            state.clone().settle_blend_rewards(9_562_000_001, 0),
            Err(BlockRewardsError::OutOfRange)
        );
        assert_eq!(
            state.settle_blend_rewards(1, 2),
            Err(BlockRewardsError::OutOfRange)
        );
    }
}
