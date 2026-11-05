//! Proof for finding `20261104T001156-96e84f/1` (P2P-NAT-SOLUTION, Phase 1 -> 2).
//!
//! Spec (NAT state machine): "AutoNAT -- Node's IP address not reachable by
//! AutoNAT server or Timeout --> Port Mapping Client"; Phase 1: "Otherwise,
//! the node continues to the next phase" (Phase 2: Automated Port Mapping).
//!
//! Setup, all on loopback, no external network:
//! * the client runs the crate's public `behaviour::nat::Behaviour`
//!   (default `Traversal` settings) plus Identify, and listens on
//!   `127.0.0.2` -- its "LAN" address;
//! * the server runs an AutoNAT v2 server plus Identify on `127.0.0.1`;
//! * the client dials the server, which observes the client as
//!   `127.0.0.1:<port>` (the "NAT's" address). Identify reports that as the
//!   external address candidate, AutoNAT v2 tests it, and the dial-back fails
//!   because nothing listens there -- exactly the NAT'd-node situation.
//!
//! The AutoNAT client reports the failure, but the NAT state machine only
//! reacts to failures for the first *listen* address it saw, so it never
//! issues `MapAddress`: no port-mapping attempt (PCP/NAT-PMP/UPnP) is made and
//! no address-mapper event is ever emitted.
//!
//! Run: `cargo test --locked -p logos-blockchain-libp2p --all-features
//!       --test spec_20261104t001156_96e84f_1`

use std::time::{Duration, Instant};

use either::Either;
use futures::StreamExt as _;
use libp2p::{
    Multiaddr, Swarm, autonat, identify, identity,
    swarm::{NetworkBehaviour, SwarmEvent},
};
use libp2p_swarm_test::SwarmExt as _;
use logos_blockchain_libp2p::{
    AutonatClientSettings, GatewaySettings, NatMappingSettings, NatSettings, TraversalSettings,
    behaviour::nat,
};
use rand::rngs::OsRng;

/// How long a spec-conforming node may take, after AutoNAT reported its
/// address unreachable, to finish one port-mapping attempt in this container
/// (no PCP/NAT-PMP/UPnP gateway: PCP+NAT-PMP time out after 1 s each, the
/// UPnP SSDP search after 10 s).
const MAPPING_WINDOW: Duration = Duration::from_secs(30);

#[derive(NetworkBehaviour)]
struct Client {
    nat: nat::Behaviour<OsRng>,
    identify: identify::Behaviour,
}

#[derive(NetworkBehaviour)]
struct Server {
    autonat_server: autonat::v2::server::Behaviour<OsRng>,
    identify: identify::Behaviour,
}

fn client_settings() -> NatSettings {
    NatSettings::Traversal(TraversalSettings {
        autonat: AutonatClientSettings {
            probe_interval_millisecs: Some(100),
            ..AutonatClientSettings::default()
        },
        mapping: NatMappingSettings {
            max_retries: 0,
            ..NatMappingSettings::default()
        },
        // Keep the gateway monitor out of the picture.
        gateway_monitor: GatewaySettings {
            check_interval: Duration::from_secs(10 * 24 * 3600),
        },
    })
}

async fn listen<B>(swarm: &mut Swarm<B>, addr: &str) -> Multiaddr
where
    B: NetworkBehaviour + Send,
    B::ToSwarm: std::fmt::Debug,
{
    swarm.listen_on(addr.parse().unwrap()).unwrap();
    swarm
        .wait(|e| match e {
            SwarmEvent::NewListenAddr { address, .. } => Some(address),
            _ => None,
        })
        .await
}

#[tokio::test]
async fn autonat_unreachable_moves_node_to_port_mapping() {
    let mut client = Swarm::new_ephemeral_tokio(|k: identity::Keypair| Client {
        nat: nat::Behaviour::new(OsRng, &client_settings()),
        identify: identify::Behaviour::new(identify::Config::new("/audit".into(), k.public())),
    });
    let mut server = Swarm::new_ephemeral_tokio(|k: identity::Keypair| Server {
        autonat_server: autonat::v2::server::Behaviour::new(OsRng),
        identify: identify::Behaviour::new(identify::Config::new("/audit".into(), k.public())),
    });

    let client_listen = listen(&mut client, "/ip4/127.0.0.2/tcp/0").await;
    let server_addr = listen(&mut server, "/ip4/127.0.0.1/tcp/0").await;
    client.dial(server_addr).unwrap();
    tokio::spawn(async move { while server.next().await.is_some() {} });

    // 1. Wait for AutoNAT to report the node's (observed) address unreachable.
    let setup_deadline = Instant::now() + Duration::from_secs(20);
    let unreachable = loop {
        let ev = tokio::time::timeout_at(setup_deadline.into(), client.next())
            .await
            .expect("setup: AutoNAT should have probed the observed address")
            .unwrap();
        match ev {
            SwarmEvent::Behaviour(ClientEvent::Nat(Either::Left(autonat_ev)))
                if autonat_ev.result.is_err() =>
            {
                break autonat_ev.tested_addr;
            }
            SwarmEvent::Behaviour(ClientEvent::Nat(Either::Left(autonat_ev))) => {
                panic!("setup: the observed address must not be reachable: {autonat_ev:?}")
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                panic!("setup: no address should be confirmed, got {address}")
            }
            _ => {}
        }
    };

    // 2. Spec: the node now continues with Phase 2 (port mapping). Any
    //    attempt surfaces as an address-mapper event (`Either::Right`).
    let mapping_deadline = Instant::now() + MAPPING_WINDOW;
    let mut mapping_event = None;
    while let Ok(Some(ev)) = tokio::time::timeout_at(mapping_deadline.into(), client.next()).await {
        if let SwarmEvent::Behaviour(ClientEvent::Nat(Either::Right(mapper_ev))) = ev {
            mapping_event = Some(format!("{mapper_ev:?}"));
            break;
        }
    }

    assert!(
        mapping_event.is_some(),
        "P2P-NAT-SOLUTION Phase 1 -> Phase 2 violated: AutoNAT reported the node's \
         address {unreachable} unreachable, but within {MAPPING_WINDOW:?} the node made \
         no port-mapping attempt (no PCP/NAT-PMP/UPnP result). The NAT state machine is \
         still waiting on its listen address {client_listen}, which AutoNAT never tests."
    );
}
