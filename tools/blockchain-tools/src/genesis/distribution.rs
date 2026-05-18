use std::collections::HashSet;

use lb_core::{
    mantle::{
        Note, Utxo, Value as NoteValue,
        ledger::{Inputs, Outputs},
        ops::{sdp::SDPDeclareOp, transfer::TransferOp},
    },
    sdp::{Locators, ServiceType},
};
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Serialize, Deserialize)]
pub struct StakeHolderInfo {
    pub zk_id: ZkPublicKey,
    pub stake: NoteValue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider_id: Ed25519PublicKey,
    pub zk_id: ZkPublicKey,
    pub locators: Locators,
    pub service_type: ServiceType,
}

/// `Faucet` is used to register a faucet key with it's value.
#[derive(Clone, Debug, Deserialize)]
pub struct Faucet {
    pub zk_id: ZkPublicKey,
    pub funds: NoteValue,
}

pub struct GenesisTransferOp {
    transfer_op: TransferOp,
    outputs: Outputs,
}

impl GenesisTransferOp {
    pub fn new(stake_holders: impl Iterator<Item = StakeHolderInfo>, faucet: &Faucet) -> Self {
        let mut notes: Vec<Note> = stake_holders
            .map(|stake_holder| Note::new(stake_holder.stake, stake_holder.zk_id))
            .collect();

        notes.push(Note::new(faucet.funds, faucet.zk_id));

        let outputs = Outputs::new(notes);
        let transfer_op = TransferOp::new(Inputs::empty(), outputs.clone());

        Self {
            transfer_op,
            outputs,
        }
    }

    pub fn notes(&self) -> impl Iterator<Item = Note> {
        self.outputs.utxos(&self.transfer_op).map(|u| u.note)
    }

    pub fn utxos(&self) -> impl Iterator<Item = Utxo> {
        self.outputs.utxos(&self.transfer_op)
    }

    #[must_use]
    pub fn utxo_by_index(&self, index: usize) -> Option<Utxo> {
        self.outputs.utxo_by_index(index, &self.transfer_op)
    }
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
    faucet: &Faucet,
) -> Result<(GenesisTransferOp, Vec<SDPDeclareOp>), DistributionError>
where
    S: IntoIterator<Item = StakeHolderInfo> + Clone,
    P: IntoIterator<Item = ProviderInfo>,
{
    let stake_holder_keys: HashSet<ZkPublicKey> =
        stake_holders.clone().into_iter().map(|s| s.zk_id).collect();

    let transfer_op = GenesisTransferOp::new(stake_holders.into_iter(), faucet);
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

        if let Some(utxo) = transfer_op.utxos().find(|u| u.note.pk == provider.zk_id) {
            declarations.push(SDPDeclareOp {
                service_type: provider.service_type,
                locators: provider.locators,
                provider_id: provider.provider_id.into(),
                zk_id: provider.zk_id,
                locked_note_id: utxo.id(),
            });
        }
    }

    Ok((transfer_op, declarations))
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

        let faucet = Faucet {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(stake_holders, providers, &faucet);

        assert!(result.is_ok());
        let (transfer_op, declarations) = result.unwrap();
        let notes = transfer_op.notes().collect::<Vec<_>>();

        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].pk, zk_id_1);
        assert_eq!(notes[1].pk, zk_id_2);

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].zk_id, zk_id_1);
        assert_eq!(
            declarations[0].locked_note_id,
            transfer_op.utxo_by_index(0).unwrap().id(),
        );
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

        let faucet = Faucet {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(stake_holders, providers, &faucet);

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

        let faucet = Faucet {
            zk_id: mock_zk_pk(3),
            funds: 100_000,
        };

        let result = distribute(stake_holders, providers, &faucet);

        assert!(matches!(
            result,
            Err(DistributionError::NoteLockedForService(
                ServiceType::BlendNetwork
            ))
        ));
    }
}
