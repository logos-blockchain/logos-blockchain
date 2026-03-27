use std::{collections::HashSet, num::NonZero, sync::Arc};

use lb_core::{
    block::Block,
    mantle::{
        Note, SignedMantleTx, Utxo,
        ops::leader_claim::{VoucherCm, VoucherSecret},
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic, check_winning},
    sdp::ServiceParameters,
};
use lb_cryptarchia_engine::{EpochConfig, Slot};
use lb_cryptarchia_sync::HeaderId;
use lb_groth16::{Field as _, Fr};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
use lb_ledger::{
    LedgerState,
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use lb_utils::math::NonNegativeRatio;
use num_bigint::BigUint;
use rand::{RngCore as _, thread_rng};

use crate::Cryptarchia;

#[test]
fn cryptarchia_switch_to_online() {
    let k = NonZero::<u32>::new(1).unwrap();
    let config = ledger_config(k);

    let (zk_key, utxo) = utxo();
    let genesis_id: HeaderId = [0; 32].into();
    let mut cryptarchia = Cryptarchia::from_lib(
        genesis_id,
        LedgerState::from_utxos([utxo], &config),
        genesis_id,
        config,
        lb_cryptarchia_engine::State::Bootstrapping,
        Slot::new(0),
        0,
    );

    // Add 3 new blocks to the chain
    let mut block_ids = vec![genesis_id];
    let mut slot = Slot::new(1);
    while block_ids.len() < 4 {
        // TODO: Use a mock proof system instead of expensive real proof generation,
        // by refactoring `Cryptarchia`.
        let block = try_build_block(
            &cryptarchia,
            *block_ids.last().unwrap(),
            utxo,
            &zk_key,
            slot,
        )
        .expect("should find a winning slot");

        let (pruned_blocks, reorged_blocks) = cryptarchia
            .try_apply_block(&block, block.header().slot())
            .unwrap();
        // No block should be pruned since LIB is not updated during Bootstrapping
        assert!(pruned_blocks.is_empty());
        assert!(reorged_blocks.is_empty());

        block_ids.push(block.header().id());
        slot = block.header().slot() + 1;
    }

    // Now, the chain is [G, B1, B2, B3].
    // We now switch to Online and check that LIB advances to B2.
    let (cryptarchia, pruned_blocks) = cryptarchia.online();
    assert_eq!(cryptarchia.lib(), block_ids[2]);
    // All immutable blocks (G, B1, excluding LIB) should have been pruned
    assert_eq!(
        pruned_blocks
            .immutable_blocks()
            .values()
            .collect::<HashSet<_>>(),
        HashSet::from([&block_ids[0], &block_ids[1]])
    );

    // Check the ledger states of immutable blocks have been pruned
    assert!(cryptarchia.ledger.state(&block_ids[0]).is_none());
    assert!(cryptarchia.ledger.state(&block_ids[1]).is_none());
}

#[must_use]
fn ledger_config(security_param: NonZero<u32>) -> lb_ledger::Config {
    let mut service_params = std::collections::HashMap::new();
    service_params.insert(
        lb_core::sdp::ServiceType::BlendNetwork,
        ServiceParameters {
            lock_period: 10,
            inactivity_period: 1,
            retention_period: 1,
            timestamp: 0,
            session_duration: 10,
        },
    );

    lb_ledger::Config {
        epoch_config: EpochConfig {
            epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
            epoch_period_nonce_buffer: 3.try_into().unwrap(),
            epoch_period_nonce_stabilization: 4.try_into().unwrap(),
        },
        consensus_config: lb_cryptarchia_engine::Config::new(
            security_param,
            NonNegativeRatio::new(1, 10.try_into().unwrap()),
            1.0.try_into().unwrap(),
        ),
        sdp_config: lb_ledger::mantle::sdp::Config {
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_session: 10.try_into().unwrap(),
                    message_frequency_per_round: 1.0.try_into().unwrap(),
                    num_blend_layers: 3.try_into().unwrap(),
                    minimum_network_size: 1.try_into().unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: lb_core::sdp::MinStake {
                threshold: 1,
                timestamp: 0,
            },
        },
        faucet_pk: None,
    }
}

/// Builds a block by grinding through slots
fn try_build_block(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    utxo: Utxo,
    key: &ZkKey,
    start_slot: Slot,
) -> Option<Block<SignedMantleTx>> {
    let start_slot: u64 = start_slot.into();
    for slot in start_slot..=(start_slot + 1000) {
        let epoch_state = cryptarchia.epoch_state_for_slot(slot.into()).unwrap();
        let tip_state = cryptarchia.ledger.state(&cryptarchia.tip()).unwrap();
        let public_inputs = LeaderPublic::new(
            epoch_state.utxo_merkle_root(),
            tip_state.latest_utxos().root(),
            epoch_state.nonce,
            slot,
            epoch_state.lottery_0,
            epoch_state.lottery_1,
        );

        if !check_winning(utxo, public_inputs, &key.to_public_key(), *key.as_fr()) {
            continue;
        }

        let signing_key = Ed25519Key::generate(&mut thread_rng());
        let private_inputs = LeaderPrivate::new(
            public_inputs,
            utxo,
            &epoch_state.utxo_merkle_path(&utxo).unwrap(),
            &tip_state.latest_utxos().path(&utxo.id()).unwrap(),
            *key.as_fr(),
            &signing_key.public_key(),
        );
        let proof = Groth16LeaderProof::prove(
            private_inputs,
            VoucherCm::from_secret(VoucherSecret::from(Fr::ZERO)),
        )
        .unwrap();

        return Some(
            Block::create(
                parent,
                slot.into(),
                proof,
                Vec::<SignedMantleTx>::new(),
                &signing_key,
            )
            .unwrap(),
        );
    }

    None
}

fn utxo() -> (ZkKey, Utxo) {
    let tx_hash: Fr = BigUint::from(thread_rng().next_u64()).into();
    let zk_sk = ZkKey::from(Fr::ZERO);
    let utxo = Utxo {
        transfer_hash: tx_hash.into(),
        output_index: 0,
        note: Note::new(10000, zk_sk.to_public_key()),
    };
    (zk_sk, utxo)
}
