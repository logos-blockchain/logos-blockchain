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
    scheduling::{
        COVER,
        DELAY,
        proofs::CORE,
        proofs::CORE_AND_LEADER,
        proofs::CORE_LEADER_AND_POW,
        proofs::LEADER,
        proofs::POW,
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
