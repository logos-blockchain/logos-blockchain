//! Shared helper for the spec-audit proofs of task `20261104T001156-96e84f`
//! that run a real `logos_blockchain_libp2p::Swarm` (public API only).
//!
//! * [`node_config`] builds a loopback `SwarmConfig` with a chosen NAT mode;
//! * [`identify_peer`] builds a plain libp2p peer (QUIC + Identify only, no
//!   AutoNAT server, no Kademlia) used to observe what a node advertises.
#![allow(dead_code, reason = "Each proof uses a subset of the helpers.")]

use std::{
    collections::HashMap,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::{Multiaddr, PeerId, Swarm as Libp2pSwarm, identify, swarm::SwarmEvent};
use logos_blockchain_libp2p::{
    ChainSyncSettings, IdentifySettings, KademliaSettings, NatSettings, SwarmConfig, ed25519,
    protocol_name::StreamProtocol,
};
use rand::rngs::OsRng;

pub const KAD_PROTOCOL: &str = "/audit/kad/1.0.0";
pub const IDENTIFY_PROTOCOL: &str = "/audit/identify/1.0.0";
pub const CHAIN_SYNC_PROTOCOL: &str = "/audit/chainsync/1.0.0";

pub type Node = logos_blockchain_libp2p::Swarm<OsRng>;

/// A loopback node configuration (QUIC on `127.0.0.1`, random port).
pub fn node_config(nat_config: NatSettings, identify_config: IdentifySettings) -> SwarmConfig {
    SwarmConfig {
        host: Ipv4Addr::LOCALHOST,
        port: 0,
        node_key: ed25519::SecretKey::generate(),
        gossipsub_config: libp2p::gossipsub::Config::default(),
        kad_protocol_name: StreamProtocol::new(KAD_PROTOCOL),
        identify_protocol_name: StreamProtocol::new(IDENTIFY_PROTOCOL),
        chain_sync_protocol_name: StreamProtocol::new(CHAIN_SYNC_PROTOCOL),
        kademlia_config: KademliaSettings::default(),
        identify_config,
        chain_sync_config: ChainSyncSettings {
            peer_response_timeout: Duration::from_secs(5),
            max_inbound_requests: 10.try_into().unwrap(),
        },
        nat_config,
    }
}

pub fn build_node(config: SwarmConfig) -> Node {
    logos_blockchain_libp2p::Swarm::build(config, HashMap::new(), OsRng).expect("node builds")
}

/// Drives `node` until it reports its listen address.
pub async fn node_listen_addr(node: &mut Node) -> Multiaddr {
    loop {
        if let Some(SwarmEvent::NewListenAddr { address, .. }) = node.next().await {
            return address;
        }
    }
}

/// A plain libp2p peer: QUIC transport and Identify, nothing else.
pub fn identify_peer() -> Libp2pSwarm<identify::Behaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_quic()
        .with_behaviour(|key| {
            identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.to_owned(),
                key.public(),
            ))
        })
        .expect("identify peer builds")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build()
}

/// Drives `node` and `peer` together until `peer` receives Identify info
/// from `node_id`, or `timeout` elapses.
pub async fn identify_info_of(
    node: &mut Node,
    peer: &mut Libp2pSwarm<identify::Behaviour>,
    node_id: PeerId,
    timeout: Duration,
) -> Option<identify::Info> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        tokio::select! {
            _ = node.next() => {}
            ev = peer.select_next_some() => {
                if let SwarmEvent::Behaviour(identify::Event::Received { peer_id, info, .. }) = ev
                    && peer_id == node_id
                {
                    return Some(info);
                }
            }
            () = tokio::time::sleep(remaining) => return None,
        }
    }
}
