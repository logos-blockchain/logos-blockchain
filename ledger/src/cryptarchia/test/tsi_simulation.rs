//! A simulation measuring whether uncle references mitigate the total stake
//! inference gap that forks open.
//!
//! The simulation artificially builds a block tree with multiple forks,
//! expecting that referencing uncles in those forks will bring the TSI closer
//! to the actual total stake.
//!
//! The real cryptarchia-engine and cryptarchia-ledger are used.
//! - engine: fork choice and uncle selection
//! - ledger: epoch transition and TSI
//!
//! Only the lottery and the fork pattern are modelled here: a slot is occupied
//! with the probability the current estimated stake implies, and a block
//! extends either the honest tip or, with probability `fork_rate`, a diverging
//! branch.
//!
//! The simulation is `#[ignore]`d because it is long-running.
//! Run it with `--nocapture` to see the tabled results:
//! ```
//! cargo test -p logos-blockchain-ledger tsi_simulation -- \
//!   --ignored --nocapture
//! ```

use std::{num::NonZero, sync::Arc};

use lb_core::{
    crypto::{Digest as _, Hasher},
    mantle::{
        Note, SignedMantleTx, Utxo, Value,
        gas::MainnetGasProfile,
        transactions::{
            GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE, states::Preverified,
        },
    },
    sdp::Declarations,
};
use lb_cryptarchia_engine::{Branch, Cryptarchia, Slot, State, UncleSlots};
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::ZkKey;
use lb_utils::math::NonNegativeRatio;
use num_bigint::BigUint;
use rand::{Rng as _, SeedableRng as _, rngs::StdRng};

use crate::{
    Config, Ledger, WINDOW_SIZE,
    cryptarchia::{
        EpochState, LedgerState, UtxoTree,
        block_density::BlockDensity,
        stake::StakeInference,
        tests::{config, generate_proof},
    },
    mantle::{pow::PowState, sdp::SdpLedger},
};

type HeaderId = [u8; 32];

const GENESIS: HeaderId = [0; 32];

// NOTE: Tune the following constants to simulate different scenarios.

/// `k`, large enough that a stake inference period holds a few hundred
/// occupied slots and the measured density is not dominated by noise.
const SECURITY_PARAM: u32 = 100;
/// `f`, as a `(numerator, denominator)` pair.
const SLOT_ACTIVATION_COEFF: (u32, u32) = (1, 10);
const UNCLE_REFERENCE_WINDOW_IN_BLOCK: u32 = 12;
/// Epochs to simulate. The inference is a multiplicative update, so a handful
/// of epochs is enough for the estimate to settle.
const EPOCHS: u64 = 6;
/// Probability that a diverging block extends a live fork rather than opening
/// a new one, which is what makes forks longer.
const FORK_EXTENSION_RATE: f64 = 0.3;
/// Deepest divergence from the honest tip, in blocks.
const MAX_FORK_DEPTH: u32 = 3;
/// The stake the inference is expected to converge back to, held in full by
/// the single leader the simulation runs with.
const TOTAL_STAKE: Value = 10_000;
const SEED: u64 = 0x5eed;

#[test]
#[ignore = "long-running simulation"]
fn uncle_references_mitigate_the_stake_inference_gap() {
    println!(
        "{:>9} {:>14} {:>13} {:>16} {:>11} {:>13} {:>15} {:>8}",
        "fork_rate",
        "uncle_enabled",
        "total_blocks",
        "blocks_on_forks",
        "uncle_refs",
        "actual_stake",
        "inferred_stake",
        "error"
    );
    for fork_rate in [0.0, 0.05, 0.1, 0.2, 0.4] {
        for uncle_enabled in [false, true] {
            let outcome = simulate(Scenario {
                fork_rate,
                uncle_enabled,
                seed: SEED,
            });
            println!(
                "{:>9.2} {:>14} {:>13} {:>16} {:>11} {:>13} {:>15} {:>+7.2}%",
                fork_rate,
                uncle_enabled,
                outcome.total_blocks,
                outcome.blocks_on_forks,
                outcome.uncle_refs,
                TOTAL_STAKE,
                outcome.inferred_total_stake,
                outcome.error() * 100.0
            );
        }
    }
}

#[derive(Clone, Copy)]
struct Scenario {
    /// Probability that a block extends something other than the honest tip.
    fork_rate: f64,
    /// Whether block proposers reference uncles.
    uncle_enabled: bool,
    seed: u64,
}

struct Outcome {
    inferred_total_stake: Value,
    total_blocks: usize, // on the honest chain and on the forks alike
    blocks_on_forks: usize,
    uncle_refs: usize,
}

impl Outcome {
    /// How far the inferred stake sits from [`TOTAL_STAKE`], negative when it
    /// underestimates.
    fn error(&self) -> f64 {
        (self.inferred_total_stake as f64 - TOTAL_STAKE as f64) / TOTAL_STAKE as f64
    }
}

fn simulate(scenario: Scenario) -> Outcome {
    let config = simulation_config();
    let utxo = leader_utxo();
    let mut ledger = genesis_ledger(&config, utxo);
    let mut engine = Cryptarchia::from_lib(
        GENESIS,
        config.consensus_config.clone(),
        State::Online,
        0.into(),
        0,
        UncleSlots::default(),
    );
    let mut rng = StdRng::seed_from_u64(scenario.seed);
    let mut total_blocks = 0;
    let mut blocks_on_forks = 0;
    let mut uncle_refs = 0;

    for slot in 1..=config.epoch_length() * EPOCHS {
        let slot = Slot::from(slot);
        let estimate = current_estimated_total_stake(&ledger, &engine);
        if !slot_won(&config, estimate, &mut rng) {
            continue;
        }

        let parent = pick_parent(&engine, scenario.fork_rate, &mut rng);
        let uncle_slots = select_uncles(&engine, parent, slot, scenario.uncle_enabled);

        total_blocks += 1;
        blocks_on_forks += usize::from(parent != engine.tip());
        uncle_refs += uncle_slots.len();

        apply_block(&mut ledger, &mut engine, parent, slot, utxo, uncle_slots);
    }

    Outcome {
        inferred_total_stake: current_estimated_total_stake(&ledger, &engine),
        total_blocks,
        blocks_on_forks,
        uncle_refs,
    }
}

fn simulation_config() -> Config {
    let mut config = config();
    config.consensus_config = lb_cryptarchia_engine::Config::new(
        NonZero::new(SECURITY_PARAM).unwrap(),
        NonNegativeRatio::new(
            SLOT_ACTIVATION_COEFF.0,
            NonZero::new(SLOT_ACTIVATION_COEFF.1).unwrap(),
        ),
        1f64.try_into().expect("1 > 0"),
        NonZero::new(UNCLE_REFERENCE_WINDOW_IN_BLOCK).unwrap(),
    );
    config
}

/// The note of the only leader, holding [`TOTAL_STAKE`].
fn leader_utxo() -> Utxo {
    Utxo {
        op_id: [0u8; 32],
        output_index: 0,
        note: Note::new(
            TOTAL_STAKE,
            ZkKey::from(BigUint::from(0u64)).to_public_key(),
        ),
    }
}

/// A ledger holding a single leader whose note is the entire stake.
fn genesis_ledger(config: &Config, leader_utxo: Utxo) -> Ledger<HeaderId> {
    let total_stake = leader_utxo.note.value;
    let (lottery_0, lottery_1) = config
        .lottery_constants()
        .compute_lottery_values(total_stake);
    let utxos: UtxoTree = std::iter::once((leader_utxo.id(), leader_utxo)).collect();
    let epoch_state = EpochState {
        epoch: 0.into(),
        nonce: Fr::ZERO,
        utxos: utxos.clone(),
        total_stake,
        lottery_0,
        lottery_1,
        active_declarations: Arc::new(Declarations::default()),
        blend_pow_difficulty: Fr::ZERO,
    };
    let cryptarchia_ledger = LedgerState {
        utxos,
        nonce: Fr::ZERO,
        slot: 0.into(),
        next_epoch_state: EpochState {
            epoch: 1.into(),
            ..epoch_state.clone()
        },
        stake_inference: Arc::new(StakeInference::new(
            config.consensus_config.stake_inference_learning_rate(),
            config.consensus_config.slot_activation_coeff().as_f64(),
            config.total_stake_inference_period(),
        )),
        block_density: BlockDensity::new(config.epoch(0.into()), config),
        epoch_state,
        fee_window: [0.into(); WINDOW_SIZE],
        average_execution_gas: 0.into(),
        execution_base_fee: GENESIS_EXECUTION_GAS_PRICE,
        storage_gas_ema: 0.into(),
        storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        storage_gas_consumed_in_epoch: 0.into(),
    };
    let state = crate::LedgerState {
        block_number: 0,
        mantle_ledger: crate::mantle::LedgerState::new(config, cryptarchia_ledger.epoch_state()),
        cryptarchia_ledger,
    };
    Ledger::new(GENESIS, state, config.clone())
}

fn block_id(parent: HeaderId, slot: Slot) -> HeaderId {
    Hasher::new()
        .chain_update(parent)
        .chain_update(slot.to_le_bytes())
        .finalize()
        .into()
}

/// The total stake that the ledger currently infers, read from the honest tip.
fn current_estimated_total_stake(
    ledger: &Ledger<HeaderId>,
    engine: &Cryptarchia<HeaderId>,
) -> Value {
    ledger
        .state(&engine.tip())
        .expect("tip state")
        .cryptarchia_ledger
        .epoch_state
        .total_stake
}

/// Runs the lottery of the only leader, who holds [`TOTAL_STAKE`].
///
/// The winning probability is `1 - (1-f)^(v/S)`, where
/// - f: the slot activation coefficient
/// - v: the stake of a leader
/// - S: the estimated total stake
fn slot_won(config: &Config, total_stake_estimate: Value, rng: &mut StdRng) -> bool {
    let slot_activation_coeff = config.consensus_config.slot_activation_coeff().as_f64();
    let winning_prob =
        1.0 - (1.0 - slot_activation_coeff).powf(TOTAL_STAKE as f64 / total_stake_estimate as f64);
    rng.gen_bool(winning_prob)
}

/// Picks the parent of a new block: the honest tip, or a diverging one with
/// the probability `fork_rate`.
fn pick_parent(engine: &Cryptarchia<HeaderId>, fork_rate: f64, rng: &mut StdRng) -> HeaderId {
    if rng.gen_bool(fork_rate) {
        diverging_parent(engine, rng)
    } else {
        engine.tip()
    }
}

/// Picks a parent of a new block: either the tip of a live fork, or
/// an ancestor of the honest tip, which opens a new fork.
fn diverging_parent(engine: &Cryptarchia<HeaderId>, rng: &mut StdRng) -> HeaderId {
    let fork_tips = engine.non_canonical_forks().collect::<Vec<_>>();
    if !fork_tips.is_empty() && rng.gen_bool(FORK_EXTENSION_RATE) {
        return fork_tips[rng.gen_range(0..fork_tips.len())].id();
    }
    let mut ancestor = engine.tip();
    for _ in 0..rng.gen_range(1..=MAX_FORK_DEPTH) {
        let Some(branch) = engine.branches().get(&ancestor) else {
            break;
        };
        if branch.parent() == ancestor {
            // reached the genesis block
            break;
        }
        ancestor = branch.parent();
    }
    ancestor
}

/// Selects the uncles that a block extending `parent` at `slot` references,
/// or none if uncle referencing is disabled.
fn select_uncles(
    engine: &Cryptarchia<HeaderId>,
    parent: HeaderId,
    slot: Slot,
    uncle_enabled: bool,
) -> UncleSlots {
    if !uncle_enabled {
        return UncleSlots::default();
    }
    let uncle_slots = engine
        .select_uncles(engine.branches().get(&parent).unwrap(), slot)
        .into_iter()
        .map(Branch::slot)
        .collect::<Vec<_>>();
    UncleSlots::try_from(uncle_slots).unwrap()
}

/// Applies a block to both the ledger and the engine, and drops the ledger
/// states of the blocks that the engine pruned.
fn apply_block(
    ledger: &mut Ledger<HeaderId>,
    engine: &mut Cryptarchia<HeaderId>,
    parent: HeaderId,
    slot: Slot,
    leader_utxo: Utxo,
    uncle_slots: UncleSlots,
) {
    let block = apply_block_to_ledger(ledger, parent, slot, leader_utxo, &uncle_slots);
    let (pruned, _) = engine
        .receive_block(block, parent, slot, uncle_slots)
        .expect("engine update");
    for pruned_block in pruned.all() {
        ledger.prune_state_at(pruned_block);
    }
}

/// Applies a block to the real ledger, by generating a dummy proof.
fn apply_block_to_ledger(
    ledger: &mut Ledger<HeaderId>,
    parent: HeaderId,
    slot: Slot,
    utxo: Utxo,
    uncle_slots: &UncleSlots,
) -> HeaderId {
    let parent_state = ledger
        .state(&parent)
        .expect("parent state")
        .clone()
        .cryptarchia_ledger
        .update_epoch_state::<HeaderId>(
            slot,
            &SdpLedger::new(0.into()),
            &PowState::new(),
            ledger.config(),
        )
        .expect("epoch state update");
    let id = block_id(parent, slot);
    let proof = generate_proof(&parent_state, &utxo, slot);
    let (_, state, _) = ledger
        .prepare_update::<_, _, MainnetGasProfile>(
            id,
            parent,
            slot,
            &proof,
            uncle_slots,
            std::iter::empty::<&SignedMantleTx<Preverified>>(),
        )
        .expect("ledger update");
    ledger.commit_update(id, state);
    id
}
