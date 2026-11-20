use core::num::{NonZeroI128, NonZeroU16, NonZeroU128};
use std::collections::VecDeque;

use lb_core::mantle::Value;
use lb_log_targets::ledger;
use lb_utils::{
    bounded::{BoundedError, ResizableRuntimeBoundedVecDeque, RuntimeBounded},
    math::{NonNegativeRatio, ProperFraction},
};

use crate::config::{BlockRewardsConfig, EmissionRatio};

/// `fraction` of the supply cap `max_supply`, rounded down, if it fits in a
/// [`Value`].
#[must_use]
pub const fn fraction_of_supply(fraction: ProperFraction, max_supply: Value) -> Option<Value> {
    let NonNegativeRatio {
        numerator,
        denominator,
    } = fraction.get();
    let share = max_supply as u128 * numerator as u128 / denominator.get() as u128;
    if share > Value::MAX as u128 {
        None
    } else {
        Some(share as Value)
    }
}

const LOG_TARGET: &str = ledger::ROOT;

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

/// The pooled fees of the last blocks, oldest first: at most the `T` of the
/// era of the last block, the blocks it lacks counting as pooling nothing.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "PooledFeesWindowFields", into = "PooledFeesWindowFields")]
pub struct PooledFeesWindow(ResizableRuntimeBoundedVecDeque<Value, u16, 0>);

/// A [`PooledFeesWindow`] as it is stored: its `T` and its pooled fees.
#[derive(serde::Serialize, serde::Deserialize)]
struct PooledFeesWindowFields {
    blocks: NonZeroU16,
    fees: VecDeque<Value>,
}

impl PooledFeesWindow {
    /// The window of `blocks` before any block pooled fees.
    #[must_use]
    pub const fn empty(blocks: NonZeroU16) -> Self {
        Self(ResizableRuntimeBoundedVecDeque::empty(blocks.get()))
    }

    /// The window once a block of an era of `blocks` pooled `pooled_fees`: the
    /// block enters last, and the oldest leaves a full window. An era that
    /// shrinks the window drops its oldest blocks. An era that grows it starts
    /// it over empty, as at genesis, so that no older fee is paid again at the
    /// new size. What they did not pay out stays in the pool.
    #[must_use]
    pub fn with_block(self, pooled_fees: Value, blocks: NonZeroU16) -> Self {
        // Reset the window if the new era grows its size.
        let mut window = if blocks.get() > self.0.max_len() {
            tracing::info!(
                target: LOG_TARGET,
                old_window = self.0.max_len(),
                new_window = blocks.get(),
                dropped_fees = self.iter().sum::<lb_core::mantle::Value>(),
                "pooled fees window grown by an era starts over"
            );
            Self::empty(blocks)
        } else {
            self
        };
        window
            .0
            .try_set_max_len(blocks.get())
            .expect("a window holds any number of blocks down to none");
        window.0.push_back_evicting(pooled_fees);
        window
    }

    /// The pooled fees of the last block.
    #[must_use]
    pub fn last(&self) -> Value {
        self.0.as_inner().back().copied().unwrap_or_default()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.0.as_inner().iter()
    }
}

impl TryFrom<PooledFeesWindowFields> for PooledFeesWindow {
    type Error = BoundedError;

    fn try_from(
        PooledFeesWindowFields { blocks, fees }: PooledFeesWindowFields,
    ) -> Result<Self, Self::Error> {
        ResizableRuntimeBoundedVecDeque::try_new(fees, blocks.get()).map(Self)
    }
}

impl From<PooledFeesWindow> for PooledFeesWindowFields {
    fn from(PooledFeesWindow(window): PooledFeesWindow) -> Self {
        Self {
            blocks: NonZeroU16::new(window.max_len()).expect("a window spans at least one block"),
            fees: RuntimeBounded::from(window).into_inner(),
        }
    }
}

/// The reserve, the pool, the closed and pending Blend shares and the window of
/// Block Rewards.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BlockRewards {
    rewards_reserve: Value,
    rewards_pool: Value,
    /// The Blend share of the last closed epoch.
    blend_rewards_closed: Value,
    /// The Blend share of the current epoch.
    blend_rewards_pending: Value,
    pooled_fees_window: PooledFeesWindow,
}

impl BlockRewards {
    /// The block rewards at genesis, whose reserve holds `rewards_reserve`,
    /// over the window of the genesis era.
    #[must_use]
    pub const fn genesis(rewards_reserve: Value, pooled_fees_window: NonZeroU16) -> Self {
        Self {
            rewards_reserve,
            rewards_pool: 0,
            blend_rewards_closed: 0,
            blend_rewards_pending: 0,
            pooled_fees_window: PooledFeesWindow::empty(pooled_fees_window),
        }
    }

    pub fn distribute_block_reward(
        self,
        pooled_fees: Value,
        total_stake: Value,
        config: &BlockRewardsConfig,
    ) -> Result<(Self, BlockRewardShares), BlockRewardsError> {
        let pooled_fees_window = self
            .pooled_fees_window
            .with_block(pooled_fees, config.pooled_fees_window());
        let (release, payout) = block_reward(
            total_stake,
            &pooled_fees_window,
            self.rewards_reserve,
            config,
        )?;
        let reward = release
            .checked_add(payout)
            .ok_or(BlockRewardsError::OutOfRange)?;
        let leader = leader_share(reward, config);
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
                blend_rewards_closed: self.blend_rewards_closed,
                blend_rewards_pending,
                pooled_fees_window,
            },
            BlockRewardShares { leader, blend },
        ))
    }

    /// The Blend settlement of the first block of an epoch, Deterministic
    /// Implementation: the reward notes are paid from `blend_rewards_closed`,
    /// what they do not use returns to `rewards_reserve`, then
    /// `blend_rewards_pending` moves into `blend_rewards_closed`.
    pub fn settle_blend_rewards(self, paid: Value) -> Result<Self, BlockRewardsError> {
        let rewards_reserve = self
            .blend_rewards_closed
            .checked_sub(paid)
            .and_then(|unused| self.rewards_reserve.checked_add(unused))
            .ok_or(BlockRewardsError::OutOfRange)?;
        Ok(Self {
            rewards_reserve,
            blend_rewards_closed: self.blend_rewards_pending,
            blend_rewards_pending: 0,
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
    pub const fn blend_rewards_closed(&self) -> Value {
        self.blend_rewards_closed
    }

    #[must_use]
    pub const fn blend_rewards_pending(&self) -> Value {
        self.blend_rewards_pending
    }

    #[must_use]
    pub const fn pooled_fees_window(&self) -> &PooledFeesWindow {
        &self.pooled_fees_window
    }
}

/// The reserve release and the pool payout of a block under the era's
/// `config`, their sum is its block reward.
pub fn block_reward(
    total_stake: Value,
    pooled_fees_window: &PooledFeesWindow,
    reserve: Value,
    config: &BlockRewardsConfig,
) -> Result<(Value, Value), BlockRewardsError> {
    let a_scale = NonZeroI128::from(config.emission_factor_scale()).get();
    let EmissionRatio {
        numerator: release_numerator,
        denominator: release_denominator,
    } = config.max_emission_per_block();
    // The pooled fees of a block never exceed the supply cap, so every product
    // stays within the range `BlockRewardsConfig::new` checks.
    let sum_fees = pooled_fees_window
        .iter()
        .try_fold(0i128, |sum, &fee| sum.checked_add(i128::from(fee)))
        .ok_or(BlockRewardsError::OutOfRange)?;
    let a = i128::from(config.fee_average_numerator())
        .checked_mul(sum_fees)
        .and_then(|fees| {
            i128::from(config.stake_target())
                .checked_sub(i128::from(total_stake))?
                .checked_add(i128::from(config.min_emission_offset()))?
                .checked_add(fees)
        })
        .ok_or(BlockRewardsError::OutOfRange)?
        .clamp(0, a_scale);
    // The release cannot exceed what the reserve still holds.
    let release = i128::from(release_numerator)
        .checked_mul(a)
        .and_then(|numerator| {
            numerator.checked_div(
                NonZeroI128::from(release_denominator)
                    .get()
                    .checked_mul(a_scale)?,
            )
        })
        .ok_or(BlockRewardsError::OutOfRange)?
        .min(i128::from(reserve));
    let payout = a_scale
        .checked_sub(a)
        .and_then(|weight| weight.checked_mul(sum_fees))
        .and_then(|numerator| {
            numerator.checked_div(
                NonZeroI128::from(config.pooled_fees_window())
                    .get()
                    .checked_mul(a_scale)?,
            )
        })
        .ok_or(BlockRewardsError::OutOfRange)?;
    Ok((
        Value::try_from(release).map_err(|_| BlockRewardsError::OutOfRange)?,
        Value::try_from(payout).map_err(|_| BlockRewardsError::OutOfRange)?,
    ))
}

/// The leader's share of `block_reward`, rounded down: what the era's Blend
/// share leaves.
#[must_use]
pub fn leader_share(block_reward: Value, config: &BlockRewardsConfig) -> Value {
    let NonNegativeRatio {
        numerator: blend_numerator,
        denominator,
    } = config.blend_share().get();
    let denominator = NonZeroU128::from(denominator).get();
    Value::try_from(
        u128::from(block_reward) * (denominator - u128::from(blend_numerator)) / denominator,
    )
    .expect("a share of a Value fits in a Value")
}

#[cfg(test)]
mod tests {
    use std::{iter, num::NonZero};

    use super::*;
    use crate::config::BlockRewardsParameters;

    /// `S_cap` of the block rewards specification.
    const MAX_SUPPLY: Value = 10_000_000_000_000_000_000;

    /// `D_0,target` of the specification, three tenths of its cap.
    const STAKE_AT_TARGET: Value = 3_000_000_000_000_000_000;

    /// `B_0` of the specification, `I_max · S_cap · Y` with `Y = 10` years.
    const REWARDS_RESERVE_GENESIS: Value = 1_000_000_000_000_000_000;

    /// The slots of the specification, one second long.
    const SLOT_DURATION: NonZero<u64> = NonZero::new(1).unwrap();

    /// The slot activation coefficient of a block every 30 seconds, the time
    /// step of the specification.
    const SLOT_ACTIVATION_COEFF: ProperFraction =
        ProperFraction::new(1, NonZero::new(30).unwrap()).unwrap();

    /// The parameters of the block rewards specification.
    const PARAMETERS: BlockRewardsParameters = BlockRewardsParameters {
        max_emission_rate: ProperFraction::new(1, NonZero::new(100).unwrap()).unwrap(),
        min_emission_rate: NonNegativeRatio::new(0, NonZero::new(1).unwrap()),
        stake_target: ProperFraction::new(3, NonZero::new(10).unwrap()).unwrap(),
        pooled_fees_target: NonNegativeRatio::new(1, NonZero::new(1).unwrap()),
        stake_deviation_weight: NonNegativeRatio::new(1, NonZero::new(4).unwrap()),
        pooled_fees_weight: NonNegativeRatio::new(1, NonZero::new(1).unwrap()),
        pooled_fees_window: NonZero::new(120).unwrap(),
        blend_share: ProperFraction::new(6, NonZero::new(10).unwrap()).unwrap(),
    };

    /// The block rewards of the specification, on its cap and time step.
    const CONFIG: BlockRewardsConfig =
        BlockRewardsConfig::new(PARAMETERS, MAX_SUPPLY, SLOT_DURATION, SLOT_ACTIVATION_COEFF)
            .unwrap();

    // The integer computation of the specification, for a block every 30
    // seconds.
    #[test]
    fn the_specification_parameters_derive_its_integer_values() {
        assert_eq!(CONFIG.stake_target(), STAKE_AT_TARGET);
        assert_eq!(
            CONFIG.emission_factor_scale().get(),
            120_000_000_000_000_000
        );
        assert_eq!(CONFIG.min_emission_offset(), 0);
        assert_eq!(CONFIG.fee_average_numerator(), 10_512);
        assert_eq!(
            CONFIG.max_emission_per_block(),
            EmissionRatio {
                numerator: 62_500_000_000_000,
                denominator: NonZero::new(657).unwrap(),
            }
        );
    }

    // A block every 20 seconds releases two thirds as much, and weighs its
    // pooled fees one and a half times as much, as a block every 30
    // seconds.
    #[test]
    fn the_values_of_a_block_follow_the_block_time() {
        let config = BlockRewardsConfig::new(
            PARAMETERS,
            MAX_SUPPLY,
            SLOT_DURATION,
            ProperFraction::new(1, NonZero::new(20).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(config.fee_average_numerator(), 15_768);
        assert_eq!(
            config.max_emission_per_block(),
            EmissionRatio {
                numerator: 125_000_000_000_000,
                denominator: NonZero::new(1_971).unwrap(),
            }
        );
    }

    // `I_min` adds `D_0,target · I_min / α_d` to the release's weight,
    // `A_SCALE` when it is `I_max`.
    #[test]
    fn the_minimum_emission_rate_offsets_the_weight() {
        let config = BlockRewardsConfig::new(
            BlockRewardsParameters {
                min_emission_rate: NonNegativeRatio::new(1, NonZero::new(100).unwrap()),
                ..PARAMETERS
            },
            MAX_SUPPLY,
            SLOT_DURATION,
            SLOT_ACTIVATION_COEFF,
        )
        .unwrap();
        assert_eq!(
            config.min_emission_offset(),
            config.emission_factor_scale().get()
        );
        // The stake at its target, no fees: a release in full, where the
        // specification's `I_min` of zero releases nothing.
        let at_target = |config| {
            block_reward(
                STAKE_AT_TARGET,
                &PooledFeesWindow::empty(CONFIG.pooled_fees_window()),
                REWARDS_RESERVE_GENESIS,
                config,
            )
        };
        assert_eq!(at_target(&config), Ok((95_129_375_951, 0)));
        assert_eq!(at_target(&CONFIG), Ok((0, 0)));
    }

    #[test]
    fn the_specification_parameters_reward_the_blocks_of_its_cap_only() {
        let config_at = |max_supply| {
            BlockRewardsConfig::new(PARAMETERS, max_supply, SLOT_DURATION, SLOT_ACTIVATION_COEFF)
        };
        // A tenth above the cap, the payout's numerator overflows an i128.
        assert_eq!(config_at(11_000_000_000_000_000_000), None);
        // Below 84 units, the scale of the release's weight rounds down to
        // zero.
        assert_eq!(config_at(83), None);
        assert!(config_at(84).is_some());
        // A weight of the stake's shortfall of zero leaves no scale.
        assert_eq!(
            BlockRewardsConfig::new(
                BlockRewardsParameters {
                    stake_deviation_weight: NonNegativeRatio::new(0, NonZero::new(1).unwrap()),
                    ..PARAMETERS
                },
                MAX_SUPPLY,
                SLOT_DURATION,
                SLOT_ACTIVATION_COEFF,
            ),
            None
        );
    }

    fn window_of(fees: impl IntoIterator<Item = Value>, blocks: u16) -> PooledFeesWindow {
        let blocks = NonZero::new(blocks).unwrap();
        fees.into_iter()
            .fold(PooledFeesWindow::empty(blocks), |window, fee| {
                window.with_block(fee, blocks)
            })
    }

    fn window_with_last(fee: Value) -> PooledFeesWindow {
        window_of([fee], CONFIG.pooled_fees_window().get())
    }

    #[test]
    fn a_block_enters_the_window_last_and_pushes_out_the_oldest() {
        let window = window_of([7, 8].into_iter().chain(iter::repeat_n(0, 118)), 120)
            .with_block(9, CONFIG.pooled_fees_window());
        assert_eq!(window.last(), 9);
        assert_eq!(window.iter().next(), Some(&8));
        assert_eq!(window.iter().sum::<Value>(), 17);
    }

    #[test]
    fn an_era_shrinking_the_window_drops_its_oldest_blocks() {
        let window = window_of([1, 2, 3], 3);
        // Shrunk to 2, the window keeps its last block and the new one.
        let window = window.with_block(4, NonZero::new(2).unwrap());
        assert_eq!(window.iter().copied().collect::<Vec<_>>(), [3, 4]);
    }

    #[test]
    fn an_era_growing_the_window_resets_it() {
        let window = window_of([1, 2], 2);
        // Grown to 3, the window holds the new block only.
        let window = window.with_block(3, NonZero::new(3).unwrap());
        assert_eq!(window.iter().copied().collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn a_window_lacking_blocks_averages_them_as_pooling_nothing() {
        // One block pooled 120 units under a window of 120: the average is 1,
        // as if 119 blocks had pooled nothing. Stake above target: no
        // release.
        assert_eq!(
            block_reward(
                4_000_000_000_000_000_000,
                &window_with_last(120),
                REWARDS_RESERVE_GENESIS,
                &CONFIG
            ),
            Ok((0, 1))
        );
    }

    #[test]
    fn block_reward_matches_the_integer_reference() {
        // No stake, no fees: A_t = 1, the maximum release 62_500 * 10^9 / 657
        // rounded down.
        assert_eq!(
            block_reward(
                0,
                &PooledFeesWindow::empty(CONFIG.pooled_fees_window()),
                REWARDS_RESERVE_GENESIS,
                &CONFIG
            ),
            Ok((95_129_375_951, 0))
        );
        // Stake at target, one block pooled 10^12: A_t' = 10_512 * 10^12.
        assert_eq!(
            block_reward(
                STAKE_AT_TARGET,
                &window_with_last(1_000_000_000_000),
                REWARDS_RESERVE_GENESIS,
                &CONFIG
            ),
            Ok((8_333_333_333, 7_603_333_333))
        );
        // Stake above target clamps A_t' to 0: no release, the average pooled
        // fee is paid.
        assert_eq!(
            block_reward(
                4_000_000_000_000_000_000,
                &window_of(iter::repeat_n(10_000_000_000, 120), 120),
                REWARDS_RESERVE_GENESIS,
                &CONFIG
            ),
            Ok((0, 10_000_000_000))
        );
        // The release is capped at the reserve.
        assert_eq!(
            block_reward(
                0,
                &PooledFeesWindow::empty(CONFIG.pooled_fees_window()),
                5,
                &CONFIG
            ),
            Ok((5, 0))
        );
    }

    #[test]
    fn leader_share_rounds_down_and_blend_takes_the_remainder() {
        assert_eq!(leader_share(7, &CONFIG), 2);
        assert_eq!(leader_share(95_129_375_951, &CONFIG), 38_051_750_380);
        assert_eq!(leader_share(Value::MAX, &CONFIG), 7_378_697_629_483_820_646);
        let (_, shares) =
            BlockRewards::genesis(REWARDS_RESERVE_GENESIS, CONFIG.pooled_fees_window())
                .distribute_block_reward(0, 0, &CONFIG)
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
        let mut state = BlockRewards::genesis(REWARDS_RESERVE_GENESIS, CONFIG.pooled_fees_window());
        for block in 0..CONFIG.pooled_fees_window().get() {
            let pooled_fees = if block == 0 { 1_000_000_000_000 } else { 0 };
            let shares;
            (state, shares) = state
                .distribute_block_reward(pooled_fees, STAKE_AT_TARGET, &CONFIG)
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

        // The fee has left the window: with the stake at target and no fees,
        // nothing is released or paid.
        let (state, shares) = state
            .distribute_block_reward(0, STAKE_AT_TARGET, &CONFIG)
            .unwrap();
        assert_eq!(
            shares,
            BlockRewardShares {
                leader: 0,
                blend: 0
            }
        );
        assert_eq!(state.rewards_pool(), 87_600_000_040);
        assert_eq!(
            state.pooled_fees_window(),
            &window_of(iter::repeat_n(0, 120), 120)
        );
    }

    #[test]
    fn block_reward_moves_tokens_between_stocks_only() {
        let (state, shares) =
            BlockRewards::genesis(REWARDS_RESERVE_GENESIS, CONFIG.pooled_fees_window())
                .distribute_block_reward(1_000_000_000_000, STAKE_AT_TARGET, &CONFIG)
                .unwrap();
        assert_eq!(state.rewards_reserve(), 999_999_991_666_666_667);
        assert_eq!(state.rewards_pool(), 992_396_666_667);
        assert_eq!(
            state.pooled_fees_window(),
            &window_with_last(1_000_000_000_000)
        );
        // reserve + pool + closed + pending + what the leader took = B_0 + the
        // fees routed to the pool
        assert_eq!(
            u128::from(state.rewards_reserve())
                + u128::from(state.rewards_pool())
                + u128::from(state.blend_rewards_closed())
                + u128::from(state.blend_rewards_pending())
                + u128::from(shares.leader),
            u128::from(REWARDS_RESERVE_GENESIS) + 1_000_000_000_000
        );
    }

    #[test]
    fn settling_an_epoch_closes_the_pending_share_and_pays_nothing_from_it() {
        let (state, _) =
            BlockRewards::genesis(REWARDS_RESERVE_GENESIS, CONFIG.pooled_fees_window())
                .distribute_block_reward(1_000_000_000_000, STAKE_AT_TARGET, &CONFIG)
                .unwrap();
        assert_eq!(state.blend_rewards_closed(), 0);

        let closed = state.clone().settle_blend_rewards(0).unwrap();
        assert_eq!(closed.blend_rewards_pending(), 0);
        assert_eq!(closed.blend_rewards_closed(), 9_562_000_000);
        assert_eq!(closed.rewards_reserve(), state.rewards_reserve());
        assert_eq!(closed.rewards_pool(), state.rewards_pool());
    }

    #[test]
    fn shrinking_the_window_never_pays_out_more_than_the_pooled_fees() {
        // Block 1 pools 100 under T = 4, then an era lowers T to 2 at block 3.
        let paid = pay_out([(100, 4), (0, 4), (0, 2), (0, 2)])
            .unwrap_or_else(|(block, err)| panic!("block {block} is invalid: {err}"));
        assert!(
            paid.iter().sum::<Value>() <= 100,
            "paid {paid:?} out of 100"
        );
    }

    #[test]
    fn growing_the_window_never_pays_out_more_than_the_pooled_fees() {
        // Block 1 pools 100 under T = 2, then an era raises T to 4. The window
        // starts over, and the 50 block 1 did not pay out stays in the pool.
        let paid = pay_out([(100, 2), (0, 4), (0, 4), (0, 4)])
            .unwrap_or_else(|(block, err)| panic!("block {block} is invalid: {err}"));
        assert_eq!(paid, [50, 0, 0, 0]);
    }

    /// The payout of each block pooling `fees` under a window of `blocks`, or
    /// the first block whose reward fails. Stake is above target, so no reserve
    /// is released and a block's reward is its payout from the pool.
    fn pay_out<const N: usize>(
        blocks: [(Value, u16); N],
    ) -> Result<[Value; N], (usize, BlockRewardsError)> {
        let mut rewards =
            BlockRewards::genesis(REWARDS_RESERVE_GENESIS, NonZero::new(blocks[0].1).unwrap());
        let mut paid = [0; N];
        for (block, (payout, (fees, window))) in (1..).zip(paid.iter_mut().zip(blocks)) {
            let (next, shares) = rewards
                .distribute_block_reward(
                    fees,
                    4_000_000_000_000_000_000,
                    &config_with_window(window),
                )
                .map_err(|err| (block, err))?;
            *payout = shares.leader + shares.blend;
            rewards = next;
        }
        Ok(paid)
    }

    fn config_with_window(blocks: u16) -> BlockRewardsConfig {
        BlockRewardsConfig::new(
            BlockRewardsParameters {
                pooled_fees_window: NonZero::new(blocks).unwrap(),
                ..PARAMETERS
            },
            MAX_SUPPLY,
            SLOT_DURATION,
            SLOT_ACTIVATION_COEFF,
        )
        .unwrap()
    }
}
