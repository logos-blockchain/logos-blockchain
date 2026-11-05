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
    num::NonZero,
    sync::{Arc, LazyLock},
    time::Duration,
};

use lb_core::{
    header::HeaderId,
    mantle::{
        Op, OpProof, SignedOps,
        batch::DeferredProofs,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        note_set::{CommitmentMmr, LEDGER_SET},
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
use lb_utils::math::{NonNegativeRatio, PositiveF64};
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

    let genesis = LedgerState::from_notes(notes.iter().copied(), &config, 0);
    assert_eq!(
        genesis
            .latest_note_sets()
            .get(LEDGER_SET)
            .expect("the ledger set exists")
            .commitment_root(),
        root
    );

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
        .try_apply_block_transactions::<_, HeaderId, MainnetGasProfile>(&TX_POOL.eras, txs)
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
        [derive_note_nullifier(&note.commitment(), &secret_key)].into(),
        [output.commitment()].into(),
        root,
        // Most of the tx's value goes as a tip, to withstand gas cost increases.
        9_999_999_999_999,
    );
    let ops = Ops::from([Op::Transfer(transfer_op)]);
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
                    epoch: 0.into(),
                },
            )])),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
                    num_blend_layers: NonZero::new(3).unwrap(),
                    minimum_network_size: NonZero::new(1).unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        },
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
                target_claims_per_block: 100,
                rate_num: 0,
                rate_den: NonZero::<u64>::MIN,
                target_claim_per_block: NonZero::<u64>::MIN,
                pow_share: 0,
                share_den: NonZero::<u64>::MIN,
                slot_window: NonZero::new(100).unwrap(),
            },
        },
    }
}
