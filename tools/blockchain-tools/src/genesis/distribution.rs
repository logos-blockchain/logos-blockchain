use std::collections::HashSet;

use lb_core::{
    mantle::{
        Value as NoteValue, note_set::CommitmentMmr, notes::InputNullifiers,
        ops::sdp::SDPDeclareOp, transactions::GenesisAllocation,
    },
    sdp::{Locators, ProviderId, ServiceType},
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct StakeHolderInfo {
    pub zk_id: ZkPublicKey,
    pub stake: NoteValue,
}

/// An initial service provider and the amount it declares. The amount counts in
/// the maximum supply and is not taken from any distributed note.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ProviderInfo {
    pub provider_id: ProviderId,
    pub zk_id: ZkPublicKey,
    pub locators: Locators,
    pub service_type: ServiceType,
    pub amount: NoteValue,
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
    #[error("Provider {0:?} declared twice for service {1:?}")]
    DuplicateProvider(ZkPublicKey, ServiceType),
}

/// The Transfer outputs and the `SDP_DECLARE` Operations of a genesis block.
/// Initial Service Declarations: a declaration consumes no note, so it has no
/// inputs and references the empty commitment root.
pub fn distribute<S, P>(
    stake_holders: S,
    providers: P,
    excluded_key: &ConsensusExcludedKey,
) -> Result<(Vec<GenesisAllocation>, Vec<SDPDeclareOp>), DistributionError>
where
    S: IntoIterator<Item = StakeHolderInfo>,
    P: IntoIterator<Item = ProviderInfo>,
{
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
    let mut declared = HashSet::new();

    for provider in providers {
        if !declared.insert((provider.zk_id, provider.service_type)) {
            return Err(DistributionError::DuplicateProvider(
                provider.zk_id,
                provider.service_type,
            ));
        }
        declarations.push(SDPDeclareOp {
            service_type: provider.service_type,
            locators: provider.locators,
            provider_id: provider.provider_id,
            zk_id: provider.zk_id,
            inputs: InputNullifiers::default(),
            commitment_merkle_root: CommitmentMmr::new().frontier_root(),
            amount: provider.amount,
        });
    }

    Ok((allocations, declarations))
}

#[cfg(test)]
mod tests {
    use lb_core::sdp::Locator;
    use lb_key_management_system_keys::keys::Ed25519PublicKey;
    use num_bigint::BigUint;

    use super::*;

    fn mock_zk_pk(byte: u8) -> ZkPublicKey {
        ZkPublicKey::from(BigUint::from(byte))
    }

    fn mock_provider_id(byte: u8) -> ProviderId {
        ProviderId(Ed25519PublicKey::from_bytes(&[byte; 32]).unwrap())
    }

    fn provider(zk_id: ZkPublicKey, amount: NoteValue) -> ProviderInfo {
        ProviderInfo {
            provider_id: mock_provider_id(10),
            zk_id,
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            service_type: ServiceType::BlendNetwork,
            amount,
        }
    }

    fn excluded_key() -> ConsensusExcludedKey {
        ConsensusExcludedKey {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        }
    }

    #[test]
    fn declarations_consume_no_note_and_declare_their_amount() {
        let zk_id_1 = mock_zk_pk(1);
        let zk_id_2 = mock_zk_pk(2);
        let stake_holders = vec![
            StakeHolderInfo {
                zk_id: zk_id_1,
                stake: 1_000,
            },
            StakeHolderInfo {
                zk_id: zk_id_2,
                stake: 2_000,
            },
        ];

        let (allocations, declarations) = distribute(
            stake_holders,
            vec![provider(mock_zk_pk(9), 500)],
            &excluded_key(),
        )
        .unwrap();

        assert_eq!(allocations.len(), 3);
        assert_eq!(
            allocations[0],
            GenesisAllocation {
                value: 1_000,
                public_key: zk_id_1
            }
        );
        assert_eq!(allocations[1].public_key, zk_id_2);
        assert_eq!(allocations[2].public_key, mock_zk_pk(3));

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].zk_id, mock_zk_pk(9));
        assert!(declarations[0].inputs.is_empty());
        assert_eq!(
            declarations[0].commitment_merkle_root,
            CommitmentMmr::new().frontier_root()
        );
        assert_eq!(declarations[0].amount, 500);
    }

    #[test]
    fn a_provider_declared_twice_for_a_service_is_rejected() {
        let zk_id = mock_zk_pk(1);
        let result = distribute(
            Vec::new(),
            vec![provider(zk_id, 500), provider(zk_id, 500)],
            &excluded_key(),
        );

        assert!(matches!(
            result,
            Err(DistributionError::DuplicateProvider(
                key,
                ServiceType::BlendNetwork
            )) if key == zk_id
        ));
    }

    #[test]
    fn a_provider_entry_requires_its_amount() {
        let without_amount = "\
provider_id: aa70aafc48536ae13168ed4845981a40cbd2dc1c38df88d04c46250e9ad65ce0
zk_id: 852efb444db8c3c811625850df39425f43aeffc69571192c0be9f72523256e0a
locators:
  - /ip4/127.0.0.1/udp/3400/quic-v1
service_type: BN
";
        assert!(serde_yaml::from_str::<ProviderInfo>(without_amount).is_err());
        let with_amount = format!("{without_amount}amount: 1000000000\n");
        assert_eq!(
            serde_yaml::from_str::<ProviderInfo>(&with_amount)
                .unwrap()
                .amount,
            1_000_000_000
        );
    }
}
