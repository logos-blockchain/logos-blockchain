#![allow(
    clippy::multiple_inherent_impl,
    reason = "We split the `Swarm` impls into different modules for better code modularity."
)]

use std::{
    collections::HashMap,
    error::Error,
    io,
    net::Ipv4Addr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use hickory_resolver::system_conf::read_system_conf;
use lb_log_targets::libp2p as lb_log_targets_libp2p;
use libp2p::{
    Multiaddr, PeerId, TransportError,
    dns::{ResolverConfig, ResolverOpts},
    identity::ed25519,
    swarm::{ConnectionId, DialError, SwarmEvent, dial_opts::DialOpts},
};
use multiaddr::multiaddr;
use rand::RngCore;

pub use crate::{
    SwarmConfig,
    behaviour::{Behaviour, BehaviourEvent},
};
use crate::{behaviour::BehaviourConfig, protocol_name::StreamProtocol};

/// How long to keep a connection alive once it is idling.
const IDLE_CONN_TIMEOUT: Duration = Duration::from_mins(5);

const LOG_TARGET: &str = lb_log_targets_libp2p::ROOT;

/// Wraps [`libp2p::Swarm`], and config it for use within Logos blockchain.
pub struct Swarm<R: Clone + Send + RngCore + 'static> {
    // A core libp2p swarm
    pub(crate) swarm: libp2p::Swarm<Behaviour<R>>,
}

impl<R: Clone + Send + RngCore + 'static> Swarm<R> {
    /// Builds a [`Swarm`] configured for use with Logos blockchain on top of a
    /// tokio executor.
    pub fn build(
        config: SwarmConfig,
        chain_sync_protocol_name: StreamProtocol,
        max_data_size_by_topic: HashMap<libp2p::gossipsub::TopicHash, usize>,
        rng: R,
    ) -> Result<Self, Box<dyn Error>> {
        let keypair =
            libp2p::identity::Keypair::from(ed25519::Keypair::from(config.node_key.clone()));
        let peer_id = PeerId::from(keypair.public());
        tracing::info!(target: LOG_TARGET, "libp2p peer_id:{}", peer_id);

        let SwarmConfig {
            gossipsub_config,
            kademlia_config,
            identify_config,
            chain_sync_config,
            nat_config,
            identify_protocol_name,
            kad_protocol_name,
            host,
            port,
            ..
        } = config;

        let (dns_config, dns_options) = system_dns_config();
        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_quic()
            .with_dns_config(dns_config, dns_options)
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
                        max_data_size_by_topic,
                    },
                    rng,
                )
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_CONN_TIMEOUT))
            .build();

        let lb_swarm = {
            let listen_addr = multiaddr(host, port);
            let mut s = Self { swarm };
            // We start listening on the provided address, which triggers the Identify flow,
            // which in turn triggers our NAT traversal state machine.
            s.start_listening_on(listen_addr.clone())
                .map_err(|e| format!("Failed to listen on {listen_addr}: {e}"))?;
            Ok::<_, Box<dyn Error>>(s)
        }?;

        Ok(lb_swarm)
    }

    /// Initiates a connection attempt to a peer
    pub fn connect(&mut self, peer_addr: &Multiaddr) -> Result<ConnectionId, DialError> {
        let opt = DialOpts::from(peer_addr.clone());
        let connection_id = opt.connection_id();

        tracing::debug!(
            target: LOG_TARGET,
            "attempting to dial {peer_addr}. connection_id:{connection_id:?}",
        );
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

/// The host's DNS resolver configuration, to build a swarm with
/// [`SwarmBuilder::with_dns_config`](libp2p::SwarmBuilder::with_dns_config).
///
/// Unlike [`SwarmBuilder::with_dns`](libp2p::SwarmBuilder::with_dns), which
/// reads the same configuration, this does not fail when the host has no
/// nameserver configured, as a Mac has none while offline. A swarm built at
/// such a time gets a resolver without nameservers: its DNS addresses fail to
/// dial until the swarm is built again, and every other address dials as
/// usual.
#[must_use]
pub fn system_dns_config() -> (ResolverConfig, ResolverOpts) {
    read_system_conf().unwrap_or_else(|e| {
        tracing::warn!(
            target: LOG_TARGET,
            "Failed to read the system DNS configuration, so DNS addresses will fail to dial until the swarm is built again: {e}"
        );
        // Not `ResolverConfig::default()`, which sends every lookup to
        // Google's public resolvers.
        (ResolverConfig::new(), ResolverOpts::default())
    })
}
