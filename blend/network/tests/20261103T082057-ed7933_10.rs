//! Proof for 20261103T082057-ed7933/10: edge slots must be reserved at acceptance.
use std::num::{NonZeroU64, NonZeroUsize};

use lb_blend_membership::{Membership, Node};
use lb_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey, encap::ProofsVerifier,
};
use lb_blend_primitives::time::RoundClock;
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
    CommonConfig,
    with_edge::behaviour::{Behaviour, Config},
};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blend/proof/20261103T082057-ed7933-10");
/// Small configured `Φ_CE^Max` to expose the simultaneous-connection limit.
const MAX_EDGE: usize = 2;

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
async fn accepted_edge_connections_reserve_capacity_before_stream_negotiation() {
    use libp2p::swarm::ConnectionHandler as _;
    let local = ed25519::Keypair::generate();
    let membership = Membership::new(&[node(&local)],
        &Ed25519PublicKey::from_bytes(&local.public().to_bytes()).unwrap());
    let mut behaviour = Behaviour::new(
        (&CommonConfig {
            round_duration_in_seconds: NonZeroU64::new(1).unwrap(),
            minimum_network_size: NonZeroUsize::new(1).unwrap(),
            num_blend_layers: NonZeroU64::new(3).unwrap(),
        }, &Config {
            connection_timeout: std::time::Duration::from_secs(1),
            max_incoming_connections: MAX_EDGE,
            accepted_connections_per_round: NonZeroU64::new(24).unwrap(),
        }),
        (membership, Epoch::from(0)),
        RoundClock::new(NonZeroU64::new(1).unwrap()),
        AcceptingVerifier,
        PROTOCOL,
    );
    // These are authenticated, identified edge connections. Keep every returned
    // handler alive; none opens its application stream or exhausts T_E.
    let mut held = Vec::new();
    for index in 0..MAX_EDGE + 1 {
        let remote = ed25519::Keypair::generate();
        let handler = behaviour.handle_established_inbound_connection(
            ConnectionId::new_unchecked(index), peer_id(&remote),
            &Multiaddr::empty(), &Multiaddr::empty()).unwrap();
        held.push(handler);
    }
    let accepted = held.iter().filter(|h| h.connection_keep_alive()).count();
    assert_eq!(accepted, MAX_EDGE,
        "Edge rule 1 limits simultaneous edge connections, including accepted connections waiting for their first stream; the third must be closed");
}
