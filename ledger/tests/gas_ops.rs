//! Tests of the `ops` and `block` cases of the gas benchmark, which live in
//! `benches/gas`.

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
use lb_core::events::BlockEvent;

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
        ("op/sdp_declare", 1),
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
        ("block/mixed", 1),
        ("block/note_sets", 5),
    ]);
    assert_eq!(counts, expected);
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
        if shape.name != "block/mixed" {
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
    let dim = |name| {
        shape
            .dims
            .iter()
            .find_map(|(dim, value)| (*dim == name).then_some(*value))
            .unwrap_or_else(|| panic!("no dimension {name}"))
    };
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
