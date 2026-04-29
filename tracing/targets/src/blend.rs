use logos_blockchain_log_targets_macros::log_targets;

log_targets! {
    blend::backend::LIBP2P,
    blend::message::REWARD,
    blend::network::core::core::BEHAVIOUR,
    blend::network::core::core::behaviour::OLD,
    blend::network::core::core::conn::HANDLER,
    blend::network::core::core::conn::MAINTENANCE,
    blend::network::core::edge::BEHAVIOUR,
    blend::network::core::handler::CORE_EDGE,
    blend::scheduling::COVER,
    blend::scheduling::DELAY,
    blend::scheduling::proofs::CORE,
    blend::scheduling::proofs::CORE_AND_LEADER,
    blend::scheduling::proofs::LEADER,
    blend::service::CORE,
    blend::service::EDGE,
    blend::service::EPOCH,
    blend::service::MODES,
    blend::service::core::KMS_POQ_GENERATOR,
    blend::service::edge::backend::LIBP2P,
}

pub use self::blend::*;
