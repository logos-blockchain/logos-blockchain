//! Proof for finding `20261104T030049-7be83a/2` (spec domain, LIP 199
//! "Block Rewards", logos-lips@fddc2c1).
//!
//! Spec, section "Pool Accounting and Supply Dynamics":
//!
//! ```text
//! iota_t = min{ A_t * I_max * S_cap * Delta_t / f , B_{t-1} }      (reserve release)
//! R_t    = (1 - A_t) * Rbar_t + iota_t                             (block reward)
//! B_t    = B_{t-1} - iota_t,  B_t >= 0,  B_0 = I_max * S_cap * Y = 10^9 LGO
//! ```
//!
//! "Once the reserve is depleted, iota_t = 0 and the reward reduces to the
//! recycled component (1 - A_t) * Rbar_t", and "Net circulating growth over
//! the reserve's life is bounded by B_0 + W_0".
//!
//! Scenario: the inferred total stake is 1, far below `D_{0,target}`, so the
//! emission factor is pinned at `A_t = 1`, and no transaction pays a fee, so
//! `Rbar_t = 0` and every block's reward is the reserve release alone. Summed
//! over the reserve's whole life, the rewards can therefore never exceed
//! `B_0 = 10^9`, and the leaders' 40 % share of them (each share floored) can
//! never exceed `0.4 * B_0 = 4 * 10^8`.
//!
//! The ledger keeps no reserve balance: every block pays the full per-block
//! release (95 = 57 Blend + 38 leader, `62500/657` floored) no matter how much
//! has already been released. After 10 530 000 such blocks (about ten years
//! of 30-second blocks) the leaders alone have been credited
//! 38 * 10 530 000 = 400 140 000 > 4 * 10^8, and the next block, released
//! from a reserve that would by then be empty, still pays its leaders 38.
//!
//! Each simulated block applies the block contents (which is where
//! `compute_block_rewards` runs) of an empty block. The release term reads
//! only the epoch's inferred total stake and the 120-block fee window, which
//! stays all-zero; the header path (nonce, voucher MMR, slot) does not feed
//! it, so it is applied only to open the run and to close the epoch, where
//! the accumulated leader rewards move into the claimable pool and become
//! observable.

use std::{
    collections::HashMap,
    num::{NonZero, NonZeroU32, NonZeroU64},
    sync::Arc,
};

use lb_core::{
    mantle::{
        Note, SignedOps, Utxo,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        ops::leader_claim::VoucherCm,
        transactions::states::Preverified,
    },
    proofs::leader_proof::{LeaderProof, LeaderPublic},
    sdp::{MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::{EpochConfig, Slot, UncleSlots};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{UnverifiedEd25519PublicKey, ZkPublicKey};
use lb_utils::math::{NonNegativeRatio, PositiveF64};
use logos_blockchain_ledger::{
    Config, LedgerState,
    config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{
        Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
    },
};

type Id = [u8; 32];
type Tx = SignedOps<Preverified, StandardMode>;

/// `B_0 = I_max * S_cap * Y = 0.01 * 10^10 * 10` (Parametrization table).
const RESERVE_B0: u128 = 1_000_000_000;

/// Blocks simulated: just past the point where 95 per block exhausts `B_0`
/// (`10^9 / 95 = 10 526 315.8`).
const BLOCKS: u64 = 10_530_000;

/// A leader proof that every slot accepts: the reward schedule does not depend
/// on who leads, only on the block being applied.
struct AnyLeader {
    leader_key: UnverifiedEd25519PublicKey,
    voucher_cm: VoucherCm,
}

impl AnyLeader {
    fn new() -> Self {
        Self {
            leader_key: UnverifiedEd25519PublicKey::from_bytes(&[0u8; 32])
                .expect("all-zero bytes form an unverified key"),
            voucher_cm: VoucherCm::default(),
        }
    }
}

impl LeaderProof for AnyLeader {
    fn verify(&self, _public_inputs: &LeaderPublic) -> bool {
        true
    }

    fn verify_genesis(&self) -> bool {
        true
    }

    fn entropy(&self) -> Fr {
        Fr::from(0u8)
    }

    fn leader_key(&self) -> &UnverifiedEd25519PublicKey {
        &self.leader_key
    }

    fn voucher_cm(&self) -> &VoucherCm {
        &self.voucher_cm
    }
}

/// k = 1, f = 1/10: a 100-slot epoch (3/3/4). The `PoW` fee share is 0.
fn config() -> Config {
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
        epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
        epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        NonZero::new(1).unwrap(),
        NonNegativeRatio::new(1, 10.try_into().unwrap()),
        1f64.try_into().expect("1 > 0"),
        NonZero::new(12).unwrap(),
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    let mut service_params = HashMap::new();
    service_params.insert(
        ServiceType::BlendNetwork,
        ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        },
    );

    Config {
        epoch_config,
        consensus_config,
        sdp_config: SdpConfig {
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
                    num_blend_layers: NonZeroU64::new(3).unwrap(),
                    minimum_network_size: NonZeroU64::new(1).unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        },
        faucet_pk: None,
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<234>(),
                target_transactions_per_block: NonZeroU64::new(10).unwrap(),
                max_step: NonZeroU64::new(4).unwrap(),
                damping_num: NonZeroU32::new(1).unwrap(),
                damping_den_offset: 1,
            },
            reward: RewardPoWConfig {
                reward_pool_genesis: 1_000_000_000,
                epoch_reward_genesis: 1_000_000,
                minimum_difficulty: ModulusShift::new::<26>(),
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
                target_claims_per_block: 100,
                rate_num: 0,
                rate_den: NonZeroU64::MIN,
                target_claim_per_block: NonZeroU64::MIN,
                pow_share: 0,
                share_den: NonZeroU64::MIN,
                slot_window: NonZeroU64::new(100).unwrap(),
            },
        },
    }
}

fn apply_header(state: LedgerState, slot: u64, config: &Config) -> LedgerState {
    state
        .try_apply_header::<_, Id>(
            Slot::from(slot),
            &AnyLeader::new(),
            &UncleSlots::default(),
            config,
        )
        .expect("the header applies")
        .0
}

fn apply_empty_contents(state: LedgerState, config: &Config) -> LedgerState {
    state
        .try_apply_block_contents::<Tx, Id, MainnetGasProfile>(config, std::iter::empty())
        .expect("empty block contents apply")
        .0
}

#[test]
fn reserve_release_stops_once_b0_is_exhausted() {
    let config = config();
    let genesis_note = Utxo {
        op_id: [7u8; 32],
        output_index: 0,
        note: Note::new(1, ZkPublicKey::from(Fr::from(34u64))),
    };
    let mut state = LedgerState::from_utxos([genesis_note], &config);
    assert_eq!(state.epoch_state().total_stake(), 1);

    state = apply_header(state, 1, &config);
    for _ in 0..BLOCKS {
        state = apply_empty_contents(state, &config);
    }
    // Close epoch 0 so the leader rewards credited so far become claimable.
    state = apply_header(state, 100, &config);
    let leader_rewards = u128::from(state.mantle_ledger().leaders.claimable_rewards());

    // One more release-only block (the contents of the block at slot 100),
    // then close epoch 1 so that block's leader share becomes claimable too.
    state = apply_empty_contents(state, &config);
    state = apply_header(state, 200, &config);
    let next_block_leader_reward =
        u128::from(state.mantle_ledger().leaders.claimable_rewards()) - leader_rewards;

    // Every reward here is reserve release (A_t = 1, no fees). By B_t >= 0
    // the rewards sum to at most B_0, so the leaders' floored 40 % shares sum
    // to at most 0.4 * B_0; and 10 530 000 releases of 62500/657 (or of the
    // 95 actually paid out) exhaust B_0, so the next block's release, and
    // with Rbar_t = 0 its whole reward, is zero.
    let leader_bound = RESERVE_B0 * 4 / 10;
    assert!(
        leader_rewards <= leader_bound && next_block_leader_reward == 0,
        "block-rewards pool accounting: iota_t = min(A_t*I_max*S_cap*Delta_t/f, B_(t-1)), \
         B_t >= 0, B_0 = {RESERVE_B0}: over {BLOCKS} release-only blocks the leaders' 40% \
         share is at most {leader_bound} and, the reserve being depleted, the next block \
         releases nothing; the ledger credited {leader_rewards} ({} per block) and then \
         {next_block_leader_reward} more for the next block, i.e. the release never stops",
        leader_rewards / u128::from(BLOCKS)
    );
}
