use std::path::{Path, PathBuf};

use sysinfo::{DiskRefreshKind, Disks, ProcessesToUpdate, RefreshKind, System};
use time::OffsetDateTime;

use super::records::{
    DiskSnapshot, HostSnapshot, SystemSample, collect_node_process_summary, collect_thread_count,
    collect_top_cpu_processes, collect_top_rss_processes, finite_f32, finite_f64,
};

/// `sysinfo`-backed collector for one host sample.
pub(super) struct SystemCollector {
    system: System,
    disks: Disks,
    cwd: PathBuf,
}

impl SystemCollector {
    pub(super) fn new() -> Self {
        let mut system = System::new_with_specifics(RefreshKind::everything());
        system.refresh_all();

        Self {
            system,
            disks: Disks::new_with_refreshed_list_specifics(DiskRefreshKind::everything()),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    pub(super) fn capture(&mut self) -> SystemSample {
        self.system.refresh_memory();
        self.system.refresh_cpu_usage();
        self.system.refresh_processes(ProcessesToUpdate::All, true);
        self.disks
            .refresh_specifics(false, DiskRefreshKind::everything());

        let processes = self.system.processes().values().collect::<Vec<_>>();
        let logical_cpus = self.system.cpus().len().max(1);
        let load_average = System::load_average();
        let load1 = finite_f64(load_average.one);

        SystemSample {
            ts_unix: OffsetDateTime::now_utc().unix_timestamp(),
            os: std::env::consts::OS,
            host: HostSnapshot {
                logical_cpus,
                process_count: processes.len(),
                thread_count: collect_thread_count(&processes),
                cpu_usage_percent: finite_f32(self.system.global_cpu_usage()).unwrap_or(0.0),
                load1,
                load5: finite_f64(load_average.five),
                load15: finite_f64(load_average.fifteen),
                norm_load1: load1.and_then(|value| finite_f64(value / logical_cpus as f64)),
                memory_used_bytes: Some(self.system.used_memory()),
                memory_total_bytes: Some(self.system.total_memory()),
                swap_used_bytes: Some(self.system.used_swap()),
            },
            disk: select_workspace_disk(&self.disks, &self.cwd),
            node_processes: collect_node_process_summary(&processes),
            top_cpu_processes: collect_top_cpu_processes(&processes),
            top_rss_processes: collect_top_rss_processes(&processes),
        }
    }
}

fn select_workspace_disk(disks: &Disks, cwd: &Path) -> Option<DiskSnapshot> {
    disks
        .list()
        .iter()
        .filter(|disk| cwd.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| DiskSnapshot {
            mount_point: disk.mount_point().display().to_string(),
            available_bytes: disk.available_space(),
            total_bytes: disk.total_space(),
        })
}
