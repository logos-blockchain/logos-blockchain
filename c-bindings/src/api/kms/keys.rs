use lb_node::config::blend::serde::Config as BlendConfig;

use crate::{OperationStatus, errors::OperationStatusCode};

/// The node key a message is signed with.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum SigningKeyRole {
    /// The Ed25519 key behind the node's Blend `provider_id`.
    BlendSigning = 0x0,
    /// The ZK key behind the node's Blend `zk_id`, which receives Blend
    /// rewards.
    BlendZk = 0x1,
}

impl TryFrom<u8> for SigningKeyRole {
    type Error = OperationStatus;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x0 => Ok(Self::BlendSigning),
            0x1 => Ok(Self::BlendZk),
            _ => Err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Unknown signing key role {value}."),
            )),
        }
    }
}

/// The KMS key IDs behind each [`SigningKeyRole`].
pub struct SigningKeyIds {
    blend_signing: String,
    blend_zk: String,
}

impl From<&BlendConfig> for SigningKeyIds {
    fn from(config: &BlendConfig) -> Self {
        Self {
            blend_signing: config.non_ephemeral_signing_key_id.clone(),
            blend_zk: config.core.zk.secret_key_kms_id.clone(),
        }
    }
}

impl SigningKeyIds {
    pub(super) fn get(&self, role: SigningKeyRole) -> &str {
        match role {
            SigningKeyRole::BlendSigning => &self.blend_signing,
            SigningKeyRole::BlendZk => &self.blend_zk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_from_known_values() {
        assert!(matches!(
            SigningKeyRole::try_from(0),
            Ok(SigningKeyRole::BlendSigning)
        ));
        assert!(matches!(
            SigningKeyRole::try_from(1),
            Ok(SigningKeyRole::BlendZk)
        ));
    }

    #[test]
    fn role_from_unknown_value_is_a_validation_error() {
        let Err(status) = SigningKeyRole::try_from(2) else {
            panic!("2 is not a signing key role");
        };
        assert_eq!(status.code, OperationStatusCode::ValidationError);
    }
}
