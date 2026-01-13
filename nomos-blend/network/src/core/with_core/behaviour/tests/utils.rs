use core::{num::NonZeroUsize, ops::RangeInclusive, time::Duration};
use std::{
    collections::{HashMap, VecDeque},
    iter::repeat_with,
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, select};
use groth16::Field as _;
use key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use libp2p::{
    Multiaddr, PeerId, Swarm,
    identity::{PublicKey, ed25519},
};
use libp2p_swarm_test::SwarmExt as _;
use nomos_blend_message::{
    crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
    encap,
};
use nomos_blend_proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs};
use nomos_blend_scheduling::membership::{Membership, Node};
use nomos_core::{crypto::ZkHash, sdp::SessionNumber};
use nomos_libp2p::{NetworkBehaviour, SwarmEvent};
use tokio::time::interval;
use tokio_stream::wrappers::IntervalStream;

use crate::core::{
    tests::utils::{PROTOCOL_NAME, TestSwarm},
    with_core::behaviour::{Behaviour, Event, IntervalStreamProvider},
};

#[derive(Clone)]
pub struct IntervalProvider(Duration, RangeInclusive<u64>);

impl IntervalStreamProvider for IntervalProvider {
    type IntervalStream = Box<dyn Stream<Item = RangeInclusive<u64>> + Send + Unpin + 'static>;
    type IntervalItem = RangeInclusive<u64>;

    fn interval_stream(&self) -> Self::IntervalStream {
        let range = self.1.clone();
        Box::new(IntervalStream::new(interval(self.0)).map(move |_| range.clone()))
    }
}

#[derive(Default)]
pub struct IntervalProviderBuilder {
    range: Option<RangeInclusive<u64>>,
}

impl IntervalProviderBuilder {
    pub fn with_range(mut self, range: RangeInclusive<u64>) -> Self {
        self.range = Some(range);
        self
    }

    pub fn build(self) -> IntervalProvider {
        IntervalProvider(Duration::from_secs(1), self.range.unwrap_or(0..=1))
    }
}

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
    provider: Option<IntervalProvider>,
    peering_degree: Option<RangeInclusive<usize>>,
    minimum_network_size: Option<NonZeroUsize>,
    poq_verification_inputs: Option<PoQVerificationInputsMinusSigningKey>,
}

impl BehaviourBuilder {
    pub fn new(identity: &ed25519::Keypair) -> Self {
        Self {
            local_public_key: identity.public(),
            membership: None,
            provider: None,
            peering_degree: None,
            minimum_network_size: None,
            poq_verification_inputs: None,
        }
    }

    pub fn with_membership(mut self, nodes: &[Node<PeerId>]) -> Self {
        self.membership = Some(Membership::new(
            nodes,
            &Ed25519PublicKey::from_bytes(&self.local_public_key.to_bytes())
                .expect("must be a valid ed25519 public key"),
        ));
        self
    }

    pub fn with_provider(mut self, provider: IntervalProvider) -> Self {
        self.provider = Some(provider);
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

    pub fn with_poq_verification_inputs(
        mut self,
        poq_verification_inputs: PoQVerificationInputsMinusSigningKey,
    ) -> Self {
        assert!(
            self.poq_verification_inputs.is_none(),
            "poq_verification_inputs already set."
        );
        self.poq_verification_inputs = Some(poq_verification_inputs);
        self
    }

    pub fn build<ProofsVerifier>(self) -> Behaviour<ProofsVerifier, IntervalProvider>
    where
        ProofsVerifier: encap::ProofsVerifier,
    {
        Behaviour {
            negotiated_peers: HashMap::new(),
            connections_waiting_upgrade: HashMap::new(),
            events: VecDeque::new(),
            waker: None,
            exchanged_message_identifiers: HashMap::new(),
            observation_window_clock_provider: self
                .provider
                .unwrap_or_else(|| IntervalProviderBuilder::default().build()),
            current_membership: self
                .membership
                .unwrap_or_else(|| Membership::new_without_local(&[])),
            peering_degree: self.peering_degree.unwrap_or(1..=1),
            local_peer_id: PublicKey::from(self.local_public_key).into(),
            protocol_name: PROTOCOL_NAME,
            minimum_network_size: self
                .minimum_network_size
                .unwrap_or_else(|| 1usize.try_into().unwrap()),
            old_session: None,
            poq_verifier: ProofsVerifier::new(
                self.poq_verification_inputs
                    .unwrap_or_else(|| default_poq_verification_inputs_for_session(0)),
            ),
        }
    }
}

pub fn default_poq_verification_inputs_for_session(
    session: SessionNumber,
) -> PoQVerificationInputsMinusSigningKey {
    PoQVerificationInputsMinusSigningKey {
        session,
        core: CoreInputs {
            zk_root: ZkHash::ZERO,
            quota: 0,
        },
        leader: LeaderInputs {
            pol_ledger_aged: ZkHash::ZERO,
            pol_epoch_nonce: ZkHash::ZERO,
            message_quota: 0,
            total_stake: 0,
        },
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
impl<ProofsVerifier> SwarmExt for Swarm<Behaviour<ProofsVerifier, IntervalProvider>>
where
    ProofsVerifier: encap::ProofsVerifier + Send + 'static,
{
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
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })
        .collect::<Vec<_>>();
    nodes
        .iter()
        .map(|node| Membership::new(&nodes, &node.public_key))
        .collect()
}
