use lb_log_targets_macros::log_targets;

log_targets! {
    root = mantle;

    sdp::{
        message::{
            ACTIVE,
            WITHDRAW,
        },
    },
}
