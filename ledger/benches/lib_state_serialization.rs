//! Measure the cost of writing the `LibState` snapshot persisted with each LIB.
//!
//! Run with:
//! `cargo bench -p logos-blockchain-ledger --bench lib_state_serialization
//! --features test-utils`

use std::{
    collections::HashMap,
    num::{NonZero, NonZeroU32},
    sync::Arc,
    time::Duration,
};

use divan::Bencher;
use lb_binary_codec::bincode::SerializeOp as _;
use lb_core::{
    era::{AcceptedForkDigests, ForkDigest},
    mantle::{
        GenesisParameters, GenesisTime,
        gas::{Gas, GasProfile},
        note_set::NoteSetIndex,
        notes::{InputNullifiers, NoteCommitment, OutputCommitments},
        ops::channel::withdraw::{PendingWithdrawal, PendingWithdrawals},
    },
    sdp::{MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::{Config as CryptarchiaConfig, EpochConfig, EraSchedule, Slot};
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_kv_store::{Bytes, ErasedKv, MemKv};
use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraTiming};
use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};
use logos_blockchain_ledger::{
    Config, Ledger, LedgerState, LibState,
    config::{
        BlendPoWConfig, BlockRewardsConfig, BlockRewardsParameters, EraScheduledConfig,
        ExecutionMarketConfig, MaxGasPerBlock, ModulusShift, PoWConfig, RewardPoWConfig,
        StorageMarketConfig,
    },
    mantle::sdp::{Config as SdpConfig, ServiceRewardsParameters, rewards},
};
use time::OffsetDateTime;

const BACKLOG_SIZES: [usize; 3] = [0, 1_024, 8_192];
const REPEATED_LIB_ADVANCEMENTS: usize = 16;
const SAMPLE_COUNT: u32 = 10;
type Snapshot = LibState<[u8; 32], u8>;

fn main() {
    for backlog_size in BACKLOG_SIZES {
        let bytes = serialize(&snapshot(backlog_size)).len();
        println!(
            "LIB snapshot backlog={backlog_size}: {bytes} encoded bytes; {REPEATED_LIB_ADVANCEMENTS} advancements={}",
            bytes * REPEATED_LIB_ADVANCEMENTS
        );
    }
    divan::main();
}

#[divan::bench(args = BACKLOG_SIZES, sample_count = SAMPLE_COUNT)]
fn serialize_lib_snapshot(bencher: Bencher, backlog_size: usize) {
    bencher
        .with_inputs(|| snapshot(backlog_size))
        .bench_values(|snapshot| serialize(&snapshot));
}

#[divan::bench(args = BACKLOG_SIZES, sample_count = SAMPLE_COUNT)]
fn serialize_repeated_lib_advancements(bencher: Bencher, backlog_size: usize) {
    bencher
        .with_inputs(|| repeated_snapshots(backlog_size))
        .bench_values(|snapshots| snapshots.iter().map(serialize).collect::<Vec<Bytes>>());
}

fn serialize(snapshot: &Snapshot) -> Bytes {
    Snapshot::to_bytes(snapshot).expect("the LIB snapshot serializes")
}

fn snapshot(backlog_size: usize) -> Snapshot {
    let pending_withdrawals =
        (0..backlog_size).fold(PendingWithdrawals::new(), |pending, index| {
            pending.push(PendingWithdrawal {
                due: Slot::from(u64::MAX),
                note_set_index: NoteSetIndex::Sdp,
                inputs: InputNullifiers::default(),
                outputs: OutputCommitments::try_from_iter([NoteCommitment(Fr::from(
                    index as u64 + 1,
                ))])
                .expect("one output is within the withdrawal output bound"),
            })
        });
    let config = config();
    let store = ErasedKv::into_dyn(MemKv::new());
    let state = LedgerState::from_notes([], &config, &genesis_parameters())
        .with_pending_withdrawals(pending_withdrawals)
        .with_nullifier_store(&store)
        .expect("the benchmark genesis state has valid nullifier trees");
    let lib = [0; 32];
    let ledger = Ledger::new(lib, state, Arc::new(single_era(config)))
        .expect("the benchmark genesis ledger finalizes its nullifier trees");
    let state = ledger.state(&lib).unwrap().clone();
    LibState {
        lib,
        state,
        info: 0,
    }
}

fn repeated_snapshots(backlog_size: usize) -> Vec<Snapshot> {
    let base = snapshot(backlog_size);
    (1..=REPEATED_LIB_ADVANCEMENTS)
        .map(|advancement| Snapshot {
            lib: [advancement as u8; 32],
            state: base.state.clone(),
            info: advancement as u8,
        })
        .collect()
}

fn genesis_parameters() -> GenesisParameters {
    GenesisParameters {
        chain_id: "lib-snapshot-bench".to_owned().try_into().unwrap(),
        start_time: GenesisTime::new(0),
        epoch_nonce: Fr::ZERO,
        max_supply: 10_000_000_000_000_000_000,
        pow_reward_pool: 0,
        rewards_reserve: 1_000_000_000_000_000_000,
        storage_gas_price: NonZero::new(1_000_000_000).unwrap(),
        execution_gas_price: NonZero::new(1_000_000_000).unwrap(),
    }
}

fn single_era(config: Config) -> EraScheduledConfig {
    EraSchedule::new(
        OffsetDateTime::UNIX_EPOCH,
        EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: EpochLength::epoch_length(&config),
        },
        EraEntriesAfterGenesis::empty(),
    )
    .expect("a single era resolves")
    .map(move |_| config.clone())
}

const GAS_PROFILE: GasProfile = GasProfile {
    transfer: Gas::new(10),
    program_register: Gas::new(15114),
    channel_config: Gas::new(24),
    channel_inscribe: Gas::new(26),
    channel_deposit: Gas::new(26),
    channel_withdraw: Gas::new(33),
    sdp_declare: Gas::new(1693),
    sdp_withdraw: Gas::new(0),
    claim_pow_reward: Gas::new(1506),
    sdp_active: Gas::new(15117),
    kilobyte: Gas::new(30),
    note_set: Gas::new(8961),
    proof: Gas::new(4177),
    root: Gas::new(2332),
    buffered: Gas::new(60),
    nullifier: Gas::new(1758),
    commitment: Gas::new(58),
    signature: Gas::new(557),
    user_step: Gas::new(28),
    program_deposit: Gas::new(144),
    program_step: Gas::new(13528),
    opening: Gas::new(470),
};

fn config() -> Config {
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
        epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
        epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
    };
    let consensus_config = CryptarchiaConfig::new(
        NonZero::new(1).unwrap(),
        ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
        1f64.try_into().expect("1 > 0"),
        12,
        4,
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    Config {
        epoch_config,
        consensus_config,
        sdp_config: SdpConfig {
            service_params: Arc::new(HashMap::from([(
                ServiceType::BlendNetwork,
                ServiceParameters {
                    inactivity_period: 2.try_into().unwrap(),
                },
            )])),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN).unwrap(),
                    cover_replication_factor: 0,
                    num_blend_layers: NonZero::new(3).unwrap(),
                    minimum_network_size: NonZero::new(1).unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: MinStake { threshold: 1 },
        },
        max_transactions: 1024,
        max_transactions_size: 2 * 1024 * 1024,
        max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
        root_window: 1024,
        channel_withdraw_delay: 86_400.into(),
        execution_market: ExecutionMarketConfig {
            max_gas_per_block: MaxGasPerBlock::new(3_137_798).unwrap(),
            fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
        },
        storage_market: StorageMarketConfig {
            max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
        },
        block_rewards: BlockRewardsConfig::new(
            BlockRewardsParameters {
                max_emission_rate: ProperFraction::new(1, NonZero::new(100).unwrap()).unwrap(),
                min_emission_rate: NonNegativeRatio::new(0, NonZero::new(1).unwrap()),
                stake_target: ProperFraction::new(3, NonZero::new(10).unwrap()).unwrap(),
                pooled_fees_target: NonNegativeRatio::new(1, NonZero::new(1).unwrap()),
                stake_deviation_weight: NonNegativeRatio::new(1, NonZero::new(4).unwrap()),
                pooled_fees_weight: NonNegativeRatio::new(1, NonZero::new(1).unwrap()),
                pooled_fees_window: NonZero::new(120).unwrap(),
                blend_share: ProperFraction::new(6, NonZero::new(10).unwrap()).unwrap(),
            },
            10_000_000_000_000_000_000,
            NonZero::new(1).unwrap(),
            ProperFraction::new(1, NonZero::new(30).unwrap()).unwrap(),
        )
        .unwrap(),
        gas_profile: GAS_PROFILE,
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<234>(),
                target_transactions_per_block: NonZero::new(10).unwrap(),
                max_step: NonZero::new(4).unwrap(),
                damping_num: NonZero::new(1).unwrap(),
                damping_den_offset: 1,
            },
            reward: RewardPoWConfig {
                minimum_difficulty: ModulusShift::new::<26>(),
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZero::new(10).unwrap(),
                target_claims_per_block: NonZero::<u64>::new(100).unwrap(),
                rate_num: 0,
                rate_den: NonZero::<u64>::MIN,
                pow_share: 0,
                share_den: NonZero::<u64>::MIN,
                slot_window: NonZero::new(100).unwrap(),
            },
        },
        accepted_fork_digests: AcceptedForkDigests::new(vec![ForkDigest::new([0x11; 32])]),
    }
}
