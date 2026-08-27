use lb_log_targets_macros::log_targets;

log_targets! {
    root = blend;

    backend::{LIBP2P},
    message::{REWARD},
    network::core::{
        core::BEHAVIOUR,
        core::behaviour::OLD,
        core::conn::HANDLER,
        core::conn::MAINTENANCE,
        edge::BEHAVIOUR,
        handler::CORE_EDGE,
    },
    processor::{
        core_and_leader::SEND,
        leader::SEND,
    },
    prover::{
        CORE,
        CORE_AND_LEADER,
        CORE_LEADER_AND_POW,
        LEADER,
        LEADER_AND_POW,
        POW
    },
    scheduling::{
        COVER,
        DELAY,
    },
    service::{
        CORE,
        EDGE,
        EPOCH,
        MODES,
        core::KMS_POQ_GENERATOR,
        edge::backend::LIBP2P,
    }
}
