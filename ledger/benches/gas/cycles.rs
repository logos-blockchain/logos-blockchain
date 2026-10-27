//! Core cycles of a closure, counted by the hardware performance counters of
//! the CPU the process is pinned to.

use std::{fs, hint::black_box, io};

use perf_event::{Builder, Counter as PerfCounter, ReadFormat, events::Hardware};

#[derive(Debug, thiserror::Error)]
pub enum CounterError {
    #[error("the cycle counter cannot be opened (is kernel.perf_event_paranoid <= 1?): {0}")]
    Unavailable(#[from] io::Error),
    #[error("the process must be pinned to one CPU (taskset -c 8), allowed CPUs: {0}")]
    NotPinned(String),
}

/// The CPU of a `Cpus_allowed_list` that names exactly one.
#[must_use]
pub fn pinned_cpu(allowed: &str) -> Option<usize> {
    allowed.trim().parse().ok()
}

pub struct Counter {
    cycles: PerfCounter,
}

impl Counter {
    pub fn new() -> Result<Self, CounterError> {
        let status = fs::read_to_string("/proc/self/status")?;
        let allowed = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .unwrap_or_default();
        Self::with_allowed_cpus(allowed)
    }

    pub fn with_allowed_cpus(allowed: &str) -> Result<Self, CounterError> {
        if pinned_cpu(allowed).is_none() {
            return Err(CounterError::NotPinned(allowed.trim().to_owned()));
        }
        let cycles = Builder::new(Hardware::CPU_CYCLES)
            .observe_self()
            .include_kernel()
            .read_format(ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING)
            .build()?;
        Ok(Self { cycles })
    }

    /// The cycles spent in `run`, on the input `setup` returns and outside
    /// of it. Panics if the counter was multiplexed, since its count would
    /// then be an estimate.
    pub fn measure<S, R>(
        &mut self,
        mut setup: impl FnMut() -> S,
        mut run: impl FnMut(S) -> R,
    ) -> u64 {
        let input = black_box(setup());
        self.cycles.reset().expect("the counter resets");
        self.cycles.enable().expect("the counter starts");
        let output = run(input);
        self.cycles.disable().expect("the counter stops");
        drop(black_box(output));
        let data = self.cycles.read_full().expect("the counter reads");
        assert_eq!(
            data.time_enabled(),
            data.time_running(),
            "the cycle counter was multiplexed"
        );
        data.count()
    }
}

/// One measured configuration: `run` returns the cycles of one execution.
pub struct Case {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub run: Box<dyn FnMut(&mut Counter) -> u64>,
}
