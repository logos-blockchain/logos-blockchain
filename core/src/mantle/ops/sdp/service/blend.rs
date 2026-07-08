use lb_key_management_system_keys::keys::ZkPublicKey;
use thiserror::Error;

use crate::{
    mantle::{ledger::Declarations, ops::sdp::SDPDeclareOp},
    sdp::{ProviderId, ServiceType},
};

/// Blend-specific declaration validation.
/// - `provider_id` must be unique across all existing Blend declarations.
/// - `zk_id` must be unique across all existing Blend declarations.
pub fn validate_declaration(msg: &SDPDeclareOp, existing: &Declarations) -> Result<(), Error> {
    existing
        .values()
        .filter(|d| d.service_type == ServiceType::BlendNetwork)
        .try_for_each(|declaration| {
            if declaration.provider_id == msg.provider_id {
                Err(Error::DuplicateProviderId(Box::new(msg.provider_id)))
            } else if declaration.zk_id == msg.zk_id {
                Err(Error::DuplicateZkId(msg.zk_id))
            } else {
                Ok(())
            }
        })
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Error {
    #[error("duplicate provider_id: {0:?}")]
    DuplicateProviderId(Box<ProviderId>),
    #[error("duplicate zk_id: {0:?}")]
    DuplicateZkId(ZkPublicKey),
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};

    use super::{Error, validate_declaration};
    use crate::{
        mantle::ledger::Declarations,
        sdp::{Declaration, DeclarationMessage, Locator, ServiceType},
    };

    #[test]
    fn empty_existing_is_ok() {
        let msg = make_msg(1, 1);
        assert!(validate_declaration(&msg, &Declarations::new_sync()).is_ok());
    }

    #[test]
    fn distinct_provider_and_zk_is_ok() {
        let existing = insert(Declarations::new_sync(), &make_msg(1, 1));
        let new = make_msg(2, 2);
        assert!(validate_declaration(&new, &existing).is_ok());
    }

    #[test]
    fn duplicate_provider_id_is_rejected() {
        let existing = insert(Declarations::new_sync(), &make_msg(1, 1));
        let new = make_msg(1, 2);
        assert!(matches!(
            validate_declaration(&new, &existing),
            Err(Error::DuplicateProviderId(_))
        ));
    }

    #[test]
    fn duplicate_zk_id_is_rejected() {
        let existing = insert(Declarations::new_sync(), &make_msg(1, 1));
        let new = make_msg(2, 1);
        assert!(matches!(
            validate_declaration(&new, &existing),
            Err(Error::DuplicateZkId(_))
        ));
    }

    fn make_msg(provider_byte: u8, zk_val: u64) -> DeclarationMessage {
        DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: "/ip4/127.0.0.1/udp/3001/quic-v1"
                .parse::<Locator>()
                .unwrap()
                .into(),
            provider_id: Ed25519Key::from_bytes(&[provider_byte; _])
                .public_key()
                .into(),
            zk_id: ZkPublicKey::new(Fr::from(zk_val)),
            locked_note_id: Fr::ZERO.into(),
        }
    }

    fn insert(mut declarations: Declarations, msg: &DeclarationMessage) -> Declarations {
        declarations.insert_mut(msg.id(), Declaration::new(Epoch::new(0), msg));
        declarations
    }
}
