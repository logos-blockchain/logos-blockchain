//! Proof for finding `20261103T082057-ed7933/8`.
//!
//! Blend Protocol, Network Maintenance, Expected Traffic and Admission:
//!
//! > A node reads at most `(Φ_CC + 1) · r_1 + r_E` messages in a round, which
//! > must not exceed `V` [...] The shares keep the messages a node reads in a
//! > round within what the slowest node the protocol targets can verify, `V`.
//!
//! and Transition Period:
//!
//! > The node needs to maintain old connections and process all messages
//! > received from these connections for the duration of TP. A connection
//! > held for the past epoch does not count towards `Φ_CC`.
//!
//! The specification does not say how the connections held for the past epoch
//! share the per-round read budget. The behaviour gives each of them a full
//! `r_1` on top of a full set of `Φ_CC + 1` new-epoch connections, so during
//! every transition period a node reads up to `2 · (Φ_CC + 1) · r_1 + r_E`
//! messages a round, past the bound the shares exist to keep.

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
use logos_blockchain_blend_network::core::{
    CommonConfig, Config, NetworkBehaviour as BlendNetworkBehaviour, NetworkBehaviourEvent,
    with_core::behaviour::{Config as CoreConfig, Event as CoreEvent},
    with_edge::behaviour::Config as EdgeConfig,
};
use tokio::{select, time::Instant};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-8");
const NUM_BLEND_LAYERS: u64 = 3;
/// `Φ_CC`. With it, a node holds at most `Φ_CC + 1 = 3` core connections and
/// accepts up to `(Φ_CC + 1) − max(Φ_CC − 2, 0) = 3` of them.
const PEERING_DEGREE: usize = 2;
/// `r_1`.
const SHARE: u64 = 3;
/// `r_E`.
const EDGE_SHARE: u64 = 1;
/// The bound of Expected Traffic: `(Φ_CC + 1) · r_1 + r_E`.
const READS_PER_ROUND_BOUND: u64 = (PEERING_DEGREE as u64 + 1) * SHARE + EDGE_SHARE;
/// The rounds over which reads are counted.
const MEASURED_ROUNDS: u64 = 6;
/// Messages each neighbour has queued: enough to keep its connection's share
/// spent for the whole measurement.
const BACKLOG: u8 = 60;

/// A `PoQ` verifier bound to one epoch's public inputs: it accepts exactly the
/// proofs generated for its own epoch, written into the first byte of the key
/// nullifier.
#[derive(Debug, Clone, Copy)]
struct EpochBoundVerifier {
    epoch: u8,
}

impl ProofsVerifier for EpochBoundVerifier {
    type Error = ();

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self { epoch: 0 }
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

/// A message every layer of which carries a proof generated for `epoch`; the
/// nullifier is distinct per `(sender, nonce)`.
fn message(epoch: u8, sender: u8, nonce: u8) -> Vec<u8> {
    let inputs = (0..NUM_BLEND_LAYERS)
        .map(|_| {
            let mut quota = [0u8; PROOF_OF_QUOTA_SIZE];
            quota[0] = epoch;
            quota[8] = sender;
            quota[9] = nonce;
            EncapsulationInput::new(
                UnsecuredEd25519Key::generate_with_chacha_rng(),
                &UnsecuredEd25519Key::generate_with_chacha_rng().public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked(quota),
                VerifiedProofOfSelection::from_bytes_unchecked([0u8; _]),
            )
        })
        .collect::<Vec<_>>();
    serialize_encapsulated_message_with_verified_public_header(
        &EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &inputs,
            PayloadType::BlockProposal,
            PaddedPayloadBody::try_from(&b"traffic"[..]).unwrap(),
            usize::try_from(NUM_BLEND_LAYERS).unwrap(),
        )
        .unwrap(),
    )
}

fn peer_id(identity: &ed25519::Keypair) -> PeerId {
    PublicKey::from(identity.public()).into()
}

fn node(identity: &ed25519::Keypair) -> Node<PeerId> {
    Node {
        id: peer_id(identity),
        address: Multiaddr::empty(),
        public_key: Ed25519PublicKey::from_bytes(&identity.public().to_bytes()).unwrap(),
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

type Core = Swarm<BlendNetworkBehaviour<EpochBoundVerifier>>;

/// Connects a core neighbour to `core`, opens its Blend stream, and leaves it
/// running in the background writing `BACKLOG` messages of `epoch`.
async fn start_neighbour(core: &mut Core, identity: &ed25519::Keypair, epoch: u8, sender: u8) {
    let mut neighbour = swarm(identity, libp2p_stream::Behaviour::new);
    let mut control = neighbour.behaviour_mut().new_control();
    let incoming = control.accept(PROTOCOL).unwrap();
    neighbour.connect(core).await;
    let core_peer_id = *core.local_peer_id();
    let neighbour_peer_id = *neighbour.local_peer_id();
    let mut open = Box::pin(control.open_stream(core_peer_id, PROTOCOL));
    // Drive both until the stream is open and the core node has taken the
    // connection up as a neighbour, whichever comes first.
    let mut stream = None;
    let mut taken_up = false;
    let setup = tokio::time::timeout(Duration::from_secs(10), async {
        while stream.is_none() || !taken_up {
            select! {
                opened = &mut open, if stream.is_none() => {
                    stream = Some(opened.unwrap_or_else(|error| {
                        panic!("setup: neighbour {sender} of epoch {epoch} could not open its stream: {error:?}")
                    }));
                }
                _ = neighbour.select_next_some() => {}
                event = core.select_next_some() => {
                    if let SwarmEvent::Behaviour(NetworkBehaviourEvent::WithCore(CoreEvent::InboundConnectionUpgradeSucceeded(peer))) = event
                        && peer == neighbour_peer_id
                    {
                        taken_up = true;
                    }
                }
            }
        }
    })
    .await;
    assert!(
        setup.is_ok(),
        "setup: neighbour {sender} of epoch {epoch} was not taken up (stream open: {}, upgraded: {taken_up})",
        stream.is_some()
    );
    let mut stream = stream.unwrap();
    tokio::spawn(async move {
        let _incoming = incoming;
        loop {
            neighbour.select_next_some().await;
        }
    });
    let backlog: Vec<_> = (0..BACKLOG).map(|nonce| message(epoch, sender, nonce)).collect();
    tokio::spawn(async move {
        for message in backlog {
            if stream.write_all(&message).await.is_err() {
                return;
            }
        }
        drop(stream.flush().await);
        // Keep the stream open until the test ends.
        std::future::pending::<()>().await;
    });
}

#[tokio::test]
async fn a_node_in_its_transition_period_reads_no_more_than_the_per_round_bound() {
    let local = ed25519::Keypair::generate();
    let old_neighbours: Vec<_> = (0..=PEERING_DEGREE)
        .map(|_| ed25519::Keypair::generate())
        .collect();
    let new_neighbours: Vec<_> = (0..=PEERING_DEGREE)
        .map(|_| ed25519::Keypair::generate())
        .collect();
    let nodes: Vec<_> = std::iter::once(&local)
        .chain(&old_neighbours)
        .chain(&new_neighbours)
        .map(node)
        .collect();
    let membership = Membership::new(
        &nodes,
        &Ed25519PublicKey::from_bytes(&local.public().to_bytes()).unwrap(),
    );

    let mut core = swarm(&local, || {
        BlendNetworkBehaviour::new(
            &Config {
                common: CommonConfig {
                    round_duration_in_seconds: NonZeroU64::new(1).unwrap(),
                    minimum_network_size: NonZeroUsize::new(1).unwrap(),
                    num_blend_layers: NonZeroU64::new(NUM_BLEND_LAYERS).unwrap(),
                },
                with_core: CoreConfig {
                    target_peering_degree: NonZeroUsize::new(PEERING_DEGREE).unwrap(),
                    liveness_window_in_rounds: NonZeroU128::new(1_000).unwrap(),
                    connection_share_per_round: NonZeroU64::new(SHARE).unwrap(),
                    send_deadline_in_rounds: RoundCount::new(NonZeroU128::new(2).unwrap()),
                    handshake_deadline_in_rounds: RoundCount::new(NonZeroU128::new(30).unwrap()),
                },
                with_edge: EdgeConfig {
                    connection_timeout: Duration::from_secs(1),
                    max_incoming_connections: 2,
                    accepted_connections_per_round: NonZeroU64::new(EDGE_SHARE).unwrap(),
                },
            },
            (membership.clone(), Epoch::from(0)),
            EpochBoundVerifier { epoch: 0 },
            peer_id(&local),
            PROTOCOL,
        )
    });
    core.listen().with_memory_addr_external().await;

    // A full set of `Φ_CC + 1` connections in epoch 0.
    for (index, identity) in old_neighbours.iter().enumerate() {
        start_neighbour(&mut core, identity, 0, u8::try_from(index).unwrap()).await;
    }
    // Epoch 1 begins: those connections are kept for the transition period,
    // and no longer count towards `Φ_CC`, so a full new set is accepted.
    core.behaviour_mut().start_new_epoch(
        (membership.clone(), Epoch::from(1)),
        EpochBoundVerifier { epoch: 1 },
    );
    for (index, identity) in new_neighbours.iter().enumerate() {
        start_neighbour(&mut core, identity, 1, 100 + u8::try_from(index).unwrap()).await;
    }

    // Let every connection reach its steady state, then count what the node
    // reads over whole rounds.
    let warm_up = Instant::now() + Duration::from_secs(2);
    let end = warm_up + Duration::from_secs(MEASURED_ROUNDS);
    let mut read = 0u64;
    let mut read_per_epoch = [0u64; 2];
    loop {
        select! {
            event = core.select_next_some() => {
                if let SwarmEvent::Behaviour(NetworkBehaviourEvent::WithCore(CoreEvent::Message { epoch, .. })) = event
                    && Instant::now() >= warm_up
                {
                    read += 1;
                    read_per_epoch[usize::try_from(epoch.into_inner()).unwrap()] += 1;
                }
            }
            () = tokio::time::sleep_until(end) => break,
        }
    }

    // Control: the connections held for the past epoch are being read, as the
    // transition period requires.
    assert!(read_per_epoch[0] > 0, "control: past-epoch connections must be read");

    // A node that never reads more than the bound in a round reads at most
    // that many in each measured round, plus one round's worth whose reads
    // were issued before the window and completed inside it.
    let allowed = READS_PER_ROUND_BOUND * (MEASURED_ROUNDS + 1);
    assert!(
        read <= allowed,
        "a node reads at most (Φ_CC + 1)·r_1 + r_E = {READS_PER_ROUND_BOUND} messages a round, \
         so at most {allowed} over {MEASURED_ROUNDS} rounds (one round of slack), but during the \
         transition period it read {read} ({} on past-epoch connections, {} on new-epoch ones)",
        read_per_epoch[0],
        read_per_epoch[1]
    );
}
