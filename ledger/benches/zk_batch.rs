//! Benchmarks for the per-block ZKP batch verification
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
//! The benchmarks use only `Transfer` operations that include a ZK signature,
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
};

use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Op, OpProof, SignedMantleTx, Utxo,
        batch::DeferredZkpVerifications,
        gas::MainnetGasProfile,
        ledger::{Inputs, Outputs},
        ops::transfer::TransferOp,
        traits::Hashable as _,
        transactions::{mantle_tx::RawMantleTx, states::Preverified},
    },
    sdp::{MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::EpochConfig;
use lb_key_management_system_keys::keys::ZkKey;
use lb_utils::math::{NonNegativeRatio, PositiveF64};
use lb_zksign::verify;
use logos_blockchain_ledger::{
    Config, LedgerState,
    config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use num_bigint::BigUint;

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
            .bench(|| apply_batched(&pool.genesis, &pool.txs[..txs_per_block]));
    }

    /// Processes a block without batched proof verification.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn sequential_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(txs_per_block))
            .bench(|| apply_sequential(&pool.genesis, &pool.txs[..txs_per_block]));
    }

    /// Verify/execute txs in a block, without verifying proofs.
    /// This is the baseline for the other two benchmarks in this module.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn no_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher
            .counter(ItemsCount::new(txs_per_block))
            .bench(|| apply(&pool.genesis, &pool.txs[..txs_per_block]).0);
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
        bencher.counter(ItemsCount::new(N_BLOCKS)).bench(|| {
            let mut state = pool.genesis.clone();
            for txs in pool.txs.chunks(txs_per_block).take(N_BLOCKS) {
                state = apply_batched(&state, txs);
            }
            state
        });
    }

    /// Processes multiple blocks without batched proof verification.
    #[divan::bench(args = TXS_PER_BLOCK, sample_count = SAMPLE_COUNT)]
    fn sequential_proofs(bencher: Bencher, txs_per_block: usize) {
        let pool = &*TX_POOL;
        bencher.counter(ItemsCount::new(N_BLOCKS)).bench(|| {
            let mut state = pool.genesis.clone();
            for txs in pool.txs.chunks(txs_per_block).take(N_BLOCKS) {
                state = apply_sequential(&state, txs);
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
    config: Config,
    genesis: LedgerState,
    txs: Vec<SignedMantleTx<Preverified>>,
}

static TX_POOL: LazyLock<TxPool> = LazyLock::new(|| {
    let config = config();
    let n_txs = *single_block::TXS_PER_BLOCK.last().unwrap();

    let keys: Vec<ZkKey> = (0..n_txs)
        .map(|i| ZkKey::from(BigUint::from(i as u64 + 1)))
        .collect();
    let utxos: Vec<Utxo> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| Utxo {
            op_id: [i as u8; 32],
            output_index: i,
            // Assign a large value to withstand gas cost increases.
            note: Note::new(1_000_000, key.to_public_key()),
        })
        .collect();

    let txs = utxos
        .iter()
        .zip(&keys)
        .map(|(utxo, key)| build_tx(*utxo, key))
        .collect();

    TxPool {
        genesis: LedgerState::from_utxos(utxos, &config),
        config,
        txs,
    }
});

fn apply_batched(state: &LedgerState, txs: &[SignedMantleTx<Preverified>]) -> LedgerState {
    let (state, deferred) = apply(state, txs);
    deferred.verify().expect("proofs should verify");
    state
}

fn apply_sequential(state: &LedgerState, txs: &[SignedMantleTx<Preverified>]) -> LedgerState {
    let (state, deferred) = apply(state, txs);
    for (proof, inputs) in deferred.zk_sigs() {
        assert!(verify(proof, inputs).expect("proof should verify"));
    }
    state
}

fn apply(
    state: &LedgerState,
    txs: &[SignedMantleTx<Preverified>],
) -> (LedgerState, DeferredZkpVerifications) {
    let (state, _, deferred) = state
        .clone()
        .try_apply_contents::<_, HeaderId, MainnetGasProfile>(&TX_POOL.config, txs.iter())
        .expect("block should apply");
    (state, deferred)
}

/// Builds a transaction with a `Transfer` operation that spends `utxo` and
/// sends the very little amount to `key`.
fn build_tx(utxo: Utxo, key: &ZkKey) -> SignedMantleTx<Preverified> {
    let transfer_op = TransferOp::new(
        Inputs::new([utxo.id()]),
        // Most of the tx's value goes as a tip, to withstand gas cost increases.
        Outputs::new([Note::new(1, key.to_public_key())]),
    );
    let mantle_tx = RawMantleTx([Op::Transfer(transfer_op)].into());
    let signature =
        ZkKey::multi_sign(std::slice::from_ref(key), &mantle_tx.hash().to_fr()).unwrap();

    SignedMantleTx::new(mantle_tx, [OpProof::ZkSig(signature)].into())
        .preverify()
        .expect("transaction should preverify")
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
        NonZero::new(12).unwrap(),
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
        faucet_pk: None,
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
                reward_pool_genesis: 1_000_000_000,
                epoch_reward_genesis: 1_000_000,
                initial_difficulty_seed: 1_000,
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZero::new(10).unwrap(),
                target_claims_per_block: 100,
                rate_num: 0,
                rate_den: NonZero::<u64>::MIN,
                target_claim_per_block: NonZero::<u64>::MIN,
                slot_window: NonZero::new(100).unwrap(),
            },
        },
    }
}
