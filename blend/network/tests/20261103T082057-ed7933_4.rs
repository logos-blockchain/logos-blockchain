//! Proof for finding `20261103T082057-ed7933/4`.
//!
//! Blend Protocol, Network Maintenance, Connectivity Maintenance, Degree:
//!
//! > 1. [...] It opens a connection while it holds fewer than `Φ_CC − 1` live
//! >    ones, or fewer than `Φ_CC − 2` live ones that it opened. [...]
//! > 4. A connection whose handshake is in progress counts towards `Φ_CC`
//! >    once the Neighbor Distinction Process has identified the neighbor as
//! >    a core node [...]
//!
//! and, Liveness 1, a connection is live while it "has been connected for
//! fewer than `W` rounds in total".
//!
//! The behaviour counts a handshake in progress against the maximum it holds
//! (`available_connection_slots`) but not against the minimum it opens towards
//! (`connections_to_open`), so a node with `Φ_CC − 1` identified outgoing
//! handshakes in flight still asks for more connections to be opened, and ends
//! up filling the slots the rule reserves for accepted connections.

use std::num::{NonZeroU64, NonZeroU128, NonZeroUsize};

use lb_blend_membership::{Membership, Node};
use lb_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey, encap::ProofsVerifier,
};
use lb_blend_primitives::time::{RoundClock, RoundCount};
use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    core::{Endpoint, transport::PortUse},
    identity::{PublicKey, ed25519},
    swarm::{ConnectionId, NetworkBehaviour as _},
};
use logos_blockchain_blend_network::core::{
    CommonConfig,
    with_core::behaviour::{Behaviour, Config},
};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-4");
/// `Φ_CC`, as deployed.
const PEERING_DEGREE: usize = 4;

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

fn node(identity: &ed25519::Keypair) -> Node<PeerId> {
    Node {
        id: peer_id(identity),
        address: Multiaddr::empty(),
        public_key: Ed25519PublicKey::from_bytes(&identity.public().to_bytes()).unwrap(),
    }
}

#[tokio::test]
async fn identified_outgoing_handshakes_count_towards_the_connections_to_open() {
    let local = ed25519::Keypair::generate();
    let remotes: Vec<_> = (0..6).map(|_| ed25519::Keypair::generate()).collect();
    let nodes: Vec<_> = std::iter::once(&local)
        .chain(&remotes)
        .map(node)
        .collect();
    let membership = Membership::new(
        &nodes,
        &Ed25519PublicKey::from_bytes(&local.public().to_bytes()).unwrap(),
    );

    let mut behaviour = Behaviour::new(
        (
            &CommonConfig {
                round_duration_in_seconds: NonZeroU64::new(1).unwrap(),
                minimum_network_size: NonZeroUsize::new(1).unwrap(),
                num_blend_layers: NonZeroU64::new(3).unwrap(),
            },
            &Config {
                target_peering_degree: NonZeroUsize::new(PEERING_DEGREE).unwrap(),
                liveness_window_in_rounds: NonZeroU128::new(10).unwrap(),
                connection_share_per_round: NonZeroU64::new(20).unwrap(),
                send_deadline_in_rounds: RoundCount::new(NonZeroU128::new(2).unwrap()),
                handshake_deadline_in_rounds: RoundCount::new(NonZeroU128::new(2).unwrap()),
            },
        ),
        (membership, Epoch::from(0)),
        AcceptingVerifier,
        peer_id(&local),
        RoundClock::new(NonZeroU64::new(1).unwrap()),
        PROTOCOL,
    );

    // Control: with nothing held, the node opens `Φ_CC − 1` connections.
    assert_eq!(behaviour.connections_to_open(), PEERING_DEGREE - 1);

    // It opens them. The transport handshakes complete and the Neighbor
    // Distinction Process identifies each peer as a core node; the Blend
    // protocol upgrade is still in progress.
    for (index, remote) in remotes.iter().take(PEERING_DEGREE - 1).enumerate() {
        let handler = behaviour.handle_established_outbound_connection(
            ConnectionId::new_unchecked(index),
            peer_id(remote),
            &Multiaddr::empty(),
            Endpoint::Dialer,
            PortUse::Reuse,
        );
        assert!(
            matches!(handler, Ok(either::Either::Left(_))),
            "control: the identified core peer is taken into the Blend handshake"
        );
    }

    // The node itself counts the three handshakes as holding three of its
    // `Φ_CC + 1` slots.
    assert_eq!(
        behaviour.available_connection_slots(),
        PEERING_DEGREE + 1 - (PEERING_DEGREE - 1),
        "control: handshakes in progress hold slots"
    );

    // It holds `Φ_CC − 1` connections towards `Φ_CC`, all opened by itself and
    // all live (connected for fewer than `W` rounds): nothing more is to be
    // opened.
    assert_eq!(
        behaviour.connections_to_open(),
        0,
        "with Φ_CC − 1 = {} identified outgoing handshakes in progress, which count towards \
         Φ_CC, the node holds enough connections and must not open more",
        PEERING_DEGREE - 1
    );
}
