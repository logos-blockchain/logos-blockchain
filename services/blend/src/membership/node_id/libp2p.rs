use lb_libp2p::ed25519;
use libp2p::{PeerId, identity::DecodingError};

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
