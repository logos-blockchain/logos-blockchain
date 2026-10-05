#![allow(
    clippy::multiple_inherent_impl,
    reason = "We split the `Behaviour` impls into different modules for better code modularity."
)]

use std::{collections::HashMap, error::Error};

use lb_cryptarchia_sync::ChainSyncError;
use lb_utils::net::MAX_WIRE_MESSAGE_SIZE;
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

pub(crate) struct BehaviourConfig {
    pub gossipsub_config: libp2p::gossipsub::Config,
    pub kademlia_config: KademliaSettings,
    pub identify_config: IdentifySettings,
    pub nat_config: NatSettings,
    pub kad_protocol_name: StreamProtocol,
    pub identify_protocol_name: StreamProtocol,
    pub chain_sync_protocol_name: StreamProtocol,
    pub public_key: identity::PublicKey,
    pub chain_sync_config: lb_cryptarchia_sync::Config,
    pub max_data_size_by_topic: HashMap<libp2p::gossipsub::TopicHash, usize>,
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
    pub(crate) chain_sync: lb_cryptarchia_sync::Behaviour,
    // The spec makes it mandatory to run an autonat server for a public node.
    pub(crate) autonat_server: autonat::v2::server::Behaviour<Rng>,
    pub(crate) nat: nat::Behaviour<Rng>,
}

impl<Rng: Clone + Send + RngCore + 'static> Behaviour<Rng> {
    pub(crate) fn new(
        config: BehaviourConfig,
        rng: Rng,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
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
            max_data_size_by_topic,
        } = config;

        let peer_id = PeerId::from(public_key.clone());
        let gossipsub_config = gossipsub::configure_topic_size_limits(
            gossipsub_config,
            peer_id,
            max_data_size_by_topic,
        )?;

        let gossipsub = libp2p::gossipsub::Behaviour::new(
            libp2p::gossipsub::MessageAuthenticity::Author(peer_id),
            libp2p::gossipsub::ConfigBuilder::from(gossipsub_config)
                .validation_mode(libp2p::gossipsub::ValidationMode::None)
                .message_id_fn(compute_message_id)
                // This is only the fallback for topics without an explicit
                // application-data limit; known topics retain their overrides.
                .max_transmit_size(MAX_WIRE_MESSAGE_SIZE)
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
            lb_cryptarchia_sync::Behaviour::new(chain_sync_protocol_name, chain_sync_config);

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

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, time::Duration};

    use lb_cryptarchia_sync::Config as ChainSyncConfig;
    use libp2p::gossipsub as libp2p_gossipsub;
    use rand::rngs::OsRng;

    use super::*;
    use crate::behaviour::gossipsub::configure_topic_size_limits;

    #[tokio::test]
    async fn final_behaviour_construction_preserves_topic_size_limits() {
        let node_key = identity::ed25519::SecretKey::generate();
        let keypair = identity::Keypair::from(identity::ed25519::Keypair::from(node_key));
        let public_key = keypair.public();
        let author = PeerId::from(public_key.clone());
        let topic = "test";
        let payload_size = 512;
        let topic_hash = libp2p_gossipsub::IdentTopic::new(topic).hash();
        let gossipsub_config = configure_topic_size_limits(
            libp2p_gossipsub::Config::default(),
            author,
            HashMap::from([(topic_hash.clone(), payload_size)]),
        )
        .unwrap();
        let topic_limit = gossipsub_config.max_transmit_size_for_topic(&topic_hash);

        let mut behaviour = Behaviour::<OsRng>::new(
            BehaviourConfig {
                gossipsub_config,
                kademlia_config: KademliaSettings::default(),
                identify_config: IdentifySettings::default(),
                nat_config: NatSettings::default(),
                kad_protocol_name: StreamProtocol::new("/test/kad/1.0.0"),
                identify_protocol_name: StreamProtocol::new("/test/identify/1.0.0"),
                chain_sync_protocol_name: StreamProtocol::new("/test/chainsync/1.0.0"),
                public_key,
                chain_sync_config: ChainSyncConfig {
                    peer_response_timeout: Duration::from_secs(1),
                    max_inbound_requests: NonZeroUsize::new(1).unwrap(),
                },
                max_data_size_by_topic: HashMap::new(),
            },
            OsRng,
        )
        .unwrap();

        assert!(matches!(
            behaviour.gossipsub.publish(
                libp2p_gossipsub::IdentTopic::new(topic),
                vec![0; topic_limit + 1]
            ),
            Err(libp2p_gossipsub::PublishError::MessageTooLarge)
        ));
    }
}
