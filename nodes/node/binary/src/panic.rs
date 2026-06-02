use std::{
    backtrace::{Backtrace, BacktraceStatus},
    panic::PanicHookInfo,
};

use lb_log_targets::node;

const LOG_TARGET: &str = node::ROOT;

pub fn log_and_exit_hook(panic_info: &PanicHookInfo) {
    let payload = panic_info.payload();

    let payload = payload.downcast_ref::<&str>().map_or_else(
        || payload.downcast_ref::<String>().map(String::as_str),
        |s| Some(&**s),
    );

    let location = panic_info.location().map(ToString::to_string);
    let backtrace = Backtrace::capture();
    let note = (backtrace.status() == BacktraceStatus::Disabled)
        .then_some("run with RUST_BACKTRACE=1 environment variable to display a backtrace");

    tracing::error!(
        target: LOG_TARGET,
        panic_payload = payload,
        panic_location = location,
        panic_backtrace = backtrace.to_string(),
        panic_note = note,
        "A panic occurred",
    );

    #[cfg(feature = "dhat-heap")]
    crate::global_allocators::dhat_heap::drop_dhat_profiler();

    std::process::exit(1);
}
