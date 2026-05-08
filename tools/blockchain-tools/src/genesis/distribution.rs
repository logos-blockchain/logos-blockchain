use std::collections::HashSet;

use lb_core::{
    mantle::{Note, Utxo, Value as NoteValue, ops::sdp::SDPDeclareOp},
    sdp::{Locators, ServiceType},
};
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
use serde::Deserialize;
use thiserror::Error;

/// `StakeHolderInfo` is used to distribute Notes of `NoteValue`.
#[derive(Clone, Deserialize)]
pub struct StakeHolderInfo {
    pub zk_id: ZkPublicKey,
    pub stake: NoteValue,
}

/// `ProviderInfo` is used to register a service provider.
/// A note matching the stake holder info by the `zk_pk` will be locked for this
/// service.
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderInfo {
    pub provider_id: Ed25519PublicKey,
    pub zk_id: ZkPublicKey,
    pub locators: Locators,
    pub service_type: ServiceType,
}

#[derive(Error, Debug)]
pub enum DistributionError {
    #[error("Provider with ZK ID {0:?} is not a registered stakeholder")]
    ProviderNotStakeHolder(Box<ProviderInfo>),

    #[error("Note already locked for service {0:?}")]
    NoteLockedForService(ServiceType),
}

/// `distribute` stake to stake holders.
/// Provider has to be a stakeholder, because stake holders note id will be used
/// as a locked note.
pub fn distribute<S, P>(
    stake_holders: S,
    providers: P,
) -> Result<(Vec<Utxo>, Vec<SDPDeclareOp>), DistributionError>
where
    S: IntoIterator<Item = StakeHolderInfo> + Clone,
    P: IntoIterator<Item = ProviderInfo>,
{
    let stake_holder_keys: HashSet<ZkPublicKey> =
        stake_holders.clone().into_iter().map(|s| s.zk_id).collect();

    let utxos: Vec<Utxo> = stake_holders
        .into_iter()
        .enumerate()
        .map(|(output_index, stake_holder)| Utxo {
            op_id: [0u8; 32],
            output_index,
            note: Note::new(stake_holder.stake, stake_holder.zk_id),
        })
        .collect();

    let mut declarations = Vec::new();
    let mut locked_services = HashSet::new();

    for provider in providers {
        if !stake_holder_keys.contains(&provider.zk_id) {
            return Err(DistributionError::ProviderNotStakeHolder(Box::new(
                provider,
            )));
        }

        if !locked_services.insert((provider.zk_id, provider.service_type)) {
            return Err(DistributionError::NoteLockedForService(
                provider.service_type,
            ));
        }

        if let Some(utxo) = utxos.iter().find(|u| u.note.pk == provider.zk_id) {
            declarations.push(SDPDeclareOp {
                service_type: provider.service_type,
                locators: provider.locators,
                provider_id: provider.provider_id.into(),
                zk_id: provider.zk_id,
                locked_note_id: utxo.id(),
            });
        }
    }

    Ok((utxos, declarations))
}

#[cfg(test)]
mod tests {
    use lb_core::sdp::Locator;
    use num_bigint::BigUint;

    use super::*;

    fn mock_zk_pk(byte: u8) -> ZkPublicKey {
        ZkPublicKey::from(BigUint::from(byte))
    }

    fn mock_ed_pk(byte: u8) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    #[test]
    fn test_successful_distribution() {
        let zk_id_1 = mock_zk_pk(1);
        let zk_id_2 = mock_zk_pk(2);

        let stake_holders = vec![
            StakeHolderInfo {
                zk_id: zk_id_1,
                stake: 1000,
            },
            StakeHolderInfo {
                zk_id: zk_id_2,
                stake: 2000,
            },
        ];

        let providers = vec![ProviderInfo {
            provider_id: mock_ed_pk(10),
            zk_id: zk_id_1,
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
        }];

        let result = distribute(stake_holders, providers);

        assert!(result.is_ok());
        let (utxos, declarations) = result.unwrap();

        assert_eq!(utxos.len(), 2);
        assert_eq!(utxos[0].note.pk, zk_id_1);
        assert_eq!(utxos[1].note.pk, zk_id_2);

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].zk_id, zk_id_1);
        assert_eq!(declarations[0].locked_note_id, utxos[0].id());
    }

    #[test]
    fn test_error_unauthorized_provider() {
        let stake_holders = vec![StakeHolderInfo {
            zk_id: mock_zk_pk(1),
            stake: 1000,
        }];

        let providers = vec![ProviderInfo {
            provider_id: mock_ed_pk(10),
            zk_id: mock_zk_pk(2),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
        }];

        let result = distribute(stake_holders, providers);

        assert!(matches!(
            result,
            Err(DistributionError::ProviderNotStakeHolder(_))
        ));

        if let Err(DistributionError::ProviderNotStakeHolder(info)) = result {
            assert_eq!(info.zk_id, mock_zk_pk(2));
        }
    }

    #[test]
    fn test_error_already_locked() {
        let zk_id = mock_zk_pk(1);
        let stake_holders = vec![StakeHolderInfo { zk_id, stake: 5000 }];

        // Two providers trying to use the same note for the same ServiceType.
        let providers = vec![
            ProviderInfo {
                provider_id: mock_ed_pk(10),
                zk_id,
                locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
                service_type: ServiceType::BlendNetwork,
            },
            ProviderInfo {
                provider_id: mock_ed_pk(11),
                zk_id,
                locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
                service_type: ServiceType::BlendNetwork,
            },
        ];

        let result = distribute(stake_holders, providers);

        assert!(matches!(
            result,
            Err(DistributionError::NoteLockedForService(
                ServiceType::BlendNetwork
            ))
        ));
    }
}
