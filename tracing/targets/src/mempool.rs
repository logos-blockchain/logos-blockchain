use lb_log_targets_macros::log_targets;

log_targets! {
    root = mempool;

    SERVICE,
    POOL,
    network::{
        LIBP2P,
    },
}
