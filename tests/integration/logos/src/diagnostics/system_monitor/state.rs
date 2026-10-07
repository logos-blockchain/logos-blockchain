use std::{collections::VecDeque, path::PathBuf};

use super::{
    records::{SystemEvent, SystemSample},
    sink::SystemStatsLog,
};

const RECENT_SAMPLE_LIMIT: usize = 12;
const RECENT_EVENT_LIMIT: usize = 24;
const RECENT_EVENT_SUMMARY_LIMIT: usize = 6;

/// Registered output files that receive the NDJSON monitor stream.
#[derive(Default)]
pub(super) struct OutputRegistry {
    paths: std::collections::BTreeSet<PathBuf>,
}

impl OutputRegistry {
    pub(super) fn register(&mut self, path: &std::path::Path) -> bool {
        if !self.paths.insert(path.to_path_buf()) {
            return false;
        }

        SystemStatsLog::reset_output(path);
        true
    }

    pub(super) fn unregister(&mut self, path: &std::path::Path) -> bool {
        self.paths.remove(path)
    }

    pub(super) fn len(&self) -> usize {
        self.paths.len()
    }

    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.paths.iter().cloned().collect()
    }
}

/// Fixed-size rolling window used for recent sample lookups.
#[derive(Default)]
pub(super) struct SampleHistory {
    samples: VecDeque<SystemSample>,
}

impl SampleHistory {
    pub(super) fn record(&mut self, sample: SystemSample) {
        if self.samples.len() == RECENT_SAMPLE_LIMIT {
            self.samples.pop_front();
        }

        self.samples.push_back(sample);
    }

    pub(super) fn latest(&self) -> Option<SystemSample> {
        self.samples.back().cloned()
    }

    pub(super) fn window(&self) -> SampleWindow {
        SampleWindow {
            samples: self.samples.clone(),
        }
    }
}

/// Recent lifecycle markers retained alongside host samples.
#[derive(Default)]
pub(super) struct EventHistory {
    events: VecDeque<SystemEvent>,
}

impl EventHistory {
    pub(super) fn record(&mut self, event: SystemEvent) {
        if self.events.len() == RECENT_EVENT_LIMIT {
            self.events.pop_front();
        }

        self.events.push_back(event);
    }

    pub(super) fn window(&self) -> EventWindow {
        EventWindow {
            events: self.events.clone(),
        }
    }
}

/// Immutable view of the current monitor state used for reporting.
pub(super) struct MonitorSnapshot {
    pub(super) output_count: usize,
    pub(super) samples: SampleWindow,
    pub(super) events: EventWindow,
}

/// Query helpers over the retained recent sample window.
pub(super) struct SampleWindow {
    samples: VecDeque<SystemSample>,
}

impl SampleWindow {
    pub(super) fn latest(&self) -> Option<&SystemSample> {
        self.samples.back()
    }

    pub(super) fn len(&self) -> usize {
        self.samples.len()
    }

    pub(super) fn norm_load_history(&self) -> Option<String> {
        if self.samples.len() <= 1 {
            return None;
        }

        Some(
            self.samples
                .iter()
                .map(super::summary::format_sample_norm_load1)
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    pub(super) fn cpu_history(&self) -> Option<String> {
        if self.samples.len() <= 1 {
            return None;
        }

        Some(
            self.samples
                .iter()
                .map(super::summary::format_sample_cpu_used_pct)
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

/// Query helpers over the retained recent event window.
pub(super) struct EventWindow {
    events: VecDeque<SystemEvent>,
}

impl EventWindow {
    pub(super) fn recent_summary(&self) -> Option<String> {
        let events = self
            .events
            .iter()
            .rev()
            .take(RECENT_EVENT_SUMMARY_LIMIT)
            .collect::<Vec<_>>();

        if events.is_empty() {
            return None;
        }

        Some(
            events
                .into_iter()
                .rev()
                .map(super::summary::render_event_summary_item)
                .collect::<Vec<_>>()
                .join(" | "),
        )
    }
}
