use lb_log_targets_macros::log_targets;

log_targets! {
    root = api;

    http::{
        MANTLE,
        PPROF,
        wallet::{
            AGED_NOTES,
            BALANCE,
            CLAIMABLE_VOUCHERS,
            TRANSFER_FUNDS,
        },
    },
}
