use lb_log_targets_macros::log_targets;

log_targets! {
    root = cryptarchia;

    engine::{},
    sync::libp2p::{
        DOWNLOADER,
        PROVIDER,
        REQUESTS,
    },
}
