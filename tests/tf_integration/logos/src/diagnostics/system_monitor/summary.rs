use super::{
    records::{DiskSnapshot, NodeProcessSummary, ProcessSnapshot, SystemEvent, SystemSample},
    state::MonitorSnapshot,
};

/// Renderable diagnostics view derived from the latest monitor state.
pub(super) struct SystemSummary {
    output_count: usize,
    sample_count: usize,
    interval_secs: u64,
    latest: SystemSample,
    recent_norm_load1: Option<String>,
    recent_cpu_used_pct: Option<String>,
    recent_events: Option<String>,
}

impl SystemSummary {
    pub(super) fn build(interval_secs: u64, snapshot: &MonitorSnapshot) -> Option<Self> {
        let latest = snapshot.samples.latest()?.clone();

        Some(Self {
            output_count: snapshot.output_count,
            sample_count: snapshot.samples.len(),
            interval_secs,
            latest,
            recent_norm_load1: snapshot.samples.norm_load_history(),
            recent_cpu_used_pct: snapshot.samples.cpu_history(),
            recent_events: snapshot.events.recent_summary(),
        })
    }

    pub(super) fn render(self) -> String {
        let mut lines = vec![
            "system_monitor:".to_owned(),
            format!(
                "  enabled=true outputs={} samples={} interval_secs={}",
                self.output_count, self.sample_count, self.interval_secs
            ),
            render_host_summary_line(&self.latest),
        ];

        if let Some(line) = self.latest.disk.as_ref().map(render_disk_summary_line) {
            lines.push(line);
        }

        lines.push(render_node_process_summary_line(
            &self.latest.node_processes,
        ));

        if let Some(line) = render_cpu_process_summary_line(&self.latest.top_cpu_processes) {
            lines.push(line);
        }

        if let Some(line) = render_rss_process_summary_line(&self.latest.top_rss_processes) {
            lines.push(line);
        }

        if let Some(history) = self.recent_norm_load1 {
            lines.push(format!("  recent norm_load1=[{history}]"));
        }

        if let Some(history) = self.recent_cpu_used_pct {
            lines.push(format!("  recent cpu_used_pct=[{history}]"));
        }

        if let Some(events) = self.recent_events {
            lines.push(format!("  recent events=[{events}]"));
        }

        lines.join("\n")
    }
}

pub(super) fn render_event_summary_item(event: &SystemEvent) -> String {
    format!("{}:{}({})", event.ts_unix, event.label, event.detail)
}

pub(super) fn render_host_summary_line(sample: &SystemSample) -> String {
    format!(
        "  latest ts_unix={} os={} cpus={} procs={} threads={} cpu_used_pct={} load1={} norm_load1={} mem={} swap={}",
        sample.ts_unix,
        sample.os,
        sample.host.logical_cpus,
        sample.host.process_count,
        format_opt_usize(sample.host.thread_count),
        format_sample_cpu_used_pct(sample),
        format_opt_f64(sample.host.load1),
        format_sample_norm_load1(sample),
        format_bytes_pair(
            sample.host.memory_used_bytes,
            sample.host.memory_total_bytes
        ),
        format_opt_bytes(sample.host.swap_used_bytes),
    )
}

pub(super) fn render_disk_summary_line(disk: &DiskSnapshot) -> String {
    format!(
        "  disk mount={} free={}",
        disk.mount_point,
        format_bytes_pair(Some(disk.available_bytes), Some(disk.total_bytes)),
    )
}

pub(super) fn render_node_process_summary_line(summary: &NodeProcessSummary) -> String {
    format!(
        "  logos_nodes count={} rss={}",
        summary.count,
        format_bytes(summary.total_rss_bytes),
    )
}

fn render_cpu_process_summary_item(process: &ProcessSnapshot) -> String {
    format!(
        "{}(pid={})={}",
        process.name,
        process.pid,
        process
            .cpu_usage_percent
            .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.1}%"))
    )
}

fn render_rss_process_summary_item(process: &ProcessSnapshot) -> String {
    format!(
        "{}(pid={})={}",
        process.name,
        process.pid,
        format_opt_bytes(process.rss_bytes)
    )
}

pub(super) fn render_cpu_process_summary_line(processes: &[ProcessSnapshot]) -> Option<String> {
    if processes.is_empty() {
        return None;
    }

    let rendered = processes
        .iter()
        .map(render_cpu_process_summary_item)
        .collect::<Vec<_>>()
        .join(", ");

    Some(format!("  top cpu=[{rendered}]"))
}

pub(super) fn render_rss_process_summary_line(processes: &[ProcessSnapshot]) -> Option<String> {
    if processes.is_empty() {
        return None;
    }

    let rendered = processes
        .iter()
        .map(render_rss_process_summary_item)
        .collect::<Vec<_>>()
        .join(", ");

    Some(format!("  top rss=[{rendered}]"))
}

pub(super) fn format_sample_cpu_used_pct(sample: &SystemSample) -> String {
    format!("{:.1}", sample.host.cpu_usage_percent)
}

pub(super) fn format_sample_norm_load1(sample: &SystemSample) -> String {
    format_opt_f64(sample.host.norm_load1)
}

fn format_opt_f64(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.2}"))
}

fn format_opt_usize(value: Option<usize>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| value.to_string())
}

fn format_opt_bytes(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), format_bytes)
}

fn format_bytes_pair(used: Option<u64>, total: Option<u64>) -> String {
    match (used, total) {
        (Some(used), Some(total)) => format!("{}/{}", format_bytes(used), format_bytes(total)),
        _ => "n/a".to_owned(),
    }
}

fn format_bytes(value: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if value >= GIB {
        return format!("{:.1}GiB", value as f64 / GIB as f64);
    }

    if value >= MIB {
        return format!("{:.1}MiB", value as f64 / MIB as f64);
    }

    if value >= KIB {
        return format!("{:.1}KiB", value as f64 / KIB as f64);
    }

    format!("{value}B")
}
