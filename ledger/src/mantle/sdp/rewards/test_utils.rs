use std::{collections::HashMap, sync::Arc};

use lb_core::sdp::{
    Declaration, DeclarationId, Declarations, Locator, ProviderId, ServiceParameters, ServiceType,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};
use num_bigint::BigUint;

use crate::{EpochState, UtxoTree, mantle::sdp::rewards::blend};

pub fn create_epoch_state(
    provider_ids: &[ProviderId],
    service_type: ServiceType,
    epoch: Epoch,
    nonce: Fr,
    _params: &blend::RewardsParameters,
) -> EpochState {
    let entries: HashMap<DeclarationId, Declaration> = provider_ids
        .iter()
        .enumerate()
        .map(|(i, provider_id)| {
            let note_id = Fr::from(i as u64).into();
            let declaration = Declaration {
                service_type,
                provider_id: *provider_id,
                locked_note_id: note_id,
                locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
                zk_id: ZkPublicKey::new(BigUint::from(i as u64).into()),
                created: 0.into(),
                active: 2.into(),
                withdrawn: None,
                nonce: 0,
            };
            (DeclarationId([i as u8; 32]), declaration)
        })
        .collect();

    let active_declarations: Declarations = HashMap::from([(service_type, entries)]).into();

    EpochState {
        epoch,
        nonce,
        utxos: UtxoTree::default(),
        total_stake: 0,
        lottery_0: Fr::ZERO,
        lottery_1: Fr::ZERO,
        active_declarations: Arc::new(active_declarations),
    }
}

pub fn create_provider_id(byte: u8) -> ProviderId {
    let key_bytes = [byte; 32];
    // Ensure the key is valid by using SigningKey
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    ProviderId(signing_key.public_key())
}

pub fn create_service_parameters() -> ServiceParameters {
    ServiceParameters {
        inactivity_period: 1.into(),
        retention_period: 1.into(),
        epoch: 0.into(),
    }
}
