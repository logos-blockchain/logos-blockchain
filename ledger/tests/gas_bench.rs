//! Tests of the gas benchmark harness, which lives in `benches/gas`.
//!
//! Run with
//!
//! ```text
//! taskset -c 8 cargo test -p logos-blockchain-ledger --release --test gas_bench -- --test-threads=1
//! ```

#[path = "../benches/gas/cycles.rs"]
mod cycles;
#[expect(
    dead_code,
    reason = "the cases of the bench use the rest of the fixture"
)]
#[path = "../benches/gas/fixture.rs"]
mod fixture;

use std::hint::black_box;

use cycles::{Counter, CounterError, pinned_cpu};

fn spin(iterations: u64) -> u64 {
    let mut acc = 0u64;
    for i in 0..iterations {
        acc = black_box(acc.wrapping_mul(31).wrapping_add(i));
    }
    acc
}

#[test]
fn busy_loop_scales_linearly() {
    let mut counter = Counter::new().expect("counters must be available and the process pinned");
    let single = counter.measure(|| (), |()| spin(1_000_000));
    let double = counter.measure(|| (), |()| spin(2_000_000));
    let ratio = double as f64 / single as f64;
    assert!((1.9..=2.1).contains(&ratio), "ratio {ratio}");
}

#[test]
fn setup_is_not_counted() {
    let mut counter = Counter::new().expect("counters must be available and the process pinned");
    let cycles = counter.measure(|| spin(50_000_000), |_| ());
    assert!(cycles < 100_000, "cycles {cycles}");
}

#[test]
fn a_single_cpu_is_pinned() {
    assert_eq!(pinned_cpu("8"), Some(8));
    assert_eq!(pinned_cpu("8\n"), Some(8));
}

#[test]
fn several_cpus_are_not_pinned() {
    assert_eq!(pinned_cpu("8-9"), None);
    assert_eq!(pinned_cpu("0-31"), None);
    assert_eq!(pinned_cpu("2,8"), None);
}

#[test]
fn rejects_unpinned_process() {
    assert!(matches!(
        Counter::with_allowed_cpus("0-31"),
        Err(CounterError::NotPinned(_))
    ));
}

mod filled_ledger {
    use lb_core::mantle::note_set::LEDGER_SET;

    use crate::fixture::{fill, filled_state, nullifiers};

    #[test]
    fn note_sets_are_filled() {
        let fixture = filled_state();
        for index in [LEDGER_SET, fixture.channel_set] {
            let set = fixture.state.latest_note_sets().get(index).unwrap();
            assert_eq!(set.nullifiers().len(), fill() + 1, "set {index}");
            assert!(set.commitments().len() >= fill(), "set {index}");
        }
    }

    #[test]
    fn fresh_nullifiers_are_spendable_against_the_current_root() {
        let fixture = filled_state();
        let set = fixture.state.latest_note_sets().get(LEDGER_SET).unwrap();
        assert!(
            set.assert_spendable(&nullifiers(4, 0), &set.commitment_root(), 1)
                .is_ok()
        );
    }

    #[test]
    fn the_channel_and_the_program_exist() {
        let fixture = filled_state();
        let mantle = fixture.state.mantle_ledger();
        assert!(mantle.channels().channel_state(&fixture.channel).is_some());
        assert!(mantle.programs().get(&fixture.program).is_some());
    }
}

#[test]
fn the_fill_defaults_to_a_million_notes() {
    assert_eq!(fixture::DEFAULT_FILL, 1_000_000);
    assert_eq!(fixture::fill_from(None), 1_000_000);
}

#[test]
fn the_fill_follows_the_environment() {
    assert_eq!(fixture::fill_from(Some("1000")), 1000);
}
