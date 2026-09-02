use lb_log_targets_macros::log_targets;

log_targets! {
    root = chain;

    broadcast::{},
    leader::{
        BLEND,
        LEADERSHIP,
    },
    network::{
        LIBP2P,
        SYNC,
        bootstrap::IBD,
    },
    service::{
        STORAGE,
        bootstrap::OFFLINE_GRACE_PERIOD,
        sync::BLOCK_PROVIDER,
    },
}
