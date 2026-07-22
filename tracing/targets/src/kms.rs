use lb_log_targets_macros::log_targets;

log_targets! {
    root = kms;

    SERVICE,
    keys::{
        ZK,
    },
    operators::{
        BLEND_POQ,
        ED25519,
        ZK_LEADER,
        ZK_VOUCHER,
    },
}
