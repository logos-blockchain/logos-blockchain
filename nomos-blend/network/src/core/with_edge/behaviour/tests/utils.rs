use core::{num::NonZeroUsize, time::Duration};
use std::collections::{HashSet, VecDeque};

use async_trait::async_trait;
use key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
use libp2p::{Multiaddr, PeerId, Stream, Swarm};
use libp2p_stream::Behaviour as StreamBehaviour;
use libp2p_swarm_test::SwarmExt as _;
use nomos_blend_message::encap;
use nomos_blend_scheduling::membership::{Membership, Node};

use crate::core::{
    tests::utils::{AlwaysTrueVerifier, PROTOCOL_NAME},
    with_edge::behaviour::Behaviour,
};

pub struct BehaviourBuilder {
    core_peer_ids: Vec<PeerId>,
    max_incoming_connections: Option<usize>,
    timeout: Option<Duration>,
    minimum_network_size: Option<NonZeroUsize>,
}

impl BehaviourBuilder {
    pub fn new(core_peer_id: PeerId) -> Self {
        Self {
            core_peer_ids: vec![core_peer_id],
            max_incoming_connections: None,
            timeout: None,
            minimum_network_size: None,
        }
    }

    pub fn with_max_incoming_connections(mut self, max_incoming_connections: usize) -> Self {
        self.max_incoming_connections = Some(max_incoming_connections);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_minimum_network_size(mut self, minimum_network_size: usize) -> Self {
        self.minimum_network_size = Some(minimum_network_size.try_into().unwrap());
        self
    }

    pub fn build(self) -> Behaviour<AlwaysTrueVerifier> {
        let current_membership = Membership::new_without_local(
            self.core_peer_ids
                .into_iter()
                .map(|edge_peer_id| Node {
                    address: Multiaddr::empty(),
                    id: edge_peer_id,
                    public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
                        .unwrap(),
                })
                .collect::<Vec<_>>()
                .as_ref(),
        );
        Behaviour {
            events: VecDeque::new(),
            waker: None,
            current_membership,
            connection_timeout: self.timeout.unwrap_or(Duration::from_secs(1)),
            upgraded_edge_peers: HashSet::new(),
            max_incoming_connections: self.max_incoming_connections.unwrap_or(100),
            protocol_name: PROTOCOL_NAME,
            minimum_network_size: self
                .minimum_network_size
                .unwrap_or_else(|| 1usize.try_into().unwrap()),
            current_session_poq_verifier: AlwaysTrueVerifier,
            previous_session_poq_verifier: None,
        }
    }
}

#[async_trait]
pub trait StreamBehaviourExt<ProofsVerifier>: libp2p_swarm_test::SwarmExt
where
    ProofsVerifier: encap::ProofsVerifier + 'static,
{
    async fn connect_and_upgrade_to_blend(
        &mut self,
        other: &mut Swarm<Behaviour<ProofsVerifier>>,
    ) -> Stream;
}

#[async_trait]
impl<ProofsVerifier> StreamBehaviourExt<ProofsVerifier> for Swarm<StreamBehaviour>
where
    ProofsVerifier: encap::ProofsVerifier + Send + 'static,
{
    async fn connect_and_upgrade_to_blend(
        &mut self,
        other: &mut Swarm<Behaviour<ProofsVerifier>>,
    ) -> Stream {
        // We connect and return the stream preventing it from being dropped so the
        // blend node does not close the connection with an EOF error.
        self.connect(other).await;
        self.behaviour_mut()
            .new_control()
            .open_stream(*other.local_peer_id(), PROTOCOL_NAME)
            .await
            .unwrap()
    }
}
