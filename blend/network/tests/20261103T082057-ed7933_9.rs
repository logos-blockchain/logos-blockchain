//! Proof for finding `20261103T082057-ed7933/9`.
//!
//! Blend Protocol, Network Maintenance, Expected Traffic:
//!
//! > A message is **novel** to a node when its proof of quota nullifier is not
//! > in the nullifier cache. [...] A node [...] verifies the public header of
//! > novel messages only.
//!
//! The core-to-core path looks the nullifier up before verifying anything. The
//! core-to-edge path does not: a message an edge node sends is always verified
//! (signature and `PoQ`) and reported to the swarm as a new message, even when
//! the node has already verified and processed the very same message. With the
//! edge node's redundancy, that is the ordinary case: the same message reaches
//! a core node both from the edge node and from a core neighbour.

use std::{
    num::{NonZeroU64, NonZeroU128, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{AsyncWriteExt as _, StreamExt as _};
use lb_blend_membership::{Membership, Node};
use lb_blend_message::{
    PaddedPayloadBody, PayloadType,
    crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
    encap::{ProofsVerifier, validated::EncapsulatedMessageWithVerifiedPublicHeader},
    input::EncapsulationInput,
    serialize_encapsulated_message_with_verified_public_header,
};
use lb_blend_primitives::time::RoundCount;
use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use libp2p::{
    Multiaddr, PeerId, Stream, StreamProtocol, Swarm, Transport as _,
    core::{transport::MemoryTransport, upgrade::Version},
    identity::{PublicKey, ed25519},
    plaintext,
    swarm::{self, NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use libp2p_swarm_test::SwarmExt as _;
use logos_blockchain_blend_network::core::{
    CommonConfig, Config, NetworkBehaviour as BlendNetworkBehaviour, NetworkBehaviourEvent,
    with_core::behaviour::{Config as CoreConfig, Event as CoreEvent},
    with_edge::behaviour::{Config as EdgeConfig, Event as EdgeEvent},
};
use tokio::{select, time::timeout};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-9");
const NUM_BLEND_LAYERS: u64 = 3;

/// Accepts every proof, and counts the proofs of quota it is asked to verify.
#[derive(Debug, Clone, Default)]
struct CountingVerifier {
    proofs_of_quota_verified: Arc<AtomicUsize>,
}

impl ProofsVerifier for CountingVerifier {
    type Error = ();

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self::default()
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        self.proofs_of_quota_verified.fetch_add(1, Ordering::SeqCst);
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

fn message() -> EncapsulatedMessageWithVerifiedPublicHeader {
    let inputs = (0..NUM_BLEND_LAYERS)
        .map(|_| {
            let mut quota = [0u8; PROOF_OF_QUOTA_SIZE];
            quota[0] = 7;
            EncapsulationInput::new(
                UnsecuredEd25519Key::generate_with_chacha_rng(),
                &UnsecuredEd25519Key::generate_with_chacha_rng().public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked(quota),
                VerifiedProofOfSelection::from_bytes_unchecked([0u8; _]),
            )
        })
        .collect::<Vec<_>>();
    EncapsulatedMessageWithVerifiedPublicHeader::try_new(
        &inputs,
        PayloadType::BlockProposal,
        PaddedPayloadBody::try_from(&b"an edge node's message, sent redundantly"[..]).unwrap(),
        usize::try_from(NUM_BLEND_LAYERS).unwrap(),
    )
    .unwrap()
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

type Core = Swarm<BlendNetworkBehaviour<CountingVerifier>>;
type RawPeer = Swarm<libp2p_stream::Behaviour>;

async fn open_blend_stream(core: &mut Core, peer: &mut RawPeer) -> Stream {
    let mut control = peer.behaviour_mut().new_control();
    peer.connect(core).await;
    let core_peer_id = *core.local_peer_id();
    let mut open = Box::pin(control.open_stream(core_peer_id, PROTOCOL));
    loop {
        select! {
            opened = &mut open => break opened.unwrap(),
            _ = peer.select_next_some() => {}
            _ = core.select_next_some() => {}
        }
    }
}

/// Drives both swarms for up to five seconds, collecting what the core node
/// reports.
async fn reports(core: &mut Core, peer: &mut RawPeer, until_first: bool) -> Vec<String> {
    let mut seen = Vec::new();
    let _: Result<(), _> = timeout(Duration::from_secs(5), async {
        loop {
            select! {
                _ = peer.select_next_some() => {}
                event = core.select_next_some() => match event {
                    SwarmEvent::Behaviour(NetworkBehaviourEvent::WithCore(CoreEvent::Message { .. })) => {
                        seen.push("message from a core neighbour".to_owned());
                        if until_first { return; }
                    }
                    SwarmEvent::Behaviour(NetworkBehaviourEvent::WithEdge(EdgeEvent::Message { .. })) => {
                        seen.push("message from an edge node".to_owned());
                        if until_first { return; }
                    }
                    _ => {}
                },
            }
        }
    })
    .await;
    seen
}

#[tokio::test]
async fn a_message_already_processed_is_not_verified_again_when_an_edge_node_sends_it() {
    let core_identity = ed25519::Keypair::generate();
    let neighbour_identity = ed25519::Keypair::generate();
    let nodes = [node(&core_identity), node(&neighbour_identity)];
    let membership = Membership::new(&nodes, &signing_key(&core_identity));
    let verifier = CountingVerifier::default();
    let verified = Arc::clone(&verifier.proofs_of_quota_verified);

    let mut core = swarm(&core_identity, || {
        BlendNetworkBehaviour::new(
            &config(),
            (membership, Epoch::from(0)),
            verifier,
            peer_id(&core_identity),
            PROTOCOL,
        )
    });
    core.listen().with_memory_addr_external().await;
    let bytes = serialize_encapsulated_message_with_verified_public_header(&message());

    // The message arrives first from a core neighbour: it is novel, so its
    // header is verified and its nullifier enters the cache.
    let mut neighbour = swarm(&neighbour_identity, libp2p_stream::Behaviour::new);
    let mut neighbour_control = neighbour.behaviour_mut().new_control();
    let _incoming = neighbour_control.accept(PROTOCOL).unwrap();
    let mut stream = open_blend_stream(&mut core, &mut neighbour).await;
    stream.write_all(&bytes).await.unwrap();
    stream.flush().await.unwrap();
    assert_eq!(
        reports(&mut core, &mut neighbour, true).await,
        vec!["message from a core neighbour".to_owned()],
        "control: the novel message is accepted from the core neighbour"
    );
    assert_eq!(verified.load(Ordering::SeqCst), 1, "control: verified once");

    // The edge node that created it sends the same message to this node too.
    let mut edge = swarm(&ed25519::Keypair::generate(), libp2p_stream::Behaviour::new);
    let mut stream = open_blend_stream(&mut core, &mut edge).await;
    stream.write_all(&bytes).await.unwrap();
    stream.flush().await.unwrap();
    let after_edge = reports(&mut core, &mut edge, false).await;

    assert_eq!(
        verified.load(Ordering::SeqCst),
        1,
        "the message is no longer novel (its nullifier is in the cache), so its public header \
         must not be verified again; instead it was verified a second time and reported as {after_edge:?}"
    );
}
