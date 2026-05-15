use lb_log_targets_macros::log_targets;

log_targets! {
    root = libp2p;

    behaviour::{
        kademlia::BEHAVIOUR,
        nat::{
            GATEWAY_MONITOR,
            INNER,
            address_mapper::protocols::{
                PCP,
                UPNP,
                pcp_core::{
                    CLIENT,
                    wire::{
                        ANNOUNCE,
                        MAP,
                    },
                },
            },
            state_machine::transitions::{
                MAPPED_PUBLIC,
                PUBLIC,
                TEST_IF_MAPPED_PUBLIC,
                TEST_IF_PUBLIC,
                TRY_MAP_ADDRESS,
            },
        },
    }
}
