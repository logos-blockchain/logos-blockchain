use core::{
    num::{NonZeroU64, NonZeroU128, NonZeroUsize},
    ops::RangeInclusive,
};
use std::{
    collections::{HashMap, VecDeque},
    iter::repeat_with,
    sync::Arc,
};

use async_trait::async_trait;
use futures::{StreamExt as _, select};
use lb_blend_membership::{Membership, Node};
use lb_blend_message::crypto::key_ext::Ed25519SecretKeyExt as _;
use lb_blend_primitives::time::{Round, RoundClock, RoundCount};
use lb_key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use lb_libp2p::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    Multiaddr, PeerId, Swarm,
    identity::{PublicKey, ed25519},
};
use libp2p_swarm_test::SwarmExt as _;

use crate::core::{
    poq_verification::PendingPoQVerifications,
    tests::utils::{PROTOCOL_NAME, TestProofsVerifier, TestSwarm},
    with_core::behaviour::{
        Behaviour, Event, liveness::PeerLivenessMap, message_cache::MessageCache,
    },
};

/// The behaviour under test, with the `PoQ` verifier the tests use.
pub type TestBehaviour = Behaviour<TestProofsVerifier>;

/// Generates `count` nodes with randomly generated identities and empty
/// addresses.
pub fn new_nodes_with_empty_address(
    count: usize,
) -> (impl Iterator<Item = ed25519::Keypair>, Vec<Node<PeerId>>) {
    let mut identities: Vec<ed25519::Keypair> = repeat_with(ed25519::Keypair::generate)
        .take(count)
        .collect();
    identities.sort_by_key(|id| PeerId::from(PublicKey::from(id.public())));

    let nodes = identities
        .iter()
        .map(|identity| Node {
            id: PublicKey::from(identity.public()).into(),
            address: Multiaddr::empty(),
            public_key: Ed25519PublicKey::from_bytes(&identity.public().to_bytes())
                .expect("must be a valid ed25519 public key"),
        })
        .collect::<Vec<_>>();

    (identities.into_iter(), nodes)
}

pub struct BehaviourBuilder {
    local_public_key: ed25519::PublicKey,
    membership: Option<Membership<PeerId>>,
    round_duration_in_seconds: Option<NonZeroU64>,
    liveness_window_in_rounds: Option<NonZeroU128>,
    peering_degree: Option<RangeInclusive<usize>>,
    minimum_network_size: Option<NonZeroUsize>,
    num_blend_layers: Option<NonZeroU64>,
    proofs_verifier: TestProofsVerifier,
}

impl BehaviourBuilder {
    pub fn new(identity: &ed25519::Keypair) -> Self {
        Self {
            local_public_key: identity.public(),
            membership: None,
            round_duration_in_seconds: None,
            liveness_window_in_rounds: None,
            peering_degree: None,
            minimum_network_size: None,
            num_blend_layers: None,
            proofs_verifier: TestProofsVerifier::accepting(),
        }
    }

    /// Makes the behaviour reject the `PoQ` of every message it receives.
    pub const fn with_rejecting_proofs_verifier(mut self) -> Self {
        self.proofs_verifier = TestProofsVerifier::rejecting();
        self
    }

    pub fn with_membership(mut self, nodes: &[Node<PeerId>]) -> Self {
        self.membership = Some(Membership::new(
            nodes,
            &Ed25519PublicKey::from_bytes(&self.local_public_key.to_bytes())
                .expect("must be a valid ed25519 public key"),
        ));
        self
    }

    pub fn with_liveness(
        mut self,
        round_duration_in_seconds: NonZeroU64,
        window_in_rounds: NonZeroU128,
    ) -> Self {
        self.round_duration_in_seconds = Some(round_duration_in_seconds);
        self.liveness_window_in_rounds = Some(window_in_rounds);
        self
    }

    pub fn with_peering_degree(mut self, peering_degree: RangeInclusive<usize>) -> Self {
        self.peering_degree = Some(peering_degree);
        self
    }

    pub fn with_minimum_network_size(mut self, minimum_network_size: usize) -> Self {
        self.minimum_network_size = Some(minimum_network_size.try_into().unwrap());
        self
    }

    pub fn with_num_blend_layers(mut self, num_blend_layers: u64) -> Self {
        self.num_blend_layers = Some(num_blend_layers.try_into().unwrap());
        self
    }

    pub fn build(self) -> TestBehaviour {
        let round_duration = self
            .round_duration_in_seconds
            .unwrap_or_else(|| 1.try_into().unwrap());
        // A window long enough that no connection in a test goes stale by accident.
        // Tests that exercise liveness set their own.
        let liveness_window = self
            .liveness_window_in_rounds
            .unwrap_or_else(|| 1_000_000.try_into().unwrap());
        Behaviour {
            negotiated_peers: HashMap::new(),
            connections_waiting_upgrade: HashMap::new(),
            events: VecDeque::new(),
            waker: None,
            current_epoch_info: (
                self.membership
                    .unwrap_or_else(|| Membership::new_without_local(&[])),
                0.into(),
            ),
            peering_degree: self.peering_degree.unwrap_or(1..=1),
            local_peer_id: PublicKey::from(self.local_public_key).into(),
            protocol_name: PROTOCOL_NAME,
            minimum_network_size: self
                .minimum_network_size
                .unwrap_or_else(|| 1usize.try_into().unwrap()),
            num_blend_layers: self
                .num_blend_layers
                .unwrap_or_else(|| 3.try_into().unwrap()),
            old_epoch: None,
            round_clock: RoundClock::new(round_duration),
            liveness: PeerLivenessMap::new(RoundCount::new(liveness_window)),
            last_liveness_check: Round::from(0),
            message_cache: MessageCache::new(),
            proofs_verifier: Arc::new(self.proofs_verifier),
            pending_poq_verifications: PendingPoQVerifications::new(),
        }
    }
}

#[async_trait]
pub trait SwarmExt: libp2p_swarm_test::SwarmExt {
    async fn connect_and_wait_for_upgrade<ListenerBehaviour>(
        &mut self,
        other: &mut Swarm<ListenerBehaviour>,
    ) where
        ListenerBehaviour: NetworkBehaviour<ToSwarm = Event> + Send;
}

#[async_trait]
impl SwarmExt for Swarm<TestBehaviour> {
    async fn connect_and_wait_for_upgrade<ListenerBehaviour>(
        &mut self,
        listener: &mut Swarm<ListenerBehaviour>,
    ) where
        ListenerBehaviour: NetworkBehaviour<ToSwarm = Event> + Send,
    {
        self.connect(listener).await;
        let mut inbound_conn_upgraded = false;
        let mut outbound_conn_upgraded = false;
        loop {
            select! {
                swarm_event = self.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::OutboundConnectionUpgradeSucceeded(peer_id)) = swarm_event && peer_id == *listener.local_peer_id() {
                        outbound_conn_upgraded = true;
                        if inbound_conn_upgraded {
                            return;
                        }
                    }
                }
                swarm_event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::InboundConnectionUpgradeSucceeded(peer_id)) = swarm_event && peer_id == *self.local_peer_id() {
                        inbound_conn_upgraded = true;
                        if outbound_conn_upgraded {
                            return;
                        }
                    }
                }
            }
        }
    }
}

pub fn build_memberships<Behaviour: NetworkBehaviour>(
    swarms: &[&TestSwarm<Behaviour>],
) -> Vec<Membership<PeerId>> {
    let nodes = swarms
        .iter()
        .map(|swarm| Node {
            id: *swarm.local_peer_id(),
            address: Multiaddr::empty(),
            public_key: UnsecuredEd25519Key::generate_with_chacha_rng().public_key(),
        })
        .collect::<Vec<_>>();
    nodes
        .iter()
        .map(|node| Membership::new(&nodes, &node.public_key))
        .collect()
}
