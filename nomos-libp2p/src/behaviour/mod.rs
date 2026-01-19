#![allow(
    clippy::multiple_inherent_impl,
    reason = "We split the `Behaviour` impls into different modules for better code modularity."
)]

use std::error::Error;

use cryptarchia_sync::ChainSyncError;
use libp2p::{PeerId, StreamProtocol, autonat, identify, identity, kad, swarm::NetworkBehaviour};
use rand::RngCore;
use thiserror::Error;

use crate::{
    IdentifySettings, KademliaSettings, NatSettings, behaviour::gossipsub::compute_message_id,
};

pub mod chainsync;
pub mod gossipsub;
pub mod kademlia;
pub mod nat;

const DATA_LIMIT: usize = 1 << 16; // Do not serialize/deserialize more than 256 KiB

pub(crate) struct BehaviourConfig {
    pub gossipsub_config: libp2p::gossipsub::Config,
    pub kademlia_config: KademliaSettings,
    pub identify_config: IdentifySettings,
    pub nat_config: NatSettings,
    pub kad_protocol_name: StreamProtocol,
    pub identify_protocol_name: StreamProtocol,
    pub chain_sync_protocol_name: StreamProtocol,
    pub public_key: identity::PublicKey,
    pub chain_sync_config: cryptarchia_sync::Config,
}

#[derive(Debug, Error)]
pub enum BehaviourError {
    #[error("Operation not supported")]
    OperationNotSupported,
    #[error("Chainsync error: {0}")]
    ChainSyncError(#[from] ChainSyncError),
}

#[derive(NetworkBehaviour)]
pub struct Behaviour<Rng: Clone + Send + RngCore + 'static> {
    pub(crate) gossipsub: libp2p::gossipsub::Behaviour,
    // todo: support persistent store if needed
    pub(crate) kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub(crate) identify: identify::Behaviour,
    pub(crate) chain_sync: cryptarchia_sync::Behaviour,
    // The spec makes it mandatory to run an autonat server for a public node.
    pub(crate) autonat_server: autonat::v2::server::Behaviour<Rng>,
    pub(crate) nat: nat::Behaviour<Rng>,
}

impl<Rng: Clone + Send + RngCore + 'static> Behaviour<Rng> {
    pub(crate) fn new(config: BehaviourConfig, rng: Rng) -> Result<Self, Box<dyn Error>> {
        let BehaviourConfig {
            gossipsub_config,
            kademlia_config,
            identify_config,
            chain_sync_config,
            nat_config,
            kad_protocol_name,
            identify_protocol_name,
            chain_sync_protocol_name,
            public_key,
        } = config;

        let peer_id = PeerId::from(public_key.clone());

        let gossipsub = libp2p::gossipsub::Behaviour::new(
            libp2p::gossipsub::MessageAuthenticity::Author(peer_id),
            libp2p::gossipsub::ConfigBuilder::from(gossipsub_config)
                .validation_mode(libp2p::gossipsub::ValidationMode::None)
                .message_id_fn(compute_message_id)
                .max_transmit_size(DATA_LIMIT)
                .build()?,
        )?;

        let identify = identify::Behaviour::new(
            identify_config.to_libp2p_config(public_key, &identify_protocol_name),
        );

        let kademlia = kad::Behaviour::with_config(
            peer_id,
            kad::store::MemoryStore::new(peer_id),
            kademlia_config.to_libp2p_config(kad_protocol_name),
        );

        let autonat_server = autonat::v2::server::Behaviour::new(rng.clone());
        let nat = nat::Behaviour::new(rng, &nat_config);

        let chain_sync =
            cryptarchia_sync::Behaviour::new(chain_sync_protocol_name, chain_sync_config);

        Ok(Self {
            gossipsub,
            kademlia,
            identify,
            chain_sync,
            autonat_server,
            nat,
        })
    }
}
