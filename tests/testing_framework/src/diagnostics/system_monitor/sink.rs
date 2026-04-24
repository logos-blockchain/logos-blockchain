use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use super::records::{CsvSampleRow, SystemMonitorRecord};

/// Persisted monitor sinks for structured logs and flat spreadsheet exports.
pub(super) struct SystemStatsLog;

impl SystemStatsLog {
    pub(super) fn ensure_parent_dir(path: &Path) {
        if let Some(parent) = path.parent() {
            drop(fs::create_dir_all(parent));
        }
    }

    pub(super) fn reset_output(path: &Path) {
        Self::ensure_parent_dir(path);

        let _unused = fs::remove_file(path);
        let _unused = fs::remove_file(csv_path(path));
    }

    pub(super) fn append(path: &Path, record: &SystemMonitorRecord) {
        if let Err(error) = Self::append_impl(path, record) {
            eprintln!(
                "failed to append system monitor record to {}: {error}",
                path.display()
            );
        }
    }

    fn append_impl(path: &Path, record: &SystemMonitorRecord) -> io::Result<()> {
        Self::append_ndjson(path, record)?;

        if let Some(sample) = record.as_sample() {
            Self::append_csv(&csv_path(path), &CsvSampleRow::from_sample(sample))?;
        }

        Ok(())
    }

    fn append_ndjson(path: &Path, record: &SystemMonitorRecord) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        serde_json::to_writer(&mut file, record).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }

    fn append_csv(path: &Path, row: &CsvSampleRow) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        if file.metadata()?.len() == 0 {
            write_csv_row(&mut file, CsvSampleRow::HEADER)?;
        }

        write_csv_row(&mut file, &row.values())
    }
}

fn csv_path(path: &Path) -> PathBuf {
    path.with_extension("csv")
}

fn write_csv_row(file: &mut fs::File, columns: &[impl AsRef<str>]) -> io::Result<()> {
    let line = columns
        .iter()
        .map(|value| escape_csv_field(value.as_ref()))
        .collect::<Vec<_>>()
        .join(",");

    writeln!(file, "{line}")
}

fn escape_csv_field(value: &str) -> String {
    if !value.contains([',', '"', '\n', '\r']) {
        return value.to_owned();
    }

    let escaped = value.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::diagnostics::system_monitor::records::{
        HostSnapshot, NodeProcessSummary, SystemSample,
    };

    #[test]
    fn appending_sample_writes_ndjson_and_csv() {
        let tempdir = tempdir().expect("tempdir should be created");
        let ndjson_path = tempdir.path().join("system_stats.ndjson");
        let record = SystemMonitorRecord::Sample(Box::new(SystemSample {
            ts_unix: 42,
            os: "linux",
            host: HostSnapshot {
                logical_cpus: 8,
                process_count: 42,
                thread_count: Some(128),
                cpu_usage_percent: 75.5,
                load1: Some(8.0),
                load5: Some(7.0),
                load15: Some(6.0),
                norm_load1: Some(1.0),
                memory_used_bytes: Some(100),
                memory_total_bytes: Some(200),
                swap_used_bytes: Some(10),
            },
            disk: None,
            node_processes: NodeProcessSummary {
                count: 2,
                total_rss_bytes: 300,
            },
            top_cpu_processes: Vec::new(),
            top_rss_processes: Vec::new(),
        }));

        SystemStatsLog::append(&ndjson_path, &record);

        let ndjson = fs::read_to_string(&ndjson_path).expect("ndjson log should exist");
        let csv = fs::read_to_string(tempdir.path().join("system_stats.csv"))
            .expect("csv log should exist");

        assert!(ndjson.contains("\"record_type\":\"sample\""));
        assert!(csv.starts_with("ts_unix,os,logical_cpus"));
        assert!(csv.contains("\n42,linux,8,42,128,75.5,8,7,6,1,100,200,10,,"));
    }

    #[test]
    fn appending_event_does_not_create_csv() {
        let tempdir = tempdir().expect("tempdir should be created");
        let ndjson_path = tempdir.path().join("system_stats.ndjson");
        let record = SystemMonitorRecord::Event(
            crate::diagnostics::system_monitor::records::SystemEvent::new("test_event", "detail"),
        );

        SystemStatsLog::append(&ndjson_path, &record);

        assert!(ndjson_path.exists());
        assert!(!tempdir.path().join("system_stats.csv").exists());
    }
}
