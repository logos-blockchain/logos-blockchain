//! Proof for 20261103T082057-ed7933/11: skipped round ticks undercount liveness.
use std::{
    num::{NonZeroU64, NonZeroU128, NonZeroUsize},
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
use logos_blockchain_blend_network::core::{
    CommonConfig, Config, NetworkBehaviour as BlendNetworkBehaviour, NetworkBehaviourEvent,
    with_core::behaviour::{Config as CoreConfig, Event as CoreEvent},
    with_edge::behaviour::Config as EdgeConfig,
};
use lb_blend_primitives::time::RoundCount;
use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, Transport as _,
    core::{transport::MemoryTransport, upgrade::Version},
    identity::{PublicKey, ed25519},
    plaintext,
    swarm::{self, NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use libp2p_swarm_test::SwarmExt as _;
use tokio::{select, time::timeout};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-11");
const NUM_BLEND_LAYERS: u64 = 3;

/// A `PoQ` verifier bound to one epoch's public inputs, as the real one is:
/// it accepts exactly the proofs generated for its own epoch. The epoch a test
/// proof belongs to is written into the first byte of its key nullifier.
#[derive(Debug, Clone, Copy)]
struct EpochBoundVerifier {
    epoch: u8,
}

impl EpochBoundVerifier {
    const fn for_epoch(epoch: u8) -> Self {
        Self { epoch }
    }
}

impl ProofsVerifier for EpochBoundVerifier {
    type Error = ();

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self::for_epoch(0)
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let bytes = <[u8; PROOF_OF_QUOTA_SIZE]>::from(&proof);
        (bytes[0] == self.epoch)
            .then(|| VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
            .ok_or(())
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

/// A message every layer of which carries a proof generated for `epoch`.
fn message_for_epoch(epoch: u8, nonce: u8) -> EncapsulatedMessageWithVerifiedPublicHeader {
    let inputs = (0..NUM_BLEND_LAYERS)
        .map(|_| {
            let mut quota = [0u8; PROOF_OF_QUOTA_SIZE];
            quota[0] = epoch;
            quota[8] = nonce;
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
        PaddedPayloadBody::try_from(&b"a payload still crossing the network"[..]).unwrap(),
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
            // Long enough that nothing goes stale while the test runs.
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

/// A swarm over the in-memory transport, configured like the Blend core swarm:
/// a connection goes away as soon as nothing holds a stream on it.
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

#[tokio::test]
async fn unpolled_connected_rounds_count_towards_liveness() {
    let core_identity = ed25519::Keypair::generate();
    let neighbour_identity = ed25519::Keypair::generate();
    let membership = Membership::new(
        &[node(&core_identity), node(&neighbour_identity)], &signing_key(&core_identity));
    let mut cfg = config();
    cfg.with_core.liveness_window_in_rounds = NonZeroU128::new(2).unwrap();
    let mut core = swarm(&core_identity, || BlendNetworkBehaviour::new(
        &cfg, (membership, Epoch::from(0)), EpochBoundVerifier::for_epoch(0),
        peer_id(&core_identity), PROTOCOL));
    let mut neighbour = swarm(&neighbour_identity, libp2p_stream::Behaviour::new);
    core.listen().with_memory_addr_external().await;
    let mut control = neighbour.behaviour_mut().new_control();
    let _incoming = control.accept(PROTOCOL).unwrap();
    neighbour.connect(&mut core).await;
    let mut open = Box::pin(control.open_stream(*core.local_peer_id(), PROTOCOL));
    let mut stream = loop {
        select! {
            opened = &mut open => break opened.unwrap(),
            _ = neighbour.select_next_some() => {},
            _ = core.select_next_some() => {},
        }
    };
    stream.write_all(&serialize_encapsulated_message_with_verified_public_header(
        &message_for_epoch(0, 1))).await.unwrap();
    stream.flush().await.unwrap();
    assert!(next_outcome(&mut core, &mut neighbour).await.starts_with("message accepted"));
    assert_eq!(core.behaviour().with_core().num_live_peers(), 1);

    // Keep the connection and streams alive, with no deliveries, while the task
    // is not scheduled for three complete one-second rounds (W is two).
    tokio::time::sleep(Duration::from_secs(3)).await;
    futures::future::poll_fn(|cx| {
        let _ = NetworkBehaviour::poll(core.behaviour_mut().with_core_mut(), cx);
        std::task::Poll::Ready(())
    }).await;
    assert_eq!(core.behaviour().with_core().num_live_peers(), 0,
        "all elapsed connected rounds must count: after more than W silent rounds the peer is not live, even if the behaviour was not polled at each boundary");
}
/// Drives both swarms until the core node either reports a message or
/// blacklists a neighbour, and describes which.
async fn next_outcome(
    core: &mut Swarm<BlendNetworkBehaviour<EpochBoundVerifier>>,
    neighbour: &mut Swarm<libp2p_stream::Behaviour>,
) -> String {
    timeout(Duration::from_secs(10), async {
        loop {
            select! {
                _ = neighbour.select_next_some() => {}
                event = core.select_next_some() => match event {
                    SwarmEvent::Behaviour(NetworkBehaviourEvent::WithCore(CoreEvent::Message { epoch, sender, .. })) => {
                        return format!("message accepted from {sender} under epoch {epoch:?}");
                    }
                    SwarmEvent::Behaviour(NetworkBehaviourEvent::WithCore(CoreEvent::PeerBlacklisted { peer, reason })) => {
                        return format!("neighbour {peer} blacklisted for {reason:?}");
                    }
                    _ => {}
                },
            }
        }
    })
    .await
    .unwrap_or_else(|_| "nothing happened within 10 s".to_owned())
}
