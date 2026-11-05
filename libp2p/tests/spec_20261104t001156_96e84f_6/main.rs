//! Proof for finding `20261104T001156-96e84f/6` (P2P-NAT-SOLUTION, Phase 2 +
//! "Node duties once Public status is assumed").
//!
//! Spec: "If the mapping is successful, the node uses an AutoNAT client to
//! confirm that Public nodes can reach it"; a Public node must "Listen on and
//! advertise via the Identify protocol its publicly reachable multiaddresses in
//! the form: `/{public_peer_ip}/udp/{port}/quic-v1/p2p/{peer_id}`".
//!
//! `PcpProtocol::map_address` (pcp.rs) parses the listen address with
//! `parse_multiaddr` and builds the result with `build_external_multiaddr`,
//! which only emits `/ip4/<ip>/<tcp|udp>/<port>`. For the node's QUIC listen
//! address the PCP-mapped address therefore loses `/quic-v1`: it is not in the
//! spec's form and no transport of the node can dial it, so the AutoNAT
//! confirmation of a successful PCP mapping can never succeed. (The NAT-PMP and
//! UPnP paths keep the suffix because they edit the listen address in place.)
//!
//! This crate compiles `pcp.rs` (pasted verbatim) with the real `pcp_core` and
//! `errors` modules at their original module paths, and calls the two helpers
//! exactly as `map_address` does for a granted PCP mapping.
//!
//! Run: `cargo test --locked -p logos-blockchain-libp2p --all-features
//!       --test spec_20261104t001156_96e84f_6`
#![allow(dead_code, unused_imports, reason = "Unmodified crate sources compiled into a test crate.")]

#[path = "../../src/behaviour/nat/address_mapper/errors.rs"]
pub mod am_errors;

#[path = "../../src/behaviour/nat/address_mapper/protocols/pcp_core/mod.rs"]
pub mod am_pcp_core;

pub mod behaviour {
    pub mod nat {
        pub mod address_mapper {
            pub use crate::am_errors as errors;
            pub mod protocols {
                pub use crate::am_pcp_core as pcp_core;
            }
        }
    }
}
pub mod config {
    pub use logos_blockchain_libp2p::NatMappingSettings;
}

/// `protocols/pcp.rs` verbatim, plus an accessor for its private helpers.
pub mod pcp {
    include!("../../src/behaviour/nat/address_mapper/protocols/pcp.rs");

    /// The multiaddr `PcpProtocol::map_address` returns for `listen_addr` when
    /// the PCP server grants `external_ip:external_port` (pcp.rs: line 31
    /// `parse_multiaddr(address)`, lines 69-73 `build_external_multiaddr(..)`).
    pub fn mapped_external_address(
        listen_addr: &Multiaddr,
        external_ip: Ipv4Addr,
        external_port: u16,
    ) -> Multiaddr {
        let (_socket_addr, protocol) = parse_multiaddr(listen_addr).expect("valid listen address");
        build_external_multiaddr(
            external_ip,
            NonZeroU16::new(external_port).expect("non-zero port"),
            protocol,
        )
    }
}

use libp2p::{
    Multiaddr, Transport as _,
    core::{
        Endpoint,
        transport::{DialOpts, PortUse},
    },
    identity,
    multiaddr::Protocol,
};

#[tokio::test]
async fn pcp_mapped_quic_address_keeps_quic_v1() {
    // The node listens on QUIC only (swarm.rs: `multiaddr!(Ip4(ip), Udp(port), QuicV1)`).
    let listen: Multiaddr = "/ip4/192.168.1.5/udp/3000/quic-v1".parse().unwrap();
    let mapped = pcp::mapped_external_address(&listen, "203.0.113.7".parse().unwrap(), 3000);

    // Can the node's own transport (QUIC) dial the address AutoNAT is asked
    // to confirm?
    let mut quic = libp2p::quic::tokio::Transport::new(libp2p::quic::Config::new(
        &identity::Keypair::generate_ed25519(),
    ));
    let dialable = quic
        .dial(
            mapped.clone(),
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .is_ok();

    assert!(
        matches!(mapped.iter().last(), Some(Protocol::QuicV1)),
        "P2P-NAT-SOLUTION Phase 2 violated: a PCP mapping of the QUIC listen address \
         {listen} yields {mapped}, not the spec form /{{ip}}/udp/{{port}}/quic-v1; \
         the QUIC transport accepts it for dialing: {dialable}, so the AutoNAT \
         confirmation of the mapping cannot succeed."
    );
}
