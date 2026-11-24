use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_libp2p::ed25519;
use libp2p::{
    PeerId,
    identity::{DecodingError, Keypair},
};

/// The libp2p identity of the node holding `non_ephemeral_signing_key`.
#[must_use]
pub(crate) fn keypair(non_ephemeral_signing_key: &UnsecuredEd25519Key) -> Keypair {
    let mut secret_key_bytes = *non_ephemeral_signing_key.as_bytes();
    Keypair::ed25519_from_bytes(&mut secret_key_bytes)
        .expect("Cryptographic secret key should be a valid Ed25519 private key.")
}

impl super::TryFrom for PeerId {
    type Error = DecodingError;

    fn try_from_provider_id(provider_id: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self::from_public_key(
            &ed25519::PublicKey::try_from_bytes(provider_id)?.into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::node_id::TryFrom as _;

    #[test]
    fn test_try_from_provider_id() {
        let keypair = ed25519::Keypair::generate();
        let public_key = keypair.public();
        let provider_id = public_key.to_bytes();

        let node_id = PeerId::try_from_provider_id(&provider_id).unwrap();
        let expected_node_id = PeerId::from_public_key(&public_key.into());

        assert_eq!(node_id, expected_node_id);
    }
}
