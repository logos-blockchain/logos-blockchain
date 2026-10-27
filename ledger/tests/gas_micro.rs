//! Tests of the `micro` cases of the gas benchmark, which lives in
//! `benches/gas`.
//!
//! Run with
//!
//! ```text
//! GAS_BENCH_FILL=1000 taskset -c 8 cargo test -p logos-blockchain-ledger --release --features test-utils --test gas_micro -- --test-threads=1
//! ```

#[path = "../benches/gas/cycles.rs"]
mod cycles;
#[path = "../benches/gas/fixture.rs"]
mod fixture;
#[path = "../benches/gas/micro.rs"]
mod micro;

use cycles::{Case, Counter};
use lb_core::mantle::note_set::{LEDGER_SET, NoteSet, RECENT_ROOTS_WINDOW};
use lb_groth16::Fr;

fn counter() -> Counter {
    Counter::new().expect("counters must be available and the process pinned")
}

/// The cycles of one run of the case of `name` and `dims`, the minimum of a
/// few runs so a context switch does not decide a comparison.
fn cycles_of(cases: &mut [Case], name: &str, dims: &[(&str, u64)]) -> u64 {
    let case = cases
        .iter_mut()
        .find(|case| case.name == name && case.dims == dims)
        .unwrap_or_else(|| panic!("no case {name} {dims:?}"));
    let mut counter = counter();
    std::iter::repeat_with(|| (case.run)(&mut counter))
        .take(3)
        .min()
        .expect("three runs")
}

fn assert_costlier(cases: &mut [Case], more: (&str, &[(&str, u64)]), less: (&str, &[(&str, u64)])) {
    let more_cycles = cycles_of(cases, more.0, more.1);
    let less_cycles = cycles_of(cases, less.0, less.1);
    assert!(
        more_cycles > less_cycles,
        "{more:?} costs {more_cycles} cycles, {less:?} costs {less_cycles}"
    );
}

#[test]
fn micro_cases_run() {
    let mut cases = micro::micro_cases();
    assert!(!cases.is_empty());
    let mut counter = counter();
    for case in &mut cases {
        let cycles = (case.run)(&mut counter);
        assert!(cycles > 0, "{} {:?} counts no cycle", case.name, case.dims);
    }
}

#[test]
fn case_names_are_grouped_and_unique() {
    let cases = micro::micro_cases();
    let mut seen = std::collections::HashSet::new();
    for case in &cases {
        assert!(case.name.starts_with("micro/"), "{}", case.name);
        assert!(
            seen.insert((case.name, case.dims.clone())),
            "{} {:?} twice",
            case.name,
            case.dims
        );
    }
}

#[test]
fn poseidon2_digest_of_four_costs_more_than_a_compression() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/poseidon2_digest", &[("elements", 4)]),
        ("micro/poseidon2_compress", &[]),
    );
}

#[test]
fn imt_insert_costs_more_than_imt_contains() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/imt_insert", &[]),
        ("micro/imt_contains", &[]),
    );
}

#[test]
fn mmr_push_costs_more_with_more_merges_and_the_root_hashes_every_height() {
    let mut cases = micro::micro_cases();
    assert_costlier(
        &mut cases,
        ("micro/mmr_push", &[("merges", 12)]),
        ("micro/mmr_push", &[("merges", 0)]),
    );
    assert_costlier(
        &mut cases,
        ("micro/mmr_frontier_root", &[]),
        ("micro/mmr_push", &[("merges", 12)]),
    );
}

#[test]
fn step_root_costs_more_with_more_buffered_commitments() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/step_root", &[("buffered", 32)]),
        ("micro/step_root", &[("buffered", 0)]),
    );
}

#[test]
fn end_block_of_one_set_costs_more_than_its_eligible_leaf() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/end_block_one_set", &[("recent_roots", 1)]),
        ("micro/eligible_leaf", &[]),
    );
}

#[test]
fn end_block_costs_more_with_more_deferred_nullifiers() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/end_block_nullifiers", &[("nullifiers", 256)]),
        ("micro/end_block_nullifiers", &[("nullifiers", 1)]),
    );
}

#[test]
fn the_ledger_set_holds_its_recent_roots() {
    let fresh = fixture::nullifiers(1, 0x7e57);
    let expired = NoteSet::empty().commitment_root();
    for recent_roots in micro::RECENT_ROOTS {
        let (sets, roots, block) = micro::recent_roots_sets(recent_roots);
        let set = sets.get(LEDGER_SET).expect("the ledger set exists");
        assert_eq!(roots.len() as u64, recent_roots);
        assert!(!roots.contains(&set.commitment_root()));
        for root in &roots {
            assert!(
                set.assert_spendable(&fresh, root, block).is_ok(),
                "recent_roots={recent_roots}: {root} is not recent at block {block}"
            );
        }
        // The roots older than the fixture block have expired.
        assert!(set.assert_spendable(&fresh, &expired, block).is_err());
    }
    // The window is full: the oldest root expires at the next block.
    let (sets, roots, block) = micro::recent_roots_sets(RECENT_ROOTS_WINDOW);
    let set = sets.get(LEDGER_SET).expect("the ledger set exists");
    assert!(set.assert_spendable(&fresh, &roots[0], block + 1).is_err());
}

#[test]
fn blake2b_costs_more_on_more_bytes() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/blake2b", &[("bytes", 65536)]),
        ("micro/blake2b", &[("bytes", 64)]),
    );
}

#[test]
fn decode_tx_costs_more_on_a_longer_inscription() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/decode_tx", &[("bytes", 65536)]),
        ("micro/decode_tx", &[("bytes", 0)]),
    );
}

#[test]
fn g2_decompression_costs_more_than_g1_and_than_its_subgroup_check() {
    let mut cases = micro::micro_cases();
    assert_costlier(
        &mut cases,
        ("micro/g2_decompress", &[]),
        ("micro/g1_decompress", &[]),
    );
    assert_costlier(
        &mut cases,
        ("micro/g2_decompress", &[]),
        ("micro/g2_subgroup", &[]),
    );
}

#[test]
fn ed25519_verify_costs_more_than_hashing_its_message() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/ed25519_verify", &[]),
        ("micro/blake2b", &[("bytes", 64)]),
    );
}

#[test]
fn groth16_batches_cost_more_with_more_proofs() {
    let mut cases = micro::micro_cases();
    for inputs in [3, 27, 37] {
        assert_costlier(
            &mut cases,
            ("micro/groth16_batch", &[("N", inputs), ("n", 8)]),
            ("micro/groth16_batch", &[("N", inputs), ("n", 1)]),
        );
    }
}

#[test]
fn program_spend_batch_inputs_are_full_size() {
    let small = Fr::from(u64::MAX);
    for spend in micro::program_spends(2) {
        let inputs = spend.inputs().to_inputs();
        assert_eq!(inputs.len(), 37);
        assert!(inputs.iter().all(|input| *input > small), "{inputs:?}");
    }
}

#[test]
fn program_proof_verification_costs_more_than_its_key_lines() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/program_proof_verify", &[]),
        ("micro/key_lines", &[]),
    );
}

#[test]
fn key_preparation_costs_more_than_each_of_its_parts() {
    let mut cases = micro::micro_cases();
    assert_costlier(
        &mut cases,
        ("micro/key_prepare", &[]),
        ("micro/key_e_alpha_beta", &[]),
    );
    assert_costlier(
        &mut cases,
        ("micro/key_prepare", &[]),
        ("micro/key_lines", &[]),
    );
    assert_costlier(
        &mut cases,
        ("micro/program_key_register", &[]),
        ("micro/key_decompress", &[]),
    );
}

#[test]
fn pol_verify_costs_more_than_one_pairing() {
    assert_costlier(
        &mut micro::micro_cases(),
        ("micro/pol_verify", &[]),
        ("micro/key_e_alpha_beta", &[]),
    );
}
