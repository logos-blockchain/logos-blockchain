//! Proof for finding `20261103T082057-ed7933/3`.
//!
//! Blend Protocol, Network Maintenance, Connectivity Maintenance, Blacklist:
//!
//! > A failure of the authenticated stream is a violation of the framing of
//! > the stream.
//! >
//! > 1. A connection with a core node whose authenticated stream fails, or
//! >    that carries a message with a malformed header, an invalid signature,
//! >    or an invalid proof of quota, is closed and its neighbor is added to
//! >    the blacklist.
//!
//! Every Blend message has the same fixed size, so a stream that its sender
//! ends part way through a message violates the framing. The core behaviour
//! reads that as the stream simply being over: it drops the substreams and
//! never blacklists the neighbour.

use std::{
    num::{NonZeroU64, NonZeroU128, NonZeroUsize},
    time::Duration,
};

use futures::{AsyncWriteExt as _, StreamExt as _};
use lb_blend_membership::{Membership, Node};
use lb_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    encap::{ProofsVerifier, encapsulated_message_encoded_size},
};
use lb_blend_primitives::time::RoundCount;
use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use libp2p::{
    Multiaddr, PeerId, Stream, StreamProtocol, Swarm, Transport as _,
    core::{transport::MemoryTransport, upgrade::Version},
    identity::{PublicKey, ed25519},
    plaintext,
    swarm::{self, NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use libp2p_stream::IncomingStreams;
use libp2p_swarm_test::SwarmExt as _;
use logos_blockchain_blend_network::core::{
    CommonConfig, Config, NetworkBehaviour as BlendNetworkBehaviour, NetworkBehaviourEvent,
    with_core::behaviour::{Config as CoreConfig, Event as CoreEvent},
    with_edge::behaviour::Config as EdgeConfig,
};
use tokio::{select, time::timeout};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-3");
const NUM_BLEND_LAYERS: u64 = 3;

#[derive(Debug, Clone, Copy)]
struct AcceptingVerifier;

impl ProofsVerifier for AcceptingVerifier {
    type Error = ();

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
            proof,
        ))
    }
}

fn peer_id(identity: &ed25519::Keypair) -> PeerId {
    PublicKey::from(identity.public()).into()
}

fn signing_key(identity: &ed25519::Keypair) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(&identity.public().to_bytes()).unwrap()
}

fn node(identity: &ed25519::Keypair) -> Node<PeerId> {
    Node {
        id: peer_id(identity),
        address: Multiaddr::empty(),
        public_key: signing_key(identity),
    }
}

fn config() -> Config {
    Config {
        common: CommonConfig {
            round_duration_in_seconds: NonZeroU64::new(1).unwrap(),
            minimum_network_size: NonZeroUsize::new(1).unwrap(),
            num_blend_layers: NonZeroU64::new(NUM_BLEND_LAYERS).unwrap(),
        },
        with_core: CoreConfig {
            target_peering_degree: NonZeroUsize::new(4).unwrap(),
            liveness_window_in_rounds: NonZeroU128::new(1_000).unwrap(),
            connection_share_per_round: NonZeroU64::new(100).unwrap(),
            send_deadline_in_rounds: RoundCount::new(NonZeroU128::new(2).unwrap()),
            handshake_deadline_in_rounds: RoundCount::new(NonZeroU128::new(30).unwrap()),
        },
        with_edge: EdgeConfig {
            connection_timeout: Duration::from_secs(10),
            max_incoming_connections: 10,
            accepted_connections_per_round: NonZeroU64::new(10).unwrap(),
        },
    }
}

fn swarm<Behaviour>(
    identity: &ed25519::Keypair,
    behaviour: impl FnOnce() -> Behaviour,
) -> Swarm<Behaviour>
where
    Behaviour: NetworkBehaviour,
{
    // TCP alongside memory, as `libp2p_swarm_test` listens on both.
    let transport = MemoryTransport::default()
        .or_transport(tcp::tokio::Transport::default())
        .upgrade(Version::V1)
        .authenticate(plaintext::Config::new(&identity.clone().into()))
        .multiplex(yamux::Config::default())
        .timeout(Duration::from_secs(20))
        .boxed();
    Swarm::new(
        transport,
        behaviour(),
        peer_id(identity),
        swarm::Config::with_tokio_executor().with_idle_connection_timeout(Duration::ZERO),
    )
}

type Core = Swarm<BlendNetworkBehaviour<AcceptingVerifier>>;
type Neighbour = Swarm<libp2p_stream::Behaviour>;

/// Connects a core neighbour that speaks raw bytes to the core node and hands
/// back the Blend stream it opened. The returned `IncomingStreams` keeps the
/// core node's own substream towards the neighbour accepted.
async fn open_blend_stream(core: &mut Core, neighbour: &mut Neighbour) -> (Stream, IncomingStreams) {
    let mut control = neighbour.behaviour_mut().new_control();
    let incoming = control.accept(PROTOCOL).unwrap();
    neighbour.connect(core).await;
    let core_peer_id = *core.local_peer_id();
    let mut open = Box::pin(control.open_stream(core_peer_id, PROTOCOL));
    let stream = loop {
        select! {
            opened = &mut open => break opened.unwrap(),
            _ = neighbour.select_next_some() => {}
            _ = core.select_next_some() => {}
        }
    };
    (stream, incoming)
}

/// Drives both swarms for up to five seconds and reports whether the core node
/// blacklisted the neighbour, and why.
async fn blacklisting_of(core: &mut Core, neighbour: &mut Neighbour) -> Option<String> {
    let neighbour_peer_id = *neighbour.local_peer_id();
    timeout(Duration::from_secs(5), async {
        loop {
            select! {
                _ = neighbour.select_next_some() => {}
                event = core.select_next_some() => {
                    if let SwarmEvent::Behaviour(NetworkBehaviourEvent::WithCore(CoreEvent::PeerBlacklisted { peer, reason })) = event
                        && peer == neighbour_peer_id
                    {
                        return format!("{reason:?}");
                    }
                }
            }
        }
    })
    .await
    .ok()
}

#[tokio::test]
async fn a_stream_that_breaks_the_framing_gets_its_neighbour_blacklisted() {
    let core_identity = ed25519::Keypair::generate();
    let control_identity = ed25519::Keypair::generate();
    let offender_identity = ed25519::Keypair::generate();
    let nodes = [
        node(&core_identity),
        node(&control_identity),
        node(&offender_identity),
    ];
    let membership = Membership::new(&nodes, &signing_key(&core_identity));
    let mut core = swarm(&core_identity, || {
        BlendNetworkBehaviour::new(
            &config(),
            (membership, Epoch::from(0)),
            AcceptingVerifier,
            peer_id(&core_identity),
            PROTOCOL,
        )
    });
    core.listen().with_memory_addr_external().await;
    let message_size =
        encapsulated_message_encoded_size(NonZeroU64::new(NUM_BLEND_LAYERS).unwrap()).get();

    // Control: a whole frame whose header is malformed gets its sender
    // blacklisted, so the blacklist and the way it is observed here both work.
    let mut control_neighbour = swarm(&control_identity, libp2p_stream::Behaviour::new);
    let (mut stream, _incoming) = open_blend_stream(&mut core, &mut control_neighbour).await;
    stream.write_all(&vec![0xAB; message_size]).await.unwrap();
    stream.flush().await.unwrap();
    assert_eq!(
        blacklisting_of(&mut core, &mut control_neighbour).await,
        Some("UndeserializableMessage".to_owned()),
        "control: a malformed header must get the neighbour blacklisted"
    );

    // The offender sends the first bytes of a frame and then ends its stream:
    // with every message the same fixed size, the framing is violated.
    let mut offender = swarm(&offender_identity, libp2p_stream::Behaviour::new);
    let (mut stream, _incoming) = open_blend_stream(&mut core, &mut offender).await;
    stream
        .write_all(&vec![0xCD; message_size / 2])
        .await
        .unwrap();
    stream.close().await.unwrap();

    assert!(
        blacklisting_of(&mut core, &mut offender).await.is_some(),
        "a core neighbour whose stream fails part way through a {message_size}-byte frame \
         violated the framing of the stream, and must be blacklisted, not just disconnected"
    );
}
