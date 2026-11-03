use std::collections::{HashMap, HashSet};

use lb_core::{
    mantle::{
        Value as NoteValue, note_set::CommitmentMmr, notes::derive_note_nullifier,
        ops::sdp::SDPDeclareOp, transactions::GenesisAllocation,
    },
    sdp::ServiceType,
};
use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
use lb_sdp_service::DeclarationConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct StakeHolderInfo {
    pub zk_id: ZkPublicKey,
    pub stake: NoteValue,
}

/// `ConsensusExcludedKey` is used to register a consensus-excluded key with
/// its value.
#[derive(Clone, Debug, Deserialize)]
pub struct ConsensusExcludedKey {
    pub zk_id: ZkPublicKey,
    pub funds: NoteValue,
}

#[derive(Error, Debug)]
pub enum DistributionError {
    #[error("Provider with ZK ID {0:?} is not a registered stakeholder")]
    ProviderNotStakeHolder(Box<DeclarationConfig>),

    #[error("Note already used for service {0:?}")]
    NoteAlreadyUsedForService(ServiceType),

    #[error("Provider {0:?} has no stake key")]
    MissingStakeKey(ZkPublicKey),
}

/// `distribute` stake to stake holders.
/// Provider has to be a stakeholder, because the declaration spends the
/// stakeholder note with the stake key of its `zk_id`.
#[expect(
    clippy::implicit_hasher,
    clippy::type_complexity,
    reason = "the stake keys file map and the distribution tuple"
)]
pub fn distribute<S, P>(
    stake_holders: S,
    providers: P,
    excluded_key: &ConsensusExcludedKey,
    stake_keys: &HashMap<ZkPublicKey, ZkKey>,
) -> Result<(Vec<GenesisAllocation>, Vec<SDPDeclareOp>, Vec<ZkKey>), DistributionError>
where
    S: IntoIterator<Item = StakeHolderInfo> + Clone,
    P: IntoIterator<Item = DeclarationConfig>,
{
    let stake_holder_keys: HashSet<ZkPublicKey> =
        stake_holders.clone().into_iter().map(|s| s.zk_id).collect();

    let allocations: Vec<GenesisAllocation> = stake_holders
        .into_iter()
        .map(|stake_holder| GenesisAllocation {
            value: stake_holder.stake,
            public_key: stake_holder.zk_id,
        })
        .chain([GenesisAllocation {
            value: excluded_key.funds,
            public_key: excluded_key.zk_id,
        }])
        .collect();
    let mut declarations = Vec::new();
    let mut keys = Vec::new();
    let mut used_services = HashSet::new();

    for provider in providers {
        if !stake_holder_keys.contains(&provider.zk_id) {
            return Err(DistributionError::ProviderNotStakeHolder(Box::new(
                provider,
            )));
        }

        if !used_services.insert((provider.zk_id, provider.service_type)) {
            return Err(DistributionError::NoteAlreadyUsedForService(
                provider.service_type,
            ));
        }

        if let Some(index) = allocations
            .iter()
            .position(|allocation| allocation.public_key == provider.zk_id)
        {
            let stake_key = stake_keys
                .get(&provider.zk_id)
                .ok_or(DistributionError::MissingStakeKey(provider.zk_id))?;
            let allocation = allocations[index];
            declarations.push(SDPDeclareOp {
                service_type: provider.service_type,
                locators: provider.locators,
                provider_id: provider.provider_id,
                zk_id: provider.zk_id,
                inputs: [derive_note_nullifier(
                    &allocation.note(index).commitment(),
                    stake_key.as_fr(),
                )]
                .into(),
                commitment_merkle_root: CommitmentMmr::new().frontier_root(),
                amount: allocation.value,
            });
            keys.push(stake_key.clone());
        }
    }

    Ok((allocations, declarations, keys))
}

#[cfg(test)]
mod tests {
    use lb_core::sdp::{Locator, ProviderId};
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::Ed25519PublicKey;
    use num_bigint::BigUint;

    use super::*;

    fn mock_zk_pk(byte: u8) -> ZkPublicKey {
        ZkPublicKey::from(BigUint::from(byte))
    }

    fn mock_provider_id(byte: u8) -> ProviderId {
        ProviderId(Ed25519PublicKey::from_bytes(&[byte; 32]).unwrap())
    }

    #[test]
    fn declarations_spend_the_stakeholder_note_with_its_key() {
        let stake_key = ZkKey::new(Fr::from(5u64));
        let zk_id = stake_key.to_public_key();
        let stake_holders = vec![
            StakeHolderInfo {
                zk_id,
                stake: 1_000,
            },
            StakeHolderInfo {
                zk_id: mock_zk_pk(2),
                stake: 2_000,
            },
        ];
        let providers = vec![DeclarationConfig {
            provider_id: mock_provider_id(10),
            zk_id,
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
        }];
        let excluded_key = ConsensusExcludedKey {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let (allocations, declarations, keys) = distribute(
            stake_holders,
            providers,
            &excluded_key,
            &HashMap::from([(zk_id, stake_key.clone())]),
        )
        .unwrap();
        assert_eq!(
            allocations[0],
            GenesisAllocation {
                value: 1_000,
                public_key: zk_id
            }
        );
        assert_eq!(
            declarations[0].inputs.as_slice(),
            [derive_note_nullifier(
                &allocations[0].note(0).commitment(),
                stake_key.as_fr()
            )]
        );
        assert_eq!(
            declarations[0].commitment_merkle_root,
            CommitmentMmr::new().frontier_root()
        );
        assert_eq!(declarations[0].amount, 1_000);
        assert_eq!(keys, vec![stake_key]);
    }

    #[test]
    fn test_successful_distribution() {
        let stake_key = ZkKey::new(Fr::from(1u64));
        let zk_id_1 = stake_key.to_public_key();
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

        let providers = vec![DeclarationConfig {
            provider_id: mock_provider_id(10),
            zk_id: zk_id_1,
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
        }];

        let excluded_key = ConsensusExcludedKey {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(
            stake_holders,
            providers,
            &excluded_key,
            &HashMap::from([(zk_id_1, stake_key.clone())]),
        );

        assert!(result.is_ok());
        let (allocations, declarations, keys) = result.unwrap();

        assert_eq!(allocations.len(), 3);
        assert_eq!(allocations[0].public_key, zk_id_1);
        assert_eq!(allocations[1].public_key, zk_id_2);
        assert_eq!(allocations[2].public_key, mock_zk_pk(3));

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].zk_id, zk_id_1);
        assert_eq!(keys, vec![stake_key]);
    }

    #[test]
    fn test_error_unauthorized_provider() {
        let stake_holders = vec![StakeHolderInfo {
            zk_id: mock_zk_pk(1),
            stake: 1000,
        }];

        let providers = vec![DeclarationConfig {
            provider_id: mock_provider_id(10),
            zk_id: mock_zk_pk(2),
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
        }];

        let excluded_key = ConsensusExcludedKey {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(stake_holders, providers, &excluded_key, &HashMap::new());

        assert!(matches!(
            result,
            Err(DistributionError::ProviderNotStakeHolder(_))
        ));

        if let Err(DistributionError::ProviderNotStakeHolder(info)) = result {
            assert_eq!(info.zk_id, mock_zk_pk(2));
        }
    }

    #[test]
    fn test_error_already_used() {
        let stake_key = ZkKey::new(Fr::from(1u64));
        let zk_id = stake_key.to_public_key();
        let stake_holders = vec![StakeHolderInfo { zk_id, stake: 5000 }];

        // Two providers trying to use the same note for the same ServiceType.
        let providers = vec![
            DeclarationConfig {
                provider_id: mock_provider_id(10),
                zk_id,
                locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
                service_type: ServiceType::BlendNetwork,
            },
            DeclarationConfig {
                provider_id: mock_provider_id(11),
                zk_id,
                locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
                service_type: ServiceType::BlendNetwork,
            },
        ];

        let excluded_key = ConsensusExcludedKey {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(
            stake_holders,
            providers,
            &excluded_key,
            &HashMap::from([(zk_id, stake_key)]),
        );

        assert!(matches!(
            result,
            Err(DistributionError::NoteAlreadyUsedForService(
                ServiceType::BlendNetwork
            ))
        ));
    }

    #[test]
    fn test_error_missing_stake_key() {
        let zk_id = mock_zk_pk(1);
        let stake_holders = vec![StakeHolderInfo { zk_id, stake: 5000 }];
        let providers = vec![DeclarationConfig {
            provider_id: mock_provider_id(10),
            zk_id,
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
        }];
        let excluded_key = ConsensusExcludedKey {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(stake_holders, providers, &excluded_key, &HashMap::new());

        assert!(matches!(
            result,
            Err(DistributionError::MissingStakeKey(key)) if key == zk_id
        ));
    }
}
