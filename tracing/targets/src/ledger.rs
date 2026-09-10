use lb_log_targets_macros::log_targets;

log_targets! {
    root = ledger;

    cryptarchia::STAKE,
    mantle::{
        SDP,
        sdp::rewards::BLEND,
    },
}
