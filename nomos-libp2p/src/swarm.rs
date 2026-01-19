#![allow(
    clippy::multiple_inherent_impl,
    reason = "We split the `Swarm` impls into different modules for better code modularity."
)]

use std::{
    error::Error,
    io,
    net::Ipv4Addr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use libp2p::{
    Multiaddr, PeerId, TransportError,
    identity::ed25519,
    swarm::{ConnectionId, DialError, SwarmEvent, dial_opts::DialOpts},
};
use multiaddr::multiaddr;
use rand::RngCore;

use crate::behaviour::BehaviourConfig;
pub use crate::{
    SwarmConfig,
    behaviour::{Behaviour, BehaviourEvent},
};

/// How long to keep a connection alive once it is idling.
const IDLE_CONN_TIMEOUT: Duration = Duration::from_secs(300);

/// Wraps [`libp2p::Swarm`], and config it for use within Nomos.
pub struct Swarm<R: Clone + Send + RngCore + 'static> {
    // A core libp2p swarm
    pub(crate) swarm: libp2p::Swarm<Behaviour<R>>,
}

impl<R: Clone + Send + RngCore + 'static> Swarm<R> {
    /// Builds a [`Swarm`] configured for use with Nomos on top of a tokio
    /// executor.
    pub fn build(config: SwarmConfig, rng: R) -> Result<Self, Box<dyn Error>> {
        let keypair =
            libp2p::identity::Keypair::from(ed25519::Keypair::from(config.node_key.clone()));
        let peer_id = PeerId::from(keypair.public());
        tracing::info!("libp2p peer_id:{}", peer_id);

        let SwarmConfig {
            gossipsub_config,
            kademlia_config,
            identify_config,
            chain_sync_config,
            nat_config,
            identify_protocol_name,
            kad_protocol_name,
            chain_sync_protocol_name,
            host,
            port,
            ..
        } = config;

        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_quic()
            .with_dns()?
            .with_behaviour(move |keypair| {
                Behaviour::new(
                    BehaviourConfig {
                        gossipsub_config,
                        kademlia_config: kademlia_config.clone(),
                        identify_config,
                        nat_config,
                        kad_protocol_name: kad_protocol_name.into(),
                        identify_protocol_name: identify_protocol_name.into(),
                        chain_sync_protocol_name: chain_sync_protocol_name.into(),
                        public_key: keypair.public(),
                        chain_sync_config,
                    },
                    rng,
                )
                .expect("Behaviour should not fail to set up.")
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_CONN_TIMEOUT))
            .build();

        let nomos_swarm = {
            let listen_addr = multiaddr(host, port);
            let mut s = Self { swarm };
            // We start listening on the provided address, which triggers the Identify flow,
            // which in turn triggers our NAT traversal state machine.
            s.start_listening_on(listen_addr.clone())
                .map_err(|e| format!("Failed to listen on {listen_addr}: {e}"))?;
            Ok::<_, Box<dyn Error>>(s)
        }?;

        Ok(nomos_swarm)
    }

    /// Initiates a connection attempt to a peer
    pub fn connect(&mut self, peer_addr: &Multiaddr) -> Result<ConnectionId, DialError> {
        let opt = DialOpts::from(peer_addr.clone());
        let connection_id = opt.connection_id();

        tracing::debug!("attempting to dial {peer_addr}. connection_id:{connection_id:?}",);
        self.swarm.dial(opt)?;
        Ok(connection_id)
    }

    pub fn start_listening_on(&mut self, addr: Multiaddr) -> Result<(), TransportError<io::Error>> {
        self.swarm.listen_on(addr)?;
        Ok(())
    }

    /// Returns a reference to the underlying [`libp2p::Swarm`]
    pub const fn swarm(&self) -> &libp2p::Swarm<Behaviour<R>> {
        &self.swarm
    }
}

impl<R: Clone + Send + RngCore + 'static> futures::Stream for Swarm<R> {
    type Item = SwarmEvent<BehaviourEvent<R>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.swarm).poll_next(cx)
    }
}

#[must_use]
pub fn multiaddr(ip: Ipv4Addr, port: u16) -> Multiaddr {
    multiaddr!(Ip4(ip), Udp(port), QuicV1)
}
