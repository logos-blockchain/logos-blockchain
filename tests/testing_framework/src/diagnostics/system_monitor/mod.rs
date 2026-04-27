use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::Duration,
};

mod collector;
mod records;
mod sink;
mod state;
mod summary;

use collector::SystemCollector;
use records::{SystemEvent, SystemMonitorRecord, SystemSample};
use state::{EventHistory, MonitorSnapshot, OutputRegistry, SampleHistory};
use summary::SystemSummary;

use crate::env as tf_env;

static SYSTEM_MONITOR: OnceLock<SystemMonitor> = OnceLock::new();

/// Registers one monitor output path.
///
/// The shared monitor writes the full stream to the provided NDJSON path and
/// writes flat sample rows to a sibling CSV file.
pub(super) fn register_output_file(path: &Path) {
    if !tf_env::logos_blockchain_system_monitor_enabled() {
        return;
    }

    system_monitor().register_output(path);
}

/// Stops writing the shared monitor stream to one registered output path.
pub(super) fn unregister_output_file(path: &Path) {
    if !tf_env::logos_blockchain_system_monitor_enabled() {
        return;
    }

    system_monitor().unregister_output(path);
}

/// Records one lifecycle marker in the shared monitor stream.
pub(super) fn record_event(label: &str, detail: impl Into<String>) {
    if !tf_env::logos_blockchain_system_monitor_enabled() {
        return;
    }

    system_monitor().record_event(SystemEvent::new(label, detail));
}

/// Renders a compact summary of the most recent samples for failure reports.
pub(super) fn render_recent_summary() -> Option<String> {
    system_monitor().render_recent_summary()
}

fn system_monitor() -> &'static SystemMonitor {
    SYSTEM_MONITOR.get_or_init(SystemMonitor::new)
}

/// Process-wide orchestrator for sampling and diagnostics rendering.
struct SystemMonitor {
    config: SystemMonitorConfig,
    shared: Arc<SystemMonitorShared>,
}

impl SystemMonitor {
    fn new() -> Self {
        let config = SystemMonitorConfig::from_env();
        let shared = Arc::new(SystemMonitorShared::default());

        SamplerThread::spawn(config, Arc::clone(&shared));

        Self { config, shared }
    }

    fn register_output(&self, path: &Path) {
        if !self.shared.register_output(path) {
            return;
        }

        self.shared
            .publish_event(SystemEvent::output_registered(path));

        if let Some(sample) = self.shared.latest_sample() {
            self.shared.append_sample_to_output(path, sample);
            return;
        }

        self.shared.publish_sample(SystemCollector::new().capture());
    }

    fn unregister_output(&self, path: &Path) {
        if !self.shared.unregister_output(path) {
            return;
        }

        self.shared.publish_event(SystemEvent::new(
            "output_unregistered",
            path.display().to_string(),
        ));
    }

    fn record_event(&self, event: SystemEvent) {
        self.shared.publish_event(event);
    }

    fn render_recent_summary(&self) -> Option<String> {
        let snapshot = self.shared.snapshot();

        SystemSummary::build(self.config.interval_secs, &snapshot).map(SystemSummary::render)
    }
}

#[derive(Clone, Copy)]
/// Environment-derived monitor settings shared by the sampler and reporters.
struct SystemMonitorConfig {
    interval_secs: u64,
}

impl SystemMonitorConfig {
    fn from_env() -> Self {
        Self {
            interval_secs: tf_env::logos_blockchain_system_monitor_interval_secs(),
        }
    }

    const fn sample_interval(self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
}

#[derive(Default)]
/// Shared monitor state accessed by registration, sampling, and reporting.
struct SystemMonitorShared {
    io: Mutex<()>,
    outputs: Mutex<OutputRegistry>,
    samples: Mutex<SampleHistory>,
    events: Mutex<EventHistory>,
}

impl SystemMonitorShared {
    fn register_output(&self, path: &Path) -> bool {
        self.outputs
            .lock()
            .expect("system monitor lock poisoned")
            .register(path)
    }

    fn unregister_output(&self, path: &Path) -> bool {
        self.outputs
            .lock()
            .expect("system monitor lock poisoned")
            .unregister(path)
    }

    fn record_sample(&self, sample: SystemSample) {
        self.samples
            .lock()
            .expect("system monitor lock poisoned")
            .record(sample);
    }

    fn record_event(&self, event: SystemEvent) {
        self.events
            .lock()
            .expect("system monitor lock poisoned")
            .record(event);
    }

    fn append_record(&self, path: &Path, record: &SystemMonitorRecord) {
        let _guard = self.io.lock().expect("system monitor lock poisoned");

        sink::SystemStatsLog::append(path, record);
    }

    fn append_sample_to_output(&self, path: &Path, sample: SystemSample) {
        let record = SystemMonitorRecord::Sample(Box::new(sample));
        self.append_record(path, &record);
    }

    fn publish_sample(&self, sample: SystemSample) {
        let outputs = self.output_paths();
        let record = SystemMonitorRecord::Sample(Box::new(sample.clone()));

        self.record_sample(sample);

        for output in outputs {
            self.append_record(&output, &record);
        }
    }

    fn publish_event(&self, event: SystemEvent) {
        let outputs = self.output_paths();
        let record = SystemMonitorRecord::Event(event.clone());

        self.record_event(event);

        for output in outputs {
            self.append_record(&output, &record);
        }
    }

    fn snapshot(&self) -> MonitorSnapshot {
        let output_count = self
            .outputs
            .lock()
            .expect("system monitor lock poisoned")
            .len();
        let samples = self
            .samples
            .lock()
            .expect("system monitor lock poisoned")
            .window();
        let events = self
            .events
            .lock()
            .expect("system monitor lock poisoned")
            .window();

        MonitorSnapshot {
            output_count,
            samples,
            events,
        }
    }

    fn output_paths(&self) -> Vec<PathBuf> {
        self.outputs
            .lock()
            .expect("system monitor lock poisoned")
            .paths()
    }

    fn latest_sample(&self) -> Option<SystemSample> {
        self.samples
            .lock()
            .expect("system monitor lock poisoned")
            .latest()
    }
}

/// Background worker that periodically captures and publishes system samples.
struct SamplerThread {
    collector: SystemCollector,
    config: SystemMonitorConfig,
    shared: Arc<SystemMonitorShared>,
}

impl SamplerThread {
    fn spawn(config: SystemMonitorConfig, shared: Arc<SystemMonitorShared>) {
        thread::Builder::new()
            .name("logos-test-system-monitor".to_owned())
            .spawn(move || {
                Self {
                    collector: SystemCollector::new(),
                    config,
                    shared,
                }
                .run();
            })
            .expect("system monitor thread should start");
    }

    fn run(mut self) {
        loop {
            self.shared.publish_sample(self.collector.capture());

            thread::sleep(self.config.sample_interval());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::{collector::SystemCollector, register_output_file};

    #[test]
    fn captured_sample_is_json_serializable() {
        let mut collector = SystemCollector::new();
        let sample = collector.capture();

        serde_json::to_string(&sample).expect("captured sample should serialize");
    }

    #[test]
    fn registering_output_file_bootstraps_ndjson_and_csv() {
        let tempdir = tempdir().expect("tempdir should be created");
        let path = tempdir.path().join("system_stats.ndjson");
        let csv_path = tempdir.path().join("system_stats.csv");

        fs::write(&path, "stale-ndjson\n").expect("stale ndjson should be written");
        fs::write(&csv_path, "stale-csv\n").expect("stale csv should be written");

        register_output_file(&path);
        std::thread::sleep(Duration::from_millis(50));

        let contents = fs::read_to_string(&path).expect("monitor log should be created");
        let first_record = contents
            .lines()
            .next()
            .expect("monitor log should contain at least one record");

        assert!(
            serde_json::from_str::<serde_json::Value>(first_record)
                .expect("monitor record should deserialize")
                .get("record_type")
                == Some(&serde_json::Value::String("event".to_owned())),
            "first monitor record should be the registration event"
        );

        let csv = fs::read_to_string(tempdir.path().join("system_stats.csv"))
            .expect("csv log should be created for new outputs");

        assert!(
            csv.starts_with("ts_unix,os,logical_cpus"),
            "csv log should contain the header row"
        );
        assert!(!contents.contains("stale-ndjson"));
        assert!(!csv.contains("stale-csv"));
    }
}
