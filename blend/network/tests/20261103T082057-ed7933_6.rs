//! Proof for finding `20261103T082057-ed7933/6`.
//!
//! Blend Protocol, Network Maintenance, Connectivity Maintenance, Degree:
//!
//! > 4. A connection whose handshake is in progress counts towards `Φ_CC`
//! >    once the Neighbor Distinction Process has identified the neighbor as
//! >    a core node [...] At most `Φ_CC + 1 + Φ_CE^Max` handshakes are in
//! >    progress at once, and one offered above that is closed.
//!
//! A handshake is in progress from the moment a connection is offered, before
//! the Neighbor Distinction Process can identify the neighbour (it reads the
//! `peer_id` off the completed TLS handshake). libp2p offers every such
//! connection to the behaviour through `handle_pending_inbound_connection`,
//! which is where a cap on handshakes in progress is enforced. Neither Blend
//! behaviour implements it, so every offered handshake is let through.

use std::{
    num::{NonZeroU64, NonZeroU128, NonZeroUsize},
    time::Duration,
};

use lb_blend_membership::{Membership, Node};
use lb_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey, encap::ProofsVerifier,
};
use lb_blend_primitives::time::RoundCount;
use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    identity::{PublicKey, ed25519},
    swarm::{ConnectionId, NetworkBehaviour as _},
};
use logos_blockchain_blend_network::core::{
    CommonConfig, Config, NetworkBehaviour as BlendNetworkBehaviour,
    with_core::behaviour::Config as CoreConfig, with_edge::behaviour::Config as EdgeConfig,
};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-6");
/// `Φ_CC`.
const PEERING_DEGREE: usize = 4;
/// `Φ_CE^Max`.
const MAX_EDGE_CONNECTIONS: usize = 2;

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
async fn handshakes_offered_above_the_cap_are_closed() {
    let local = ed25519::Keypair::generate();
    let others: Vec<_> = (0..8).map(|_| ed25519::Keypair::generate()).collect();
    let nodes: Vec<_> = std::iter::once(&local).chain(&others).map(node).collect();
    let membership = Membership::new(
        &nodes,
        &Ed25519PublicKey::from_bytes(&local.public().to_bytes()).unwrap(),
    );

    let mut behaviour = BlendNetworkBehaviour::new(
        &Config {
            common: CommonConfig {
                round_duration_in_seconds: NonZeroU64::new(1).unwrap(),
                minimum_network_size: NonZeroUsize::new(1).unwrap(),
                num_blend_layers: NonZeroU64::new(3).unwrap(),
            },
            with_core: CoreConfig {
                target_peering_degree: NonZeroUsize::new(PEERING_DEGREE).unwrap(),
                liveness_window_in_rounds: NonZeroU128::new(10).unwrap(),
                connection_share_per_round: NonZeroU64::new(20).unwrap(),
                send_deadline_in_rounds: RoundCount::new(NonZeroU128::new(2).unwrap()),
                handshake_deadline_in_rounds: RoundCount::new(NonZeroU128::new(2).unwrap()),
            },
            with_edge: EdgeConfig {
                connection_timeout: Duration::from_secs(1),
                max_incoming_connections: MAX_EDGE_CONNECTIONS,
                accepted_connections_per_round: NonZeroU64::new(1).unwrap(),
            },
        },
        (membership, Epoch::from(0)),
        AcceptingVerifier,
        peer_id(&local),
        PROTOCOL,
    );

    let cap = PEERING_DEGREE + 1 + MAX_EDGE_CONNECTIONS;
    let local_address: Multiaddr = "/memory/1".parse().unwrap();

    // `cap + 1` peers start a handshake with this node at once, none of them
    // finishing it.
    let admitted = (0..=cap)
        .filter(|&index| {
            let remote_address: Multiaddr = format!("/memory/{}", 1_000 + index).parse().unwrap();
            behaviour
                .handle_pending_inbound_connection(
                    ConnectionId::new_unchecked(index),
                    &local_address,
                    &remote_address,
                )
                .is_ok()
        })
        .count();

    assert_eq!(
        admitted, cap,
        "at most Φ_CC + 1 + Φ_CE^Max = {cap} handshakes may be in progress at once, and the one \
         offered above that must be closed"
    );
}
