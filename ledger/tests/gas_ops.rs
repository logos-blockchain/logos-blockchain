//! Tests of the `ops` and `block` cases of the gas benchmark, which live in
//! `benches/gas`.
//!
//! Run with
//!
//! ```text
//! GAS_BENCH_FILL=1000 taskset -c 8 cargo test -p logos-blockchain-ledger --release --features test-utils --test gas_ops -- --test-threads=1
//! ```
//!
//! The cycle counters are read through `perf_event`, on Linux only.

#![cfg(target_os = "linux")]

#[path = "../benches/gas/block.rs"]
mod block;
#[path = "../benches/gas/cycles.rs"]
mod cycles;
#[path = "../benches/gas/fixture.rs"]
mod fixture;
#[path = "../benches/gas/ops.rs"]
mod ops;

use std::collections::BTreeMap;

use cycles::{Case, Counter};
use lb_core::{
    events::{BlockEvent, HeaderEvent},
    mantle::note_set::LEDGER_SET,
    sdp::ServiceType,
};

// The fewest cycles of a few runs of the case named `name` with `dims`.
fn cycles_of(cases: &mut [Case], name: &str, dims: &[(&str, u64)], counter: &mut Counter) -> u64 {
    let case = cases
        .iter_mut()
        .find(|case| case.name == name && case.dims == dims)
        .unwrap_or_else(|| panic!("no case {name} {dims:?}"));
    std::iter::repeat_with(|| (case.run)(counter))
        .take(5)
        .min()
        .expect("five runs")
}

fn dim(dims: &[(&str, u64)], name: &str) -> Option<u64> {
    dims.iter()
        .find_map(|(dim, value)| (*dim == name).then_some(*value))
}

fn series(names: impl Iterator<Item = &'static str>) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for name in names {
        *counts.entry(name).or_default() += 1;
    }
    counts
}

#[test]
fn every_op_case_applies() {
    for shape in ops::op_shapes() {
        let (_, _, _, deferred) = ops::execute(shape.state.clone(), &shape.config, &shape.bytes)
            .unwrap_or_else(|error| panic!("{} {:?}: {error}", shape.name, shape.dims));
        assert_eq!(
            ops::Deferred::of(&deferred),
            shape.deferred,
            "{} {:?}",
            shape.name,
            shape.dims
        );
    }
}

#[test]
fn the_op_cases_cover_every_series() {
    let counts = series(ops::op_cases().into_iter().map(|case| case.name));
    let expected = BTreeMap::from([
        ("op/claim_pow", 1),
        ("op/config", 4),
        ("op/deposit", 11),
        ("op/inscribe", 13),
        ("op/inscribe_program", 24),
        ("op/program_register", 1),
        ("op/sdp_active", 1),
        ("op/sdp_declare", 4),
        ("op/sdp_withdraw", 1),
        ("op/transfer", 11),
        ("op/withdraw", 11),
    ]);
    assert_eq!(counts, expected);
}

#[test]
fn more_inputs_cost_more_cycles() {
    let mut counter = Counter::new().expect("counters must be available and the process pinned");
    let mut cases = ops::op_cases();
    let one = cycles_of(
        &mut cases,
        "op/transfer",
        &[("inputs", 1), ("outputs", 1)],
        &mut counter,
    );
    let four = cycles_of(
        &mut cases,
        "op/transfer",
        &[("inputs", 4), ("outputs", 1)],
        &mut counter,
    );
    assert!(four > one, "inputs=4: {four}, inputs=1: {one}");
}

#[test]
fn a_larger_payload_costs_more_cycles() {
    let mut counter = Counter::new().expect("counters must be available and the process pinned");
    let mut cases = ops::op_cases();
    let dims = |payload| {
        [
            ("user_steps", 1),
            ("outputs", 1),
            ("buffered", 0),
            ("payload", payload),
        ]
    };
    let empty = cycles_of(&mut cases, "op/inscribe", &dims(0), &mut counter);
    let large = cycles_of(&mut cases, "op/inscribe", &dims(16_384), &mut counter);
    assert!(large > empty, "payload=16384: {large}, payload=0: {empty}");
}

#[test]
fn every_block_case_applies() {
    for shape in block::block_shapes() {
        let (_, tx_events, block_events, _) =
            block::execute(shape.state.clone(), &shape.config, &shape.txs)
                .unwrap_or_else(|error| panic!("{} {:?}: {error}", shape.name, shape.dims));
        assert!(
            matches!(
                block_events.first(),
                Some(BlockEvent::LeaderRewardPaid { .. })
            ),
            "{} {:?}: {block_events:?} {tx_events:?}",
            shape.name,
            shape.dims
        );
    }
}

#[test]
fn the_block_cases_cover_every_series() {
    let counts = series(block::block_cases().into_iter().map(|case| case.name));
    let expected = BTreeMap::from([
        ("block/fixed", 1),
        ("block/header", 5),
        ("block/header_sdp_release", 4),
        ("block/header_withdrawal", 11),
        ("block/mixed", 1),
        ("block/mixed_no_verify", 1),
        ("block/note_sets", 5),
    ]);
    assert_eq!(counts, expected);
}

#[test]
fn every_header_case_applies_and_releases_its_withdrawals() {
    for shape in block::header_shapes() {
        let label = format!("{} {:?}", shape.name, shape.dims);
        let (state, events) = block::apply_header(
            shape.state.clone(),
            &shape.config,
            shape.slot,
            &block::leadership(),
        )
        .unwrap_or_else(|error| panic!("{label}: {error}"));
        let mut released = Vec::new();
        for event in &events {
            match event {
                HeaderEvent::ChannelWithdrawalReleased {
                    note_set,
                    inputs,
                    outputs,
                } => {
                    let set = state
                        .latest_note_sets()
                        .get(*note_set)
                        .expect("the set exists");
                    assert!(
                        inputs.iter().all(|input| set.contains_nullifier(input)),
                        "{label}: an input is unspent"
                    );
                    released.extend(outputs.iter().copied());
                }
                HeaderEvent::SdpWithdrawalReleased { outputs, .. } => {
                    released.extend(outputs.iter().copied());
                }
                HeaderEvent::SdpRewardDistributed { .. } => {}
            }
        }
        assert_eq!(released, shape.released, "{label}");
        let outputs = dim(&shape.dims, "outputs")
            .or_else(|| dim(&shape.dims, "withdrawals"))
            .expect("the released outputs are a dimension");
        assert_eq!(shape.released.len() as u64, outputs, "{label}");
        let ledger_set = |state: &logos_blockchain_ledger::LedgerState| {
            state
                .latest_note_sets()
                .get(LEDGER_SET)
                .expect("the ledger set exists")
                .clone()
        };
        let expected = ledger_set(&shape.state)
            .execute_adding(&shape.released)
            .expect("the commitment MMR has room")
            .commitment_root();
        assert_eq!(
            ledger_set(&state).commitment_root(),
            expected,
            "{label}: the released outputs are not in the ledger set"
        );
    }
}

#[test]
fn releasing_more_withdrawals_costs_more_cycles() {
    let mut counter = Counter::new().expect("counters must be available and the process pinned");
    let mut cases = block::block_cases();
    for (name, more) in [("block/header", 8), ("block/header_sdp_release", 4)] {
        let released = cycles_of(&mut cases, name, &[("withdrawals", more)], &mut counter);
        let none = cycles_of(&mut cases, name, &[("withdrawals", 0)], &mut counter);
        assert!(
            released > none,
            "{name} withdrawals={more}: {released}, withdrawals=0: {none}"
        );
    }
}

fn mixed_shape(name: &str) -> block::BlockShape {
    block::block_shapes()
        .into_iter()
        .find(|shape| shape.name == name)
        .unwrap_or_else(|| panic!("no block {name}"))
}

#[test]
fn the_mixed_block_verifies_each_of_its_deferred_proofs() {
    let shape = mixed_shape("block/mixed");
    let bare = mixed_shape("block/mixed_no_verify");
    assert!(shape.verify && !bare.verify);
    assert_eq!((&shape.dims, &shape.txs), (&bare.dims, &bare.txs));
    let (.., deferred) =
        block::execute(shape.state.clone(), &shape.config, &shape.txs).expect("the block applies");
    let program_steps = dim(&shape.dims, "program_steps").expect("the program steps");
    // One ZkTransfer batch, one ZkProgramSpend batch and each program proof.
    assert_eq!(block::verify_each(deferred).len() as u64, 2 + program_steps);
}

#[test]
fn the_mixed_block_measures_the_verification_of_its_proofs() {
    let mut counter = Counter::new().expect("counters must be available and the process pinned");
    let shape = mixed_shape("block/mixed");
    let mut cases = block::block_cases();
    let whole = cycles_of(&mut cases, "block/mixed", &shape.dims, &mut counter);
    let bare = cycles_of(
        &mut cases,
        "block/mixed_no_verify",
        &shape.dims,
        &mut counter,
    );
    let verification = std::iter::repeat_with(|| {
        counter.measure(
            || {
                block::execute(shape.state.clone(), &shape.config, &shape.txs)
                    .expect("the block applies")
                    .3
            },
            block::verify_each,
        )
    })
    .take(5)
    .min()
    .expect("five runs");
    assert!(
        whole.saturating_sub(bare) > verification / 5 * 4,
        "whole: {whole}, without verification: {bare}, verification: {verification}"
    );
}

#[test]
fn each_sdp_declare_case_holds_its_declarations() {
    let shapes: Vec<ops::OpShape> = ops::op_shapes()
        .into_iter()
        .filter(|shape| shape.name == "op/sdp_declare")
        .collect();
    assert_eq!(shapes.len(), 4);
    for shape in shapes {
        let held = shape
            .state
            .mantle_ledger()
            .sdp_ledger()
            .get_declarations_by_service(ServiceType::BlendNetwork)
            .map_or(0, rpds::RedBlackTreeMap::size);
        assert_eq!(
            Some(held as u64),
            dim(&shape.dims, "declarations"),
            "{:?}",
            shape.dims
        );
    }
}

#[test]
fn each_changed_note_set_is_recorded_at_the_end_of_the_block() {
    for shape in block::block_shapes() {
        let (_, _, block_events, _) =
            block::execute(shape.state.clone(), &shape.config, &shape.txs)
                .expect("the block applies");
        let updated = block_events
            .iter()
            .filter(|event| matches!(event, BlockEvent::NoteSetUpdated(_)))
            .count() as u64;
        let channel_sets = shape
            .dims
            .iter()
            .find_map(|(name, value)| (*name == "changed").then_some(*value))
            .unwrap_or(0);
        // The leader reward note changes the ledger set in every block.
        if !shape.name.starts_with("block/mixed") {
            assert_eq!(updated, channel_sets + 1, "{} {:?}", shape.name, shape.dims);
        }
    }
}

#[test]
fn the_mixed_block_holds_64_transactions_of_its_counted_shape() {
    let shape = block::block_shapes()
        .into_iter()
        .find(|shape| shape.name == "block/mixed")
        .expect("the mixed block");
    assert_eq!(shape.txs.len(), 64);
    let bytes: usize = shape.txs.iter().map(Vec::len).sum();
    let dim = |name| dim(&shape.dims, name).unwrap_or_else(|| panic!("no dimension {name}"));
    assert_eq!(dim("bytes"), bytes as u64);
    let (_, _, _, deferred) =
        block::execute(shape.state.clone(), &shape.config, &shape.txs).expect("the block applies");
    let deferred = ops::Deferred::of(&deferred);
    assert_eq!(deferred.program_spends as u64, dim("program_steps"));
    assert_eq!(
        deferred.zk_transfers as u64,
        dim("transfers") + dim("user_steps") + dim("payments") + dim("deposits")
    );
}

#[test]
fn no_two_cases_share_a_name_and_dimensions() {
    let mut seen = std::collections::BTreeSet::new();
    for case in ops::op_cases().into_iter().chain(block::block_cases()) {
        assert!(
            seen.insert((case.name, case.dims.clone())),
            "{} {:?}",
            case.name,
            case.dims
        );
    }
}
