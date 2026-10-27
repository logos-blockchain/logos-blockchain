//! Gas benchmarks: the core cycles each part of the validation and execution
//! of a Mantle Transaction costs, from which the Execution Gas is derived.
//!
//! Run, pinned to one performance core with its sibling idle, with
//!
//! ```text
//! taskset -c 8 cargo bench -p logos-blockchain-ledger --bench gas [-- <prefix>]
//! ```
//!
//! Counting the cycles needs `kernel.perf_event_paranoid` at 1 or below.
//!
//! Each line of the output is `case,dimensions,cycles`, the minimum over
//! [`ROUNDS`] rounds, each round running every case once in a new random
//! order.

mod cycles;

use cycles::Counter;
use rand::seq::SliceRandom as _;

const ROUNDS: usize = 15;

/// One measured configuration: `run` returns the cycles of one execution.
pub struct Case {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub run: Box<dyn FnMut(&mut Counter) -> u64>,
}

fn run_cases(mut cases: Vec<Case>, counter: &mut Counter, rounds: usize) {
    let mut best = vec![u64::MAX; cases.len()];
    let mut order: Vec<usize> = (0..cases.len()).collect();
    let mut rng = rand::thread_rng();
    for _ in 0..rounds {
        order.shuffle(&mut rng);
        for &index in &order {
            best[index] = best[index].min((cases[index].run)(counter));
        }
    }
    for (case, cycles) in cases.iter().zip(best) {
        let dims: Vec<String> = case
            .dims
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        println!("{},{},{cycles}", case.name, dims.join(";"));
    }
}

fn main() {
    let prefix = std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with('-'))
        .unwrap_or_default();
    let mut counter = Counter::new().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    let cases: Vec<Case> = Vec::new()
        .into_iter()
        .filter(|case: &Case| case.name.starts_with(&prefix))
        .collect();
    run_cases(cases, &mut counter, ROUNDS);
}
