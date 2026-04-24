use std::{ffi::OsStr, path::Path};

use serde::Serialize;
use sysinfo::Process;
use time::OffsetDateTime;

const TOP_PROCESS_LIMIT: usize = 3;
const EVENT_DETAIL_LIMIT: usize = 160;

/// Persisted monitor stream entry.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub(super) enum SystemMonitorRecord {
    Sample(Box<SystemSample>),
    Event(SystemEvent),
}

impl SystemMonitorRecord {
    pub(super) fn as_sample(&self) -> Option<&SystemSample> {
        match self {
            Self::Sample(sample) => Some(sample),
            Self::Event(_) => None,
        }
    }
}

/// One point-in-time host sample captured from `sysinfo`.
#[derive(Clone, Debug, Serialize)]
pub(super) struct SystemSample {
    pub(super) ts_unix: i64,
    pub(super) os: &'static str,
    pub(super) host: HostSnapshot,
    pub(super) disk: Option<DiskSnapshot>,
    pub(super) node_processes: NodeProcessSummary,
    pub(super) top_cpu_processes: Vec<ProcessSnapshot>,
    pub(super) top_rss_processes: Vec<ProcessSnapshot>,
}

/// Host-wide resource usage at one point in time.
#[derive(Clone, Debug, Serialize)]
pub(super) struct HostSnapshot {
    pub(super) logical_cpus: usize,
    pub(super) process_count: usize,
    pub(super) thread_count: Option<usize>,
    pub(super) cpu_usage_percent: f32,
    pub(super) load1: Option<f64>,
    pub(super) load5: Option<f64>,
    pub(super) load15: Option<f64>,
    pub(super) norm_load1: Option<f64>,
    pub(super) memory_used_bytes: Option<u64>,
    pub(super) memory_total_bytes: Option<u64>,
    pub(super) swap_used_bytes: Option<u64>,
}

/// Disk capacity view for the workspace or test artifact mount.
#[derive(Clone, Debug, Serialize)]
pub(super) struct DiskSnapshot {
    pub(super) mount_point: String,
    pub(super) available_bytes: u64,
    pub(super) total_bytes: u64,
}

/// Summary of currently running `logos-blockchain-node` processes.
#[derive(Clone, Debug, Serialize)]
pub(super) struct NodeProcessSummary {
    pub(super) count: usize,
    pub(super) total_rss_bytes: u64,
}

/// Compact view of one busy process.
#[derive(Clone, Debug, Serialize)]
pub(super) struct ProcessSnapshot {
    pub(super) pid: String,
    pub(super) name: String,
    pub(super) cpu_usage_percent: Option<f32>,
    pub(super) rss_bytes: Option<u64>,
}

impl ProcessSnapshot {
    pub(super) fn from_process(process: &Process) -> Self {
        Self {
            pid: process.pid().to_string(),
            name: process_display_name(process),
            cpu_usage_percent: finite_f32(process.cpu_usage()),
            rss_bytes: Some(process.memory()),
        }
    }
}

/// Flat CSV projection of one system sample for spreadsheet-friendly analysis.
pub(super) struct CsvSampleRow {
    ts_unix: i64,
    os: String,
    logical_cpus: usize,
    process_count: usize,
    thread_count: Option<usize>,
    cpu_usage_percent: f32,
    load1: Option<f64>,
    load5: Option<f64>,
    load15: Option<f64>,
    norm_load1: Option<f64>,
    memory_used_bytes: Option<u64>,
    memory_total_bytes: Option<u64>,
    swap_used_bytes: Option<u64>,
    disk_mount_point: Option<String>,
    disk_available_bytes: Option<u64>,
    disk_total_bytes: Option<u64>,
    node_process_count: usize,
    node_process_rss_bytes: u64,
    top_cpu_processes: String,
    top_rss_processes: String,
}

impl CsvSampleRow {
    pub(super) const HEADER: &[&str] = &[
        "ts_unix",
        "os",
        "logical_cpus",
        "process_count",
        "thread_count",
        "cpu_usage_percent",
        "load1",
        "load5",
        "load15",
        "norm_load1",
        "memory_used_bytes",
        "memory_total_bytes",
        "swap_used_bytes",
        "disk_mount_point",
        "disk_available_bytes",
        "disk_total_bytes",
        "node_process_count",
        "node_process_rss_bytes",
        "top_cpu_processes",
        "top_rss_processes",
    ];

    pub(super) fn from_sample(sample: &SystemSample) -> Self {
        let disk = sample.disk.as_ref();

        Self {
            ts_unix: sample.ts_unix,
            os: sample.os.to_owned(),
            logical_cpus: sample.host.logical_cpus,
            process_count: sample.host.process_count,
            thread_count: sample.host.thread_count,
            cpu_usage_percent: sample.host.cpu_usage_percent,
            load1: sample.host.load1,
            load5: sample.host.load5,
            load15: sample.host.load15,
            norm_load1: sample.host.norm_load1,
            memory_used_bytes: sample.host.memory_used_bytes,
            memory_total_bytes: sample.host.memory_total_bytes,
            swap_used_bytes: sample.host.swap_used_bytes,
            disk_mount_point: disk.map(|value| value.mount_point.clone()),
            disk_available_bytes: disk.map(|value| value.available_bytes),
            disk_total_bytes: disk.map(|value| value.total_bytes),
            node_process_count: sample.node_processes.count,
            node_process_rss_bytes: sample.node_processes.total_rss_bytes,
            top_cpu_processes: render_process_list(&sample.top_cpu_processes),
            top_rss_processes: render_process_list(&sample.top_rss_processes),
        }
    }

    pub(super) fn values(&self) -> Vec<String> {
        vec![
            self.ts_unix.to_string(),
            self.os.clone(),
            self.logical_cpus.to_string(),
            self.process_count.to_string(),
            format_opt_usize(self.thread_count),
            self.cpu_usage_percent.to_string(),
            format_opt_f64(self.load1),
            format_opt_f64(self.load5),
            format_opt_f64(self.load15),
            format_opt_f64(self.norm_load1),
            format_opt_u64(self.memory_used_bytes),
            format_opt_u64(self.memory_total_bytes),
            format_opt_u64(self.swap_used_bytes),
            self.disk_mount_point.clone().unwrap_or_default(),
            format_opt_u64(self.disk_available_bytes),
            format_opt_u64(self.disk_total_bytes),
            self.node_process_count.to_string(),
            self.node_process_rss_bytes.to_string(),
            self.top_cpu_processes.clone(),
            self.top_rss_processes.clone(),
        ]
    }
}

/// One lifecycle marker written alongside samples in the monitor stream.
#[derive(Clone, Debug, Serialize)]
pub(super) struct SystemEvent {
    pub(super) ts_unix: i64,
    pub(super) label: String,
    pub(super) detail: String,
}

impl SystemEvent {
    pub(super) fn new(label: &str, detail: impl Into<String>) -> Self {
        Self {
            ts_unix: OffsetDateTime::now_utc().unix_timestamp(),
            label: label.to_owned(),
            detail: truncate_string(detail.into(), EVENT_DETAIL_LIMIT),
        }
    }

    pub(super) fn output_registered(path: &Path) -> Self {
        Self::new("output_registered", path.display().to_string())
    }
}

pub(super) fn collect_node_process_summary(processes: &[&Process]) -> NodeProcessSummary {
    let mut count = 0usize;
    let mut total_rss_bytes = 0u64;

    for process in processes
        .iter()
        .copied()
        .filter(|process| is_logos_node_process(process))
    {
        count += 1;
        total_rss_bytes += process.memory();
    }

    NodeProcessSummary {
        count,
        total_rss_bytes,
    }
}

pub(super) fn collect_top_cpu_processes(processes: &[&Process]) -> Vec<ProcessSnapshot> {
    top_processes_by(processes, |process| {
        finite_f32(process.cpu_usage()).map_or(0.0, f64::from)
    })
}

pub(super) fn collect_top_rss_processes(processes: &[&Process]) -> Vec<ProcessSnapshot> {
    top_processes_by(processes, |process| process.memory() as f64)
}

pub(super) fn collect_thread_count(processes: &[&Process]) -> Option<usize> {
    let mut total = 0usize;
    let mut observed = false;

    for process in processes {
        let Some(tasks) = process.tasks() else {
            continue;
        };

        observed = true;
        total += tasks.len();
    }

    observed.then_some(total)
}

pub(super) fn finite_f32(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

pub(super) fn finite_f64(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn top_processes_by<F>(processes: &[&Process], metric: F) -> Vec<ProcessSnapshot>
where
    F: Fn(&Process) -> f64,
{
    let mut processes = processes.to_vec();
    processes.sort_by(|left, right| metric(right).total_cmp(&metric(left)));

    processes
        .into_iter()
        .filter(|process| metric(process) > 0.0)
        .take(TOP_PROCESS_LIMIT)
        .map(ProcessSnapshot::from_process)
        .collect()
}

fn is_logos_node_process(process: &Process) -> bool {
    process.exe().is_some_and(|exe| {
        exe.file_name()
            .is_some_and(|name| name == OsStr::new("logos-blockchain-node"))
    }) || process.name() == OsStr::new("logos-blockchain-node")
}

fn process_display_name(process: &Process) -> String {
    process
        .exe()
        .and_then(|exe| exe.file_name())
        .or_else(|| {
            process
                .cmd()
                .first()
                .and_then(|cmd| Path::new(cmd).file_name())
        })
        .unwrap_or_else(|| process.name())
        .to_string_lossy()
        .into_owned()
}

fn truncate_string(mut value: String, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value;
    }

    value = value.chars().take(limit.saturating_sub(1)).collect();
    value.push('\u{2026}');
    value
}

fn render_process_list(processes: &[ProcessSnapshot]) -> String {
    processes
        .iter()
        .map(render_process_summary)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn render_process_summary(process: &ProcessSnapshot) -> String {
    let cpu = process
        .cpu_usage_percent
        .map(|value| format!("{value:.1}"))
        .unwrap_or_default();
    let rss = process
        .rss_bytes
        .map(|value| value.to_string())
        .unwrap_or_default();

    format!("{}#{}#{}#{}", process.name, process.pid, cpu, rss)
}

fn format_opt_f64(value: Option<f64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn format_opt_u64(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn format_opt_usize(value: Option<usize>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}
