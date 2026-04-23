use std::{
    fs,
    io::{self, Write as _},
    path::Path,
};

use super::records::SystemMonitorRecord;

/// NDJSON sink for persisted monitor records.
pub(super) struct SystemStatsLog;

impl SystemStatsLog {
    pub(super) fn ensure_parent_dir(path: &Path) {
        if let Some(parent) = path.parent() {
            drop(fs::create_dir_all(parent));
        }
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
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        serde_json::to_writer(&mut file, record).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }
}
