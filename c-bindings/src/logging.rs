use std::io::Write as _;

pub struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let mut stderr = std::io::stderr().lock();
        drop(writeln!(stderr, "[{}] {}", record.level(), record.args()));
        drop(stderr.flush());
    }

    fn flush(&self) {}
}

/// Installs a minimal stderr logger via the `log` crate (once).
/// Ensures that `log::error!` calls are visible on stderr when no other logger
/// is configured.
pub fn install_stderr_logger() {
    let _ = log::set_logger(&StderrLogger);
    log::set_max_level(log::LevelFilter::Warn);
}
