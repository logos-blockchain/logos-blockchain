//! Proof for finding `20261104T001156-96e84f/2` (P2P-NAT-SOLUTION, Phase 3).
//!
//! Spec, Phase 3 "Network Monitoring": "Monitor public reachability of mapped
//! addresses -- loop periodically checks public reachability: AutoNAT Client
//! ->> AutoNAT Server: Requests dialback"; "A Public node must [restart from
//! Phase 1] when: AutoNAT client no longer confirms that at least one of the
//! node's addresses is publicly reachable."
//!
//! The crate exposes `retest_successful_external_addresses_interval` ("The
//! interval at which we will retest successful external addresses ... to
//! ensure that the external address is still valid and reachable"). The proof
//! sets it to 200 ms, lets an AutoNAT v2 server confirm the client's address,
//! and then counts the dial-back probes the server receives for that address
//! over 25 retest intervals. A node that monitors its reachability must probe
//! again; this one never does (the retest tick in `nat/inner.rs` is a TODO and
//! libp2p's AutoNAT v2 client never re-tests a confirmed candidate), so it can
//! never notice that its public address became unreachable.
//!
//! Run: `cargo test --locked -p logos-blockchain-libp2p --all-features
//!       --test spec_20261104t001156_96e84f_2`

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, Swarm, autonat, identify, identity,
    swarm::{NetworkBehaviour, SwarmEvent},
};
use libp2p_swarm_test::SwarmExt as _;
use logos_blockchain_libp2p::{
    AutonatClientSettings, GatewaySettings, NatSettings, TraversalSettings, behaviour::nat,
};
use rand::rngs::OsRng;

const RETEST_INTERVAL: Duration = Duration::from_millis(200);
const OBSERVATION: Duration = Duration::from_secs(5);

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
async fn confirmed_address_is_periodically_rechecked_with_autonat() {
    let settings = NatSettings::Traversal(TraversalSettings {
        autonat: AutonatClientSettings {
            probe_interval_millisecs: Some(50),
            retest_successful_external_addresses_interval: RETEST_INTERVAL,
            ..AutonatClientSettings::default()
        },
        gateway_monitor: GatewaySettings {
            check_interval: Duration::from_secs(10 * 24 * 3600),
        },
        ..TraversalSettings::default()
    });
    let mut client = Swarm::new_ephemeral_tokio(|k: identity::Keypair| Client {
        nat: nat::Behaviour::new(OsRng, &settings),
        identify: identify::Behaviour::new(identify::Config::new("/audit".into(), k.public())),
    });
    let mut server = Swarm::new_ephemeral_tokio(|k: identity::Keypair| Server {
        autonat_server: autonat::v2::server::Behaviour::new(OsRng),
        identify: identify::Behaviour::new(identify::Config::new("/audit".into(), k.public())),
    });

    let client_addr = listen(&mut client, "/ip4/127.0.0.1/tcp/0").await;
    let server_addr = listen(&mut server, "/ip4/127.0.0.1/tcp/0").await;
    client.dial(server_addr).unwrap();
    tokio::spawn(async move { while client.next().await.is_some() {} });

    // Count every dial-back probe the AutoNAT server performs for the
    // client's address, before and after the first confirmation.
    let mut first_confirmation = None;
    let mut probes_after_confirmation = 0usize;
    let setup_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let deadline = first_confirmation.map_or(setup_deadline, |t: Instant| t + OBSERVATION);
        let Ok(Some(ev)) = tokio::time::timeout_at(deadline.into(), server.next()).await else {
            break;
        };
        if let SwarmEvent::Behaviour(ServerEvent::AutonatServer(autonat::v2::server::Event {
            tested_addr,
            result,
            ..
        })) = ev
        {
            if tested_addr != client_addr {
                continue;
            }
            match (first_confirmation, result) {
                (None, Ok(())) => first_confirmation = Some(Instant::now()),
                (None, Err(e)) => panic!("setup: first probe of {tested_addr} failed: {e}"),
                (Some(_), _) => probes_after_confirmation += 1,
            }
        }
    }

    assert!(
        first_confirmation.is_some(),
        "setup: the AutoNAT server never confirmed {client_addr}"
    );
    assert!(
        probes_after_confirmation >= 1,
        "P2P-NAT-SOLUTION Phase 3 violated: after AutoNAT confirmed {client_addr} the node \
         never asked for another dial-back in {OBSERVATION:?} ({} retest intervals of \
         {RETEST_INTERVAL:?}); its public reachability is not monitored, so losing it can \
         never trigger the restart from Phase 1.",
        OBSERVATION.as_millis() / RETEST_INTERVAL.as_millis()
    );
}
