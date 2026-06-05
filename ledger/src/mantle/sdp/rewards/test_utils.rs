use std::marker::PhantomData;

use lb_core::{
    mantle::Note,
    sdp::{
        Declaration, DeclarationId, Locator, MinStake, ProviderId, ServiceParameters, ServiceType,
        locked_notes::LockedNotes,
    },
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{Field as _, Fr};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};
use num_bigint::BigUint;
use rpds::HashTrieMapSync;

use crate::{
    EpochState, UtxoTree,
    mantle::sdp::{SdpLedger, Service, ServiceState, rewards::blend},
};

pub fn create_epoch_state(
    provider_ids: &[ProviderId],
    service_type: ServiceType,
    epoch: Epoch,
    nonce: Fr,
    _params: &blend::RewardsParameters,
) -> EpochState {
    let mut declarations = rpds::RedBlackTreeMapSync::new_sync();
    let mut locked_notes = LockedNotes::new();
    for (i, provider_id) in provider_ids.iter().enumerate() {
        let note = Note::new(100, ZkPublicKey::zero());
        let note_id = Fr::from(i as u64).into();
        let declaration = Declaration {
            service_type,
            provider_id: *provider_id,
            locked_note_id: note_id,
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
            zk_id: ZkPublicKey::new(BigUint::from(i as u64).into()),
            created: 0.into(),
            active: 0.into(),
            withdrawn: None,
            nonce: 0,
        };
        declarations = declarations.insert(DeclarationId([i as u8; 32]), declaration);
        locked_notes = locked_notes
            .lock(
                &MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
                service_type,
                note,
                &note_id,
            )
            .unwrap();
    }

    let mut epoch_state = EpochState {
        epoch,
        nonce,
        utxos: UtxoTree::default(),
        total_stake: 0,
        lottery_0: Fr::ZERO,
        lottery_1: Fr::ZERO,
        sdp: SdpLedger::new(epoch),
    };

    let ledger = SdpLedger {
        services: HashTrieMapSync::new_sync().insert(
            ServiceType::BlendNetwork,
            Service::BlendNetwork(ServiceState {
                declarations,
                // TODO: enable after making `rewards` module stable
                // rewards: blend::Rewards::new(params, &epoch_state),
                _phantom: PhantomData,
            }),
        ),
        locked_notes,
        epoch,
    };

    epoch_state.sdp = ledger;
    epoch_state
}

pub fn create_provider_id(byte: u8) -> ProviderId {
    let key_bytes = [byte; 32];
    // Ensure the key is valid by using SigningKey
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    ProviderId(signing_key.public_key())
}

pub fn create_service_parameters() -> ServiceParameters {
    ServiceParameters {
        lock_period: 10.into(),
        inactivity_period: 1.into(),
        retention_period: 1.into(),
        epoch: 0.into(),
    }
}
