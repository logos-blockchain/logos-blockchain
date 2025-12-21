use core::{ops::RangeInclusive, time::Duration};
use std::iter::repeat_with;

use async_trait::async_trait;
use futures::StreamExt as _;
use key_management_system_service::keys::UnsecuredEd25519Key;
use libp2p::{
    Multiaddr, PeerId, Swarm, allow_block_list, connection_limits, core::transport::ListenerId,
    identity::Keypair,
};
use libp2p_swarm_test::SwarmExt as _;
use nomos_blend::{
    message::{
        crypto::key_ext::Ed25519SecretKeyExt as _,
        encap::{
            ProofsVerifier as ProofsVerifierTrait,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
        },
    },
    network::core::{
        Config, NetworkBehaviour,
        with_core::behaviour::{Config as CoreToCoreConfig, IntervalStreamProvider},
        with_edge::behaviour::Config as CoreToEdgeConfig,
    },
    scheduling::membership::{Membership, Node},
};
use nomos_libp2p::{Protocol, SwarmEvent};
use nomos_utils::blake_rng::BlakeRng;
use rand::SeedableRng as _;
use tokio::{
    sync::{broadcast, mpsc},
    time::interval,
};
use tokio_stream::wrappers::IntervalStream;

use crate::{
    core::{
        backends::{
            PublicInfo,
            libp2p::{BlendSwarm, behaviour::BlendBehaviour, swarm::BlendSwarmMessage},
        },
        settings::BlendConfig,
    },
    test_utils::{PROTOCOL_NAME, crypto::MockProofsVerifier},
};

pub type InnerSwarm<ProofsVerifier> =
    BlendSwarm<BlakeRng, ProofsVerifier, TestObservationWindowProvider>;

pub struct TestSwarm<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + 'static,
{
    pub swarm: InnerSwarm<ProofsVerifier>,
    pub swarm_message_sender: mpsc::Sender<BlendSwarmMessage>,
    pub incoming_message_receiver: broadcast::Receiver<EncapsulatedMessageWithVerifiedPublicHeader>,
}

/// Generates `count` nodes with randomly generated identities and empty
/// addresses.
pub fn new_nodes_with_empty_address(
    count: usize,
) -> (impl Iterator<Item = Keypair>, Vec<Node<PeerId>>) {
    let ids: Vec<Keypair> = repeat_with(Keypair::generate_ed25519).take(count).collect();
    let nodes = ids
        .iter()
        .map(|identity| Node {
            id: identity.public().into(),
            address: Multiaddr::empty(),
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })
        .collect::<Vec<_>>();
    (ids.into_iter(), nodes)
}

/// Updates the address of the node with the given `peer_id`.
pub fn update_nodes(nodes: &mut [Node<PeerId>], peer_id: &PeerId, addr: Multiaddr) {
    for node in nodes.iter_mut() {
        if &node.id == peer_id {
            node.address = addr;
            break;
        }
    }
}

pub fn build_membership(
    nodes: &[Node<PeerId>],
    local_peer_id: Option<PeerId>,
) -> Membership<PeerId> {
    nodes
        .iter()
        .find(|node| Some(node.id) == local_peer_id)
        .map(|node| node.public_key)
        .map_or_else(
            || Membership::new_without_local(nodes),
            |local_public_key| Membership::new(nodes, &local_public_key),
        )
}

pub struct SwarmBuilder {
    identity: Keypair,
    public_info: PublicInfo<PeerId>,
}

impl SwarmBuilder {
    pub fn new(identity: Keypair, membership: &[Node<PeerId>]) -> Self {
        let public_info = build_membership(membership, Some(identity.public().into())).into();
        Self {
            identity,
            public_info,
        }
    }

    pub fn build<BehaviourConstructor, ProofsVerifier>(
        self,
        behaviour_constructor: BehaviourConstructor,
    ) -> TestSwarm<ProofsVerifier>
    where
        BehaviourConstructor:
            FnOnce(
                PeerId,
                Membership<PeerId>,
            ) -> BlendBehaviour<ProofsVerifier, TestObservationWindowProvider>,
        ProofsVerifier: ProofsVerifierTrait,
    {
        let (swarm_message_sender, swarm_message_receiver) = mpsc::channel(100);
        let (incoming_message_sender, incoming_message_receiver) = broadcast::channel(100);

        let swarm = BlendSwarm::new_test(
            &self.identity,
            behaviour_constructor,
            swarm_message_receiver,
            incoming_message_sender,
            self.public_info,
            BlakeRng::from_entropy(),
            3u64.try_into().unwrap(),
            1usize.try_into().unwrap(),
        );

        TestSwarm {
            swarm,
            swarm_message_sender,
            incoming_message_receiver,
        }
    }
}

pub struct BlendBehaviourBuilder<ProofsVerifier> {
    peer_id: PeerId,
    proofs_verifier: ProofsVerifier,
    membership: Membership<PeerId>,
    observation_window: Option<(Duration, RangeInclusive<u64>)>,
}

impl<ProofsVerifier> BlendBehaviourBuilder<ProofsVerifier> {
    pub fn new(
        peer_id: PeerId,
        proofs_verifier: ProofsVerifier,
        membership: Membership<PeerId>,
    ) -> Self {
        Self {
            peer_id,
            proofs_verifier,
            membership,
            observation_window: None,
        }
    }

    pub fn with_observation_window(
        mut self,
        round_duration: Duration,
        expected_message_range: RangeInclusive<u64>,
    ) -> Self {
        self.observation_window = Some((round_duration, expected_message_range));
        self
    }
}

impl<ProofsVerifier> BlendBehaviourBuilder<ProofsVerifier>
where
    ProofsVerifier: Clone,
{
    pub fn build(self) -> BlendBehaviour<ProofsVerifier, TestObservationWindowProvider> {
        let observation_window_values = self
            .observation_window
            .unwrap_or((Duration::from_secs(1), u64::MIN..=u64::MAX));

        BlendBehaviour {
            blend: NetworkBehaviour::new(
                &Config {
                    with_core: CoreToCoreConfig {
                        peering_degree: 1..=100,
                        minimum_network_size: 1.try_into().unwrap(),
                    },
                    with_edge: CoreToEdgeConfig {
                        connection_timeout: Duration::from_secs(1),
                        max_incoming_connections: 300,
                        minimum_network_size: 1.try_into().unwrap(),
                    },
                },
                TestObservationWindowProvider {
                    expected_message_range: observation_window_values.1,
                    interval: observation_window_values.0,
                },
                self.membership,
                self.peer_id,
                PROTOCOL_NAME,
                self.proofs_verifier,
            ),
            limits: connection_limits::Behaviour::new(
                connection_limits::ConnectionLimits::default(),
            ),
            blocked_peers: allow_block_list::Behaviour::default(),
        }
    }
}

pub struct TestObservationWindowProvider {
    interval: Duration,
    expected_message_range: RangeInclusive<u64>,
}

#[expect(
    clippy::fallible_impl_from,
    reason = "We need this `From` impl to fulfill the behaviour requirements, but for tests we are actually expect it not to use it."
)]
impl<Settings> From<&BlendConfig<Settings>> for TestObservationWindowProvider {
    fn from(_: &BlendConfig<Settings>) -> Self {
        panic!(
            "This function should never be called in tests since we are hard-coding expected values for the test observation window provider."
        );
    }
}

impl IntervalStreamProvider for TestObservationWindowProvider {
    type IntervalStream =
        Box<dyn futures::Stream<Item = RangeInclusive<u64>> + Send + Unpin + 'static>;
    type IntervalItem = RangeInclusive<u64>;

    fn interval_stream(&self) -> Self::IntervalStream {
        let expected_message_range = self.expected_message_range.clone();
        Box::new(
            IntervalStream::new(interval(self.interval))
                .map(move |_| expected_message_range.clone()),
        )
    }
}

#[async_trait]
pub trait SwarmExt: libp2p_swarm_test::SwarmExt {
    async fn listen_and_return_membership_entry(
        &mut self,
        addr: Option<Multiaddr>,
    ) -> (Node<PeerId>, ListenerId);
}

#[async_trait]
impl SwarmExt for Swarm<BlendBehaviour<MockProofsVerifier, TestObservationWindowProvider>> {
    async fn listen_and_return_membership_entry(
        &mut self,
        addr: Option<Multiaddr>,
    ) -> (Node<PeerId>, ListenerId) {
        let memory_addr_listener_id = self
            .listen_on(addr.unwrap_or_else(|| Protocol::Memory(0).into()))
            .unwrap();

        // block until we are actually listening
        let address = self
            .wait(|event| match event {
                SwarmEvent::NewListenAddr {
                    address,
                    listener_id,
                } => (listener_id == memory_addr_listener_id).then_some(address),
                other => {
                    panic!("Unexpected event while waiting for `NewListenAddr`: {other:?}")
                }
            })
            .await;
        (
            Node {
                address,
                id: *self.local_peer_id(),
                public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
            },
            memory_addr_listener_id,
        )
    }
}
