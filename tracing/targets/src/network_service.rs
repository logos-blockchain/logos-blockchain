use lb_log_targets_macros::log_targets;

log_targets! {
    root = network_service;

    backends::{
        MOCK,
        libp2p::{
            CHAINSYNC,
            GOSSIPSUB,
            IDENTIFY,
            KADEMLIA,
        },
    }
}
