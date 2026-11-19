//! Benchmarks for the per-block proof batch verification
//!
//! Run with `cargo bench -p logos-blockchain-ledger --bench zk_batch`.
//!
//! These measure the payoff of batch-verifying a block's ZK proofs at the
//! level a node actually works at, inclusing tx validations & executions.
//!
//! The benchmarks compare two results:
//! - sequential path: every tx's proof is verified one by one.
//! - batched path: all tx proofs are batched together and verified in one go.
//!
//! The benchmarks use only `Transfer` operations that include a `ZkTransfer`,
//! because the ZK cost of other operations is not so different from `Transfer`.
//!
//! Multi-block scenarios are also included. But, since the multi-block batching
//! is not implemented yet, the expected performance gain is simply the sum of
//! the single-block gains. These benchmarks exist primarily to verify the
//! expectation.

use std::{
    collections::HashMap,
    num::{NonZero, NonZeroU32},
    sync::{Arc, LazyLock},
    time::Duration,
};

use lb_core::{
    era::{AcceptedForkDigests, ForkDigest},
    header::HeaderId,
    mantle::{
        GenesisParameters, GenesisTime, Op, OpProof, SignedOps,
        batch::DeferredProofs,
        gas::{Gas, GasProfile},
        ledger::verification_mode::StandardMode,
        note_set::CommitmentMmr,
        notes::{self, derive_note_nullifier},
        ops::{
            ZkTransferProof,
            transfer::{TransferOp, test_utils::prove_transfer},
        },
        traits::Hashable as _,
        transactions::{OpProofs, Ops, states::Preverified},
    },
    sdp::{MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::EpochConfig;
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::ZkKey;
use lb_mmr::MerklePath;
use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraSchedule, EraTiming};
use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};
use logos_blockchain_ledger::{
    Config, LedgerState,
    config::{BlendPoWConfig, EraScheduledConfig, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use time::OffsetDateTime;

fn main() {
    divan::main();
}

/// Samples `divan` takes per benchmark.
/// 10 samples are sufficient since the benchmark result is quite stable.
const SAMPLE_COUNT: u32 = 10;

/// Single-block benchmarks.
mod single_block {
    use divan::{Bencher, counter::ItemsCount};

    use crate::{SAMPLE_COUNT, TX_POOL, apply, apply_batched, apply_sequential};

    /// Transactions per block. Each transaction contains one `Transfer`
    /// operation, which contains a ZK signature.
    pub const TXS_PER_BLOCK: [usize; 7] = [1, 4, 16, 64, 256, 512, 1024];

    /// Processes a block with batched proof verification.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn batched_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(txs_per_block))
            .with_inputs(|| pool.txs[..txs_per_block].to_vec())
            .bench_values(|txs| apply_batched(&pool.genesis, txs.into_iter()));
    }

    /// Processes a block without batched proof verification.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn sequential_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(txs_per_block))
            .with_inputs(|| pool.txs[..txs_per_block].to_vec())
            .bench_values(|txs| apply_sequential(&pool.genesis, txs.into_iter()));
    }

    /// Verify/execute txs in a block, without verifying proofs.
    /// This is the baseline for the other two benchmarks in this module.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn no_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(txs_per_block))
            .with_inputs(|| pool.txs[..txs_per_block].to_vec())
            .bench_values(|txs| apply(&pool.genesis, txs.into_iter()).0);
    }
}

/// Multi-block benchmarks with per-block batching.
///
/// NOTE: These do not benchmark multi-block proof batching, which is not
/// implemented yet. Instead, they process multiple blocks with per-block proof
/// batching. Therefore, the expected performance gain is simply the sum of the
/// single-block gains. There benchmarks exist primarily to verify the
/// expectation.
mod multi_block_with_per_block_batching {
    use divan::{Bencher, counter::ItemsCount};

    use crate::{SAMPLE_COUNT, TX_POOL, apply_batched, apply_sequential};

    const N_BLOCKS: usize = 16;
    const TXS_PER_BLOCK: [usize; 4] = [1, 4, 16, 64];

    /// Processes multiple blocks with per-block batched proof verification.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn batched_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(N_BLOCKS))
            .with_inputs(|| {
                pool.txs
                    .chunks(txs_per_block)
                    .take(N_BLOCKS)
                    .map(<[_]>::to_vec)
                    .collect::<Vec<_>>()
            })
            .bench_values(|blocks| {
                let mut state = pool.genesis.clone();
                for txs in blocks {
                    state = apply_batched(&state, txs.into_iter());
                }
                state
            });
    }

    /// Processes multiple blocks without batched proof verification.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn sequential_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(N_BLOCKS))
            .with_inputs(|| {
                pool.txs
                    .chunks(txs_per_block)
                    .take(N_BLOCKS)
                    .map(<[_]>::to_vec)
                    .collect::<Vec<_>>()
            })
            .bench_values(|blocks| {
                let mut state = pool.genesis.clone();
                for txs in blocks {
                    state = apply_sequential(&state, txs.into_iter());
                }
                state
            });
    }
}

/// A pool of prebuilt transactions.
/// All benchmarks use the same pool to save the cost of building proofs.
///
/// Each transaction contains one `Transfer` operation, which contains a ZK
/// signature.
///
/// The genesis state seeded with one UTXO per transaction. Each transaction
/// spends one of them.
struct TxPool {
    eras: EraScheduledConfig,
    genesis: LedgerState,
    txs: Vec<SignedOps<Preverified, StandardMode>>,
}

static TX_POOL: LazyLock<TxPool> = LazyLock::new(|| {
    let config = config();
    let n_txs = *single_block::TXS_PER_BLOCK.last().unwrap();

    let keys: Vec<ZkKey> = (0..n_txs)
        .map(|i| ZkKey::new(Fr::from(i as u64 + 1)))
        .collect();
    let notes: Vec<notes::Note> = keys
        .iter()
        .enumerate()
        // Assign a large value to withstand gas cost increases.
        .map(|(i, key)| {
            notes::Note::new(10_000_000_000_000, Fr::from(i as u64), key.to_public_key())
        })
        .collect();

    // Each push updates the paths of the earlier notes in place.
    let mut mmr = CommitmentMmr::new();
    let mut paths: Vec<MerklePath> = Vec::with_capacity(n_txs);
    for note in &notes {
        let (pushed, path) = mmr
            .push_with_paths(note.commitment(), &mut paths)
            .expect("the commitment MMR has room");
        mmr = pushed;
        paths.push(path);
    }
    let root = mmr.frontier_root();

    let genesis = LedgerState::from_notes(notes.iter().copied(), &config, &genesis_parameters());
    assert_eq!(genesis.latest_note_sets().ledger().commitment_root(), root);

    let txs = notes
        .iter()
        .zip(&keys)
        .zip(paths)
        .enumerate()
        .map(|(i, ((note, key), path))| {
            build_tx(
                *note,
                *key.as_fr(),
                path,
                root,
                Fr::from((i + n_txs) as u64),
            )
        })
        .collect();

    TxPool {
        genesis,
        eras: single_era(config),
        txs,
    }
});

fn apply_batched(
    state: &LedgerState,
    txs: impl Iterator<Item = SignedOps<Preverified, StandardMode>>,
) -> LedgerState {
    let (state, deferred) = apply(state, txs);
    deferred.verify().expect("proofs should verify");
    state
}

fn apply_sequential(
    state: &LedgerState,
    txs: impl Iterator<Item = SignedOps<Preverified, StandardMode>>,
) -> LedgerState {
    let (state, deferred) = apply(state, txs);
    for deferred_proof in deferred.zk_transfers() {
        assert!(
            lb_zktransfer::verify(deferred_proof.proof(), deferred_proof.inputs())
                .expect("proof should verify")
        );
    }
    state
}

fn apply(
    state: &LedgerState,
    txs: impl Iterator<Item = SignedOps<Preverified, StandardMode>>,
) -> (LedgerState, DeferredProofs) {
    let (state, gas_and_fees, _, deferred) = state
        .clone()
        .try_apply_block_transactions::<_, HeaderId>(&TX_POOL.eras, txs)
        .expect("block should apply");
    let reward_value = state
        .leader_reward::<HeaderId>(&gas_and_fees, &TX_POOL.eras)
        .expect("block should apply");
    let (state, _) = state
        .try_finish_block::<HeaderId>(
            &TX_POOL.eras,
            &gas_and_fees,
            notes::NoteCommitment(Fr::ZERO),
            reward_value,
        )
        .expect("block should apply");
    (state, deferred)
}

/// Builds a transaction with a `Transfer` operation that spends `note` and
/// sends the very little amount back to its public key.
fn build_tx(
    note: notes::Note,
    secret_key: Fr,
    path: MerklePath,
    root: Fr,
    output_nonce: Fr,
) -> SignedOps<Preverified, StandardMode> {
    let output = notes::Note::new(1, output_nonce, note.public_key);
    let transfer_op = TransferOp::new(
        [derive_note_nullifier(
            &note.commitment(),
            &ZkKey::new(secret_key).nullifier_key(),
        )]
        .into(),
        [output.commitment()].into(),
        root,
        // Most of the tx's value goes as a tip, to withstand gas cost increases.
        9_999_999_999_999,
    );
    let ops = Ops::new(ForkDigest::new([0x11; 32]), [Op::Transfer(transfer_op)]);
    let proof = prove_transfer(
        &[(note, secret_key, path)],
        &[output],
        root,
        ops.hash().to_fr(),
    );
    let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(proof))]);

    SignedOps::from_parts(ops, op_proofs)
        .expect("transaction should build")
        .preverify()
        .expect("transaction should preverify")
}

/// `config` as the only era, from genesis.
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

/// The costs of the deployments' current gas profile.
const GAS_PROFILE: GasProfile = GasProfile {
    transfer: Gas::new(10),
    program_register: Gas::new(15114),
    channel_config: Gas::new(24),
    channel_inscribe: Gas::new(26),
    channel_deposit: Gas::new(26),
    channel_withdraw: Gas::new(33),
    sdp_declare: Gas::new(1693),
    sdp_withdraw: Gas::new(0),
    sdp_active: Gas::new(15117),
    claim_pow_reward: Gas::new(1506),
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
/// The genesis parameters of the benchmark chain: the supply cap and the
/// genesis gas prices of the deployments.
fn genesis_parameters() -> GenesisParameters {
    GenesisParameters {
        chain_id: "bench".to_owned().try_into().unwrap(),
        start_time: GenesisTime::new(0),
        epoch_nonce: Fr::ZERO,
        max_supply: 10_000_000_000_000_000_000,
        pow_reward_pool: 0,
        rewards_reserve: 1_000_000_000_000_000_000,
        storage_gas_price: NonZero::new(1_000_000_000).unwrap(),
        execution_gas_price: NonZero::new(1_000_000_000).unwrap(),
    }
}

/// The block rewards of the specification, on the supply cap of the
/// deployments and a block every 30 seconds.
const BLOCK_REWARDS: logos_blockchain_ledger::config::BlockRewardsConfig =
    logos_blockchain_ledger::config::BlockRewardsConfig::new(
        logos_blockchain_ledger::config::BlockRewardsParameters {
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
    .unwrap();

fn config() -> Config {
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
        epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
        epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
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
        sdp_config: logos_blockchain_ledger::mantle::sdp::Config {
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
        execution_market: logos_blockchain_ledger::config::ExecutionMarketConfig {
            max_gas_per_block: logos_blockchain_ledger::config::MaxGasPerBlock::new(3_137_798)
                .unwrap(),
            fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
        },
        storage_market: logos_blockchain_ledger::config::StorageMarketConfig {
            max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
        },
        block_rewards: BLOCK_REWARDS,
        gas_profile: GAS_PROFILE,
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<234>(),
                target_transactions_per_block: NonZero::new(10).unwrap(),
                max_step: NonZero::new(4).unwrap(),
                damping_num: NonZero::new(1).unwrap(),
                damping_den_offset: 1,
            },
            // `rate_num = 0` pays no reward, which keeps the benchmark measuring
            // transaction processing only.
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
