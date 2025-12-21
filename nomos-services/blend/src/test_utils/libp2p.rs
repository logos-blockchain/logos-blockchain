use core::{
    iter::repeat_with,
    ops::{Deref, DerefMut},
    time::Duration,
};

use key_management_system_service::keys::UnsecuredEd25519Key;
use libp2p::{
    PeerId, StreamProtocol, Swarm, Transport as _, core::transport::MemoryTransport,
    identity::Keypair, plaintext, swarm, tcp, yamux,
};
use nomos_blend::{
    message::{
        PayloadType, crypto::key_ext::Ed25519SecretKeyExt as _,
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader, input::EncapsulationInput,
    },
    proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection},
    scheduling::membership::Membership,
};
use nomos_libp2p::{NetworkBehaviour, upgrade::Version};

pub const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/blend/swarm/test");

#[derive(Debug)]
pub struct TestEncapsulatedMessage(EncapsulatedMessageWithVerifiedPublicHeader);

impl TestEncapsulatedMessage {
    pub fn new(payload: &[u8]) -> Self {
        Self(EncapsulatedMessageWithVerifiedPublicHeader::new(
            &generate_valid_inputs(),
            PayloadType::Data,
            payload.try_into().unwrap(),
        ))
    }

    pub fn into_inner(self) -> EncapsulatedMessageWithVerifiedPublicHeader {
        self.0
    }
}

impl Deref for TestEncapsulatedMessage {
    type Target = EncapsulatedMessageWithVerifiedPublicHeader;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TestEncapsulatedMessage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn generate_valid_inputs() -> Vec<EncapsulationInput> {
    repeat_with(UnsecuredEd25519Key::generate_with_blake_rng)
        .take(3)
        .map(|recipient_signing_key| {
            let recipient_signing_pubkey = recipient_signing_key.public_key();
            EncapsulationInput::new(
                UnsecuredEd25519Key::generate_with_blake_rng(),
                &recipient_signing_pubkey,
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            )
        })
        .collect::<Vec<_>>()
}

/// Instantiate a new memory-based Swarm that uses the configured timeout for
/// idle connections and instantiates the behaviour as returned by the provided
/// constructor.
pub fn memory_test_swarm<BehaviourConstructor, Behaviour>(
    identity: &Keypair,
    membership: Membership<PeerId>,
    idle_connection_timeout: Duration,
    behaviour_constructor: BehaviourConstructor,
) -> Swarm<Behaviour>
where
    BehaviourConstructor: FnOnce(PeerId, Membership<PeerId>) -> Behaviour,
    Behaviour: NetworkBehaviour,
{
    let peer_id = PeerId::from(identity.public());

    let transport = MemoryTransport::default()
        .or_transport(tcp::tokio::Transport::default())
        .upgrade(Version::V1)
        .authenticate(plaintext::Config::new(identity))
        .multiplex(yamux::Config::default())
        .timeout(Duration::from_secs(1))
        .boxed();

    Swarm::new(
        transport,
        behaviour_constructor(peer_id, membership),
        peer_id,
        swarm::Config::with_tokio_executor().with_idle_connection_timeout(idle_connection_timeout),
    )
}
