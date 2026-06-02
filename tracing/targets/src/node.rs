use lb_log_targets_macros::log_targets;

log_targets! {
    root = node;

    CONFIG,
    api::{
        TRACING,
    },
}
