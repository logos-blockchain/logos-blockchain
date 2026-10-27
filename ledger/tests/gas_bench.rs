//! Tests of the gas benchmark harness, which lives in `benches/gas`.
//!
//! Run with
//!
//! ```text
//! taskset -c 8 cargo test -p logos-blockchain-ledger --release --test gas_bench -- --test-threads=1
//! ```

#[path = "../benches/gas/cycles.rs"]
mod cycles;

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
