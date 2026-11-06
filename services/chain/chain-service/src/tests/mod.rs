use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Display},
    num::NonZero,
    sync::Arc,
};

use futures::StreamExt as _;
use lb_binary_codec::canonical::BinaryEncode as _;
use lb_core::{
    block::{
        Block, BlockTransactions, UncleHeaders, body_root,
        genesis::{GenesisBlock, GenesisBlockBuilder},
    },
    crypto::ZkHasher,
    header::{ContentId, Header, block_id, pol_msg},
    mantle::{
        GenesisParameters, GenesisTime, Note, Op, OpProof, SignedOps, Value,
        channel::Channels,
        gas::{MainnetGasProfile, TxGasCalculator as _},
        ledger::verification_mode::StandardMode,
        note_set::{CommitmentMmr, LEDGER_SET},
        notes::{NoteCommitment, OutputCommitments, derive_note_nullifier},
        ops::{
            ZkTransferProof,
            channel::{
                ChannelId, MsgId, NULL_CHANNEL_ID,
                config::ChannelConfigOp,
                inscribe::{Inscription, InscriptionOp, MiniBlocks},
            },
            transfer::{TransferOp, prove_transfer},
        },
        traits::Hashable as _,
        transactions::{
            GasPrices, GenesisAllocation, OpProofs, Ops,
            states::{Preverified, Unverified},
            tx_list::ops::OpsGasContext,
        },
    },
    proofs::{
        channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
        leader_proof::{
            Groth16LeaderProof, LeaderPrivate, LeaderProof, LeaderPublic, check_winning,
            pol_wallet_inputs,
        },
    },
    sdp::ServiceParameters,
};
use lb_cryptarchia_engine::{EpochConfig, EraSchedule, Slot, UncleSlots};
use lb_cryptarchia_sync::HeaderId;
use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof, Fr};
use lb_key_management_system_keys::keys::{
    Ed25519Key, UnverifiedEd25519PublicKey, ZkKey, ZkPublicKey,
};
use lb_ledger::{
    GENESIS_STAKE_SUPPLY, LedgerState,
    config::{BlendPoWConfig, EraScheduledConfig, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use lb_mmr::MerklePath;
use lb_storage_service::{
    StorageMsg, StorageService,
    api::StorageApi,
    backend::StorageBackend as _,
    rocksdb::{RocksBackend, RocksBackendSettings},
};
use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraTiming};
use lb_time_service::backends::SystemTimeBackend;
use lb_utils::math::NonNegativeRatio;
use overwatch::services::{AsServiceId, relay::OutboundRelay};
use rand::{RngCore as _, thread_rng};
use tempfile::TempDir;
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

use crate::{
    BlockOrigin, BootstrapConfig, Cryptarchia, CryptarchiaConsensus, CryptarchiaConsensusState,
    Error, OfflineGracePeriodConfig,
    relays::CryptarchiaConsensusRelays,
    service::{get_block_ids, process_block},
};

#[test]
fn cryptarchia_switch_to_online() {
    let k = NonZero::<u32>::new(1).unwrap();
    let config = ledger_config(k);

    let (secret_key, note) = note();
    let genesis_id: HeaderId = [0; 32].into();
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes([note], &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );

    // Add 3 new blocks to the chain
    let mut block_ids = vec![genesis_id];
    let mut slot = Slot::new(1);
    while block_ids.len() < 4 {
        // TODO: Use a mock proof system instead of expensive real proof generation,
        // by refactoring `Cryptarchia`.
        let (block, _) = try_build_block(
            &cryptarchia,
            *block_ids.last().unwrap(),
            &note,
            secret_key,
            slot,
            UncleHeaders::empty(),
        )
        .expect("should find a winning slot");

        let block_header_id = block.header().id();
        let block_header_slot = block.header().slot();
        let (pruned_blocks, reorged_blocks, _) = cryptarchia
            .try_apply_block(block, block_header_slot)
            .unwrap();
        // No block should be pruned since LIB is not updated during Bootstrapping
        assert!(pruned_blocks.is_empty());
        assert!(reorged_blocks.is_empty());

        block_ids.push(block_header_id);
        slot = block_header_slot.strict_add(1.into());
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

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "better to have one comprehensive test"
)]
async fn get_block_ids_from_memory_and_storage() {
    // Init dummy relays for chain service
    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_svc = spawn_storage_service(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<_>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);

    // Init `Cryptarchia`
    let k = 3.try_into().unwrap();
    let config = ledger_config(k);
    let genesis_id = [0; 32].into();
    let (secret_key, note) = note();
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes([note], &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Online,
    );

    // Add 2 blocks (not finalized yet since k=3)
    let mut slot = Slot::genesis().strict_add(1.into());
    let mut block_ids = vec![genesis_id];
    for _ in 0..2 {
        let (block, _) = try_build_block(
            &cryptarchia,
            cryptarchia.tip(),
            &note,
            secret_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        process_block(
            &mut cryptarchia,
            block.clone(),
            block.header().slot(),
            BlockOrigin::Network,
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
        .unwrap();
        block_ids.push(block.header().id());
        slot = block.header().slot().strict_add(1.into());
    }

    // get_block_ids when all blocks are in memory.
    let mut stream = get_block_ids(
        &cryptarchia,
        block_ids[2],
        block_ids[0],
        relays.storage().clone(),
    );
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[2]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[1]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[0]);
    assert!(stream.next().await.is_none());

    // Hitting genesis before reaching `to_ancestor`
    let mut stream = get_block_ids(
        &cryptarchia,
        block_ids[2],
        [99; 32].into(), // unknown block ID
        relays.storage().clone(),
    );
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[2]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[1]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[0]);
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(Error::ParentIdNotFound(_))
    ));

    // Add 3 more blocks.
    // Now G, b1 are in storage, and b2~5 are in memory.
    for _ in 0..3 {
        let (block, _) = try_build_block(
            &cryptarchia,
            cryptarchia.tip(),
            &note,
            secret_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        process_block(
            &mut cryptarchia,
            block.clone(),
            block.header().slot(),
            BlockOrigin::Network,
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
        .unwrap();
        block_ids.push(block.header().id());
        slot = block.header().slot().strict_add(1.into());
    }

    // All blocks are loaded from memory + storage.
    let mut stream = get_block_ids(
        &cryptarchia,
        block_ids[5],
        block_ids[0],
        relays.storage().clone(),
    );
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[5]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[4]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[3]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[2]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[1]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[0]);
    assert!(stream.next().await.is_none());

    // Hitting genesis in storage before reaching `to_ancestor`
    let mut stream = get_block_ids(
        &cryptarchia,
        block_ids[1],
        [99; 32].into(), // unknown block ID
        relays.storage().clone(),
    );
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[1]);
    assert_eq!(stream.next().await.unwrap().unwrap(), block_ids[0]);
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(Error::ParentIdNotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_blocks_fall_back_to_lib_when_tip_missing_from_storage() {
    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_svc = spawn_storage_service(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<SignedOps<Preverified, StandardMode>>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );

    let lib = [0; 32].into();
    let missing_tip = [1; 32].into();

    let recovery_blocks = CryptarchiaConsensus::<_, SystemTimeBackend, TestRuntimeServiceId>::load_recovery_blocks_or_fall_back_to_lib(
        missing_tip, lib, relays.storage().clone()
    )
    .await;

    assert!(recovery_blocks.fell_back_to_lib);
    assert_eq!(recovery_blocks.blocks, []);
}

/// The chain must be recovered successfully from storage by skipping the uncle
/// validation.
///
/// Build a chain:
/// G -- B1 -- B2(uncles=[U1])
///    \
///      U1
/// and recover the chain from the state persisted with `LIB = B1` and
/// `tip = B2`, where `U1`'s parent `G` is older than LIB.
#[tokio::test(flavor = "multi_thread")]
async fn recovery_chain_with_uncle_whose_parent_is_older_than_lib() {
    type Tx = SignedOps<Preverified, StandardMode>;

    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_svc = spawn_storage_service(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<Tx>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);

    let (cryptarchia, b1, u1, _, secret_key, note) = chain_with_fork();
    let (b2, _) = try_build_block(
        &cryptarchia,
        cryptarchia.tip(),
        &note,
        secret_key,
        u1.header().slot().strict_add(1.into()),
        UncleHeaders::try_new(vec![signed_header(&u1)], 4).unwrap(),
    )
    .unwrap();
    let (b1_id, b2_id, b2_slot) = (b1.header().id(), b2.header().id(), b2.header().slot());

    // Before the restart: `B1` and `B2` are verified and stored.
    let mut stored = genesis_cryptarchia(note);
    for block in [b1.clone(), b2] {
        process_block(
            &mut stored,
            block,
            b2_slot,
            BlockOrigin::Network,
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
        .expect("the block should be valid before the restart");
    }

    let recovery_state = CryptarchiaConsensusState {
        tip: b2_id,
        lib: b1_id,
        lib_ledger_state: stored.ledger.state(&b1_id).unwrap().clone(),
        lib_block_length: 1,
        lib_block_slot: b1.header().slot(),
        lib_block_uncle_slots: UncleSlots::default(),
        genesis_id: GENESIS_ID.into(),
        storage_blocks_to_remove: HashSet::new(),
        last_engine_state: None,
    };
    let bootstrap_config = BootstrapConfig {
        prolonged_bootstrap_period: std::time::Duration::ZERO,
        force_bootstrap: true,
        offline_grace_period: OfflineGracePeriodConfig::default(),
    };
    let initialized = CryptarchiaConsensus::<
        Tx,
        SystemTimeBackend,
        TestRuntimeServiceId,
    >::initialize_cryptarchia(
        &recovery_state,
        &bootstrap_config,
        Arc::clone(stored.ledger.era_schedule()),
        &relays,
        &new_block_tx,
        &lib_tx,
        b2_slot,
    )
    .await;
    let initialized = initialized.expect("the chain should be recovered");
    assert!(!initialized.fell_back_to_lib);
    assert_eq!(initialized.cryptarchia.lib(), b1_id);
    assert_eq!(initialized.cryptarchia.tip(), b2_id);
}

/// The chain must be recovered successfully from storage even if the current
/// slot sampled at startup is behind the slot of the stored tip.
///
/// Build a chain `G -- B1 -- B2`, and recover it with `LIB = B1`, `tip = B2`
/// and `current_slot = B2.slot - 1`.
#[tokio::test(flavor = "multi_thread")]
async fn recovery_chain_with_tip_ahead_of_current_slot() {
    type Tx = SignedOps<Preverified, StandardMode>;

    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_svc = spawn_storage_service(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<Tx>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);

    // Before the restart: `B1` and `B2` are verified and stored.
    let (secret_key, note) = note();
    let mut stored = genesis_cryptarchia(note);
    let mut blocks = Vec::new();
    let mut slot = Slot::genesis().strict_add(1.into());
    for _ in 0..2 {
        let (block, _) = try_build_block(
            &stored,
            stored.tip(),
            &note,
            secret_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        slot = block.header().slot().strict_add(1.into());
        process_block(
            &mut stored,
            block.clone(),
            block.header().slot(),
            BlockOrigin::Network,
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
        .unwrap();
        blocks.push(block);
    }
    let (b1, b2) = (&blocks[0], &blocks[1]);
    let (b1_id, b2_id) = (b1.header().id(), b2.header().id());
    let current_slot = b2.header().slot().checked_sub(1.into()).unwrap();

    let recovery_state = CryptarchiaConsensusState {
        tip: b2_id,
        lib: b1_id,
        lib_ledger_state: stored.ledger.state(&b1_id).unwrap().clone(),
        lib_block_length: 1,
        lib_block_slot: b1.header().slot(),
        lib_block_uncle_slots: UncleSlots::default(),
        genesis_id: GENESIS_ID.into(),
        storage_blocks_to_remove: HashSet::new(),
        last_engine_state: None,
    };
    let bootstrap_config = BootstrapConfig {
        prolonged_bootstrap_period: std::time::Duration::ZERO,
        force_bootstrap: true,
        offline_grace_period: OfflineGracePeriodConfig::default(),
    };
    let initialized = CryptarchiaConsensus::<
        Tx,
        SystemTimeBackend,
        TestRuntimeServiceId,
    >::initialize_cryptarchia(
        &recovery_state,
        &bootstrap_config,
        Arc::clone(stored.ledger.era_schedule()),
        &relays,
        &new_block_tx,
        &lib_tx,
        current_slot,
    )
    .await
    .expect("the chain should be recovered");
    assert!(!initialized.fell_back_to_lib);
    assert_eq!(initialized.cryptarchia.lib(), b1_id);
    assert_eq!(initialized.cryptarchia.tip(), b2_id);
}

/// A block from the network must still be rejected if its slot is ahead of the
/// current slot, while the same block replayed from storage is accepted.
#[test]
fn future_block_is_rejected_only_from_network() {
    let (cryptarchia, block) = test_chain_with_next_block();
    let block_slot = block.header().slot();
    let current_slot = block_slot.checked_sub(1.into()).unwrap();

    let result = cryptarchia.clone().try_apply_block_with_state_retention(
        block.clone(),
        current_slot,
        BlockOrigin::Network,
    );
    assert!(matches!(
        result,
        Err(Error::FutureBlock { block_slot: s, current_slot: c })
            if s == block_slot && c == current_slot
    ));

    let mut cryptarchia = cryptarchia;
    cryptarchia
        .try_apply_block_with_state_retention(block.clone(), current_slot, BlockOrigin::Storage)
        .expect("a stored block should be replayed regardless of the current slot");
    assert_eq!(cryptarchia.tip(), block.header().id());
}

/// Header traversal must continue from storage past a recovered LIB, instead of
/// treating the LIB as genesis.
///
/// Build a chain `G -- B1 -- B2`, recover it with `LIB = B1` and `tip = B2`,
/// and fetch the block IDs from `B2` to `G`.
#[tokio::test(flavor = "multi_thread")]
async fn get_block_ids_past_recovered_lib() {
    type Tx = SignedOps<Preverified, StandardMode>;

    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_svc = spawn_storage_service(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<Tx>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);

    // Before the restart: `B1` and `B2` are verified and stored.
    let (secret_key, note) = note();
    let mut stored = genesis_cryptarchia(note);
    let mut blocks = Vec::new();
    let mut slot = Slot::genesis().strict_add(1.into());
    for _ in 0..2 {
        let (block, _) = try_build_block(
            &stored,
            stored.tip(),
            &note,
            secret_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        slot = block.header().slot().strict_add(1.into());
        process_block(
            &mut stored,
            block.clone(),
            block.header().slot(),
            BlockOrigin::Network,
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
        .unwrap();
        blocks.push(block);
    }
    let (b1, b2) = (&blocks[0], &blocks[1]);
    let (b1_id, b2_id) = (b1.header().id(), b2.header().id());

    let recovery_state = CryptarchiaConsensusState {
        tip: b2_id,
        lib: b1_id,
        lib_ledger_state: stored.ledger.state(&b1_id).unwrap().clone(),
        lib_block_length: 1,
        lib_block_slot: b1.header().slot(),
        lib_block_uncle_slots: UncleSlots::default(),
        genesis_id: GENESIS_ID.into(),
        storage_blocks_to_remove: HashSet::new(),
        last_engine_state: None,
    };
    let bootstrap_config = BootstrapConfig {
        prolonged_bootstrap_period: std::time::Duration::ZERO,
        force_bootstrap: true,
        offline_grace_period: OfflineGracePeriodConfig::default(),
    };
    let initialized = CryptarchiaConsensus::<
        Tx,
        SystemTimeBackend,
        TestRuntimeServiceId,
    >::initialize_cryptarchia(
        &recovery_state,
        &bootstrap_config,
        Arc::clone(stored.ledger.era_schedule()),
        &relays,
        &new_block_tx,
        &lib_tx,
        slot,
    )
    .await
    .expect("the chain should be recovered");
    let cryptarchia = initialized.cryptarchia;
    assert_eq!(cryptarchia.lib(), b1_id);
    assert_eq!(cryptarchia.lib_branch().parent(), GENESIS_ID.into());

    let block_ids = get_block_ids(
        &cryptarchia,
        b2_id,
        GENESIS_ID.into(),
        relays.storage().clone(),
    )
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .expect("the traversal should continue from storage past LIB");
    assert_eq!(block_ids, vec![b2_id, b1_id, GENESIS_ID.into()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn process_block_does_not_mutate_state_when_storage_send_fails() {
    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    // Close the storage relay before processing so the initial combined store
    // request fails.
    drop(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<SignedOps<Preverified, StandardMode>>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );
    let (new_block_tx, mut new_block_rx) = broadcast::channel(10);
    let (lib_tx, mut lib_rx) = broadcast::channel(10);

    let (mut cryptarchia, block) = test_chain_with_next_block();
    let initial_info = cryptarchia.info();
    let block_slot = block.header().slot();

    let result = process_block(
        &mut cryptarchia,
        block,
        block_slot,
        BlockOrigin::Network,
        &relays,
        &new_block_tx,
        &lib_tx,
    )
    .await;

    match &result {
        Err(Error::Storage(_)) => {}
        Err(e) => panic!("expected storage error, got {e:?}"),
        Ok(_) => panic!("expected storage error, got Ok"),
    }
    assert_eq!(cryptarchia.info(), initial_info);
    assert!(new_block_rx.try_recv().is_err());
    assert!(lib_rx.try_recv().is_err());
}

/// A fork switch reports the transactions of every block that entered the
/// canonical chain, and reorgs only the transactions of the displaced blocks
/// that the adopted branch does not carry.
#[tokio::test(flavor = "multi_thread")]
async fn fork_switch_reports_newly_canonical_and_reorged_transactions() {
    // G - b1(tx1, tx3)       (local chain)
    //   \
    //    f1(tx1) - f2(tx2)   (f2 becomes the new tip)
    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_svc = spawn_storage_service(storage_rx);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let relays = CryptarchiaConsensusRelays::<_>::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    );
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);

    let config = ledger_config(3.try_into().unwrap());
    let genesis_id: HeaderId = [0; 32].into();
    let (secret_key, leader_note) = note();
    let (funding_key_a, funding_note_a) = note();
    let (funding_key_b, funding_note_b) = note();
    let (funding_key_c, funding_note_c) = note();
    let genesis_notes = [leader_note, funding_note_a, funding_note_b, funding_note_c];
    let paths = commitment_paths(&genesis_notes);
    let genesis = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes(genesis_notes, &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Online,
    );
    let tx1 = burn_tx(&funding_note_a, funding_key_a, &paths[1]);
    let tx2 = burn_tx(&funding_note_b, funding_key_b, &paths[2]);
    let tx3 = burn_tx(&funding_note_c, funding_key_c, &paths[3]);

    // Build the fork on its own view, where each fork block is the tip when
    // its child is built.
    let mut fork_view = genesis.clone();
    let (f1, _) = try_build_block_with_transactions(
        &fork_view,
        genesis_id,
        &leader_note,
        secret_key,
        &paths[0],
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::from([tx1.clone()]),
    )
    .unwrap();
    fork_view
        .try_apply_block(f1.clone(), f1.header().slot())
        .unwrap();
    let (f2, _) = try_build_block_with_transactions(
        &fork_view,
        f1.header().id(),
        &leader_note,
        secret_key,
        &paths[0],
        f1.header().slot().strict_add(1.into()),
        UncleHeaders::empty(),
        BlockTransactions::from([tx2.clone()]),
    )
    .unwrap();

    let mut cryptarchia = genesis;
    let (b1, _) = try_build_block_with_transactions(
        &cryptarchia,
        genesis_id,
        &leader_note,
        secret_key,
        &paths[0],
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::from([tx1.clone(), tx3.clone()]),
    )
    .unwrap();

    let mut apply = async |block: Block<SignedOps<Preverified, StandardMode>>| {
        let slot = block.header().slot();
        process_block(
            &mut cryptarchia,
            block,
            slot,
            BlockOrigin::Network,
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
        .unwrap()
    };

    let (b1_slot, f1_slot, f2_slot) = (b1.header().slot(), f1.header().slot(), f2.header().slot());

    let outcome = apply(b1).await;
    assert_eq!(
        outcome.newly_canonical_txs,
        HashMap::from([(tx1.hash(), b1_slot), (tx3.hash(), b1_slot)])
    );
    assert_eq!(outcome.reorged_txs, vec![]);

    // f1 only ties b1, so the local chain is kept and nothing enters it.
    let outcome = apply(f1).await;
    assert_eq!(outcome.newly_canonical_txs, HashMap::new());
    assert_eq!(outcome.reorged_txs, vec![]);

    // tx1 is carried by both branches, so only tx3 leaves the canonical chain.
    let outcome = apply(f2.clone()).await;
    assert_eq!(
        outcome.newly_canonical_txs,
        HashMap::from([(tx1.hash(), f1_slot), (tx2.hash(), f2_slot)])
    );
    assert_eq!(outcome.reorged_txs, vec![tx3]);
    assert_eq!(cryptarchia.tip(), f2.header().id());
}

#[test]
fn ledger_is_not_committed_if_block_contains_invalid_deferred_proof() {
    let config = ledger_config(NonZero::<u32>::new(1).unwrap());
    let (secret_key, note) = note();
    let genesis_id: HeaderId = [0; 32].into();
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes([note], &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );

    let fake_key = ZkKey::from(Fr::from(42u64));
    assert_ne!(fake_key.to_public_key(), note.public_key);
    let (block, _) = try_build_block_with_transactions(
        &cryptarchia,
        genesis_id,
        &note,
        secret_key,
        &commitment_paths(&[note])[0],
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::from([transfer_tx_with_fake_sig(&note, &fake_key)]),
    )
    .expect("should find a winning slot");

    let block_header = block.header().clone();
    let result = cryptarchia.try_apply_block(block, block_header.slot());
    assert!(matches!(
        result,
        Err(Error::BatchDeferredProofVerification(_))
    ));
    assert!(
        cryptarchia.ledger.state(&block_header.id()).is_none(),
        "ledger state should not be committed"
    );
    assert_eq!(cryptarchia.tip(), genesis_id, "tip should not advance");
}

/// Block Header Validation: a copy of a block failing an Operation proof shares
/// the ID of the genuine block, which stays applicable.
#[test]
fn a_copy_failing_an_operation_proof_leaves_the_genuine_block_applicable() {
    let config = ledger_config(NonZero::<u32>::new(1).unwrap());
    let genesis_id: HeaderId = [0; 32].into();
    let (secret_key, leader_note) = note();
    let (funding_key, funding_note) = note();
    let genesis_notes = [leader_note, funding_note];
    let paths = commitment_paths(&genesis_notes);
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes(genesis_notes, &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );

    let (block, _) = try_build_block_with_transactions(
        &cryptarchia,
        genesis_id,
        &leader_note,
        secret_key,
        &paths[0],
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::from([burn_tx(&funding_note, funding_key, &paths[1])]),
    )
    .expect("should find a winning slot");
    let id = block.header().id();
    let slot = block.header().slot();

    let tampered_proof = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(
        CompressedGroth16Proof::from_bytes(&[0u8; 128]),
    ))]);
    let copy: Block<SignedOps<Preverified, StandardMode>> = Block::reconstruct(
        block.header().clone(),
        UncleHeaders::empty(),
        BlockTransactions::from([SignedOps::from_parts(
            burn_ops(&funding_note, funding_key, &paths[1]),
            tampered_proof,
        )
        .unwrap()
        .preverify()
        .unwrap()]),
    )
    .expect("the body root does not cover the Operation proofs");
    assert_eq!(copy.header().id(), id);

    let Err(err) = cryptarchia.try_apply_block(copy, slot) else {
        panic!("expected the copy to be rejected");
    };
    assert!(matches!(
        crate::api::apply_block_api_error(err, id),
        crate::api::ApiError::InvalidOperationProof(block_id) if block_id == id
    ));
    assert_eq!(cryptarchia.tip(), genesis_id);

    cryptarchia
        .try_apply_block(block, slot)
        .expect("the genuine block must still apply");
    assert_eq!(cryptarchia.tip(), id);
}

/// The length of a channel configuration proof enters the storage fee, and so
/// the reward the block ID covers, so a relayer must not be able to lengthen
/// it. A copy with one genuine signature too many fails as a bad copy, before
/// the reward is compared, and the genuine block stays applicable.
#[test]
fn a_copy_with_one_signature_too_many_leaves_the_genuine_block_applicable() {
    let config = ledger_config(NonZero::<u32>::new(1).unwrap());
    let genesis_id: HeaderId = [0; 32].into();
    let (secret_key, leader_note) = note();
    let (create_key, create_note) = note();
    let (update_key, update_note) = note();
    let genesis_notes = [leader_note, create_note, update_note];
    let paths = commitment_paths(&genesis_notes);
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes(genesis_notes, &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );

    // Two keys accredited and a threshold of one, so that a second signature
    // passes every check but the count.
    let channel_keys = [
        Ed25519Key::from_bytes(&[1; 32]),
        Ed25519Key::from_bytes(&[2; 32]),
    ];
    let config_op = |parent| {
        ChannelConfigOp::new(
            ChannelId::from([3; 32]),
            parent,
            [channel_keys[0].public_key(), channel_keys[1].public_key()].into(),
            0.into(),
            0.into(),
            NonZero::<u16>::MIN,
        )
        .unwrap()
    };

    let create_op = config_op(MsgId::root());
    let (parent, _) = try_build_block_with_transactions(
        &cryptarchia,
        genesis_id,
        &leader_note,
        secret_key,
        &paths[0],
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::from([channel_config_tx(
            &create_note,
            create_key,
            &paths[1],
            create_op.clone(),
            &[],
        )]),
    )
    .expect("should find a winning slot");
    let parent_id = parent.header().id();
    let parent_slot = parent.header().slot();
    cryptarchia.try_apply_block(parent, parent_slot).unwrap();

    let update_tx = |signers: &[&Ed25519Key]| {
        channel_config_tx(
            &update_note,
            update_key,
            &paths[2],
            config_op(create_op.id()),
            signers,
        )
    };
    let (block, _) = try_build_block_with_transactions(
        &cryptarchia,
        parent_id,
        &leader_note,
        secret_key,
        &paths[0],
        parent_slot.strict_add(1.into()),
        UncleHeaders::empty(),
        BlockTransactions::from([update_tx(&[&channel_keys[0]])]),
    )
    .expect("should find a winning slot");
    let id = block.header().id();
    let slot = block.header().slot();

    let copy: Block<SignedOps<Preverified, StandardMode>> = Block::reconstruct(
        block.header().clone(),
        UncleHeaders::empty(),
        BlockTransactions::from([update_tx(&[&channel_keys[0], &channel_keys[1]])]),
    )
    .expect("the body root does not cover the Operation proofs");
    assert_eq!(copy.header().id(), id);

    let Err(err) = cryptarchia.try_apply_block(copy, slot) else {
        panic!("expected the copy to be rejected");
    };
    assert!(matches!(
        crate::api::apply_block_api_error(err, id),
        crate::api::ApiError::InvalidOperationProof(block_id) if block_id == id
    ));
    assert_eq!(cryptarchia.tip(), parent_id);

    cryptarchia
        .try_apply_block(block, slot)
        .expect("the genuine block must still apply");
    assert_eq!(cryptarchia.tip(), id);
}

/// Block Proposal Validation: an unauthenticated proposal is discarded from its
/// header alone, leaving the chain untouched.
#[test]
fn proposal_header_check_classifies_without_touching_the_chain() {
    let config = ledger_config(NonZero::<u32>::new(1).unwrap());
    let genesis_id: HeaderId = [0; 32].into();
    let (secret_key, note) = note();
    let cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes([note], &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );
    let (block, _) = try_build_block(
        &cryptarchia,
        genesis_id,
        &note,
        secret_key,
        Slot::new(1),
        UncleHeaders::empty(),
    )
    .expect("should find a winning slot");
    let header = block.header();
    let slot = header.slot();
    let check = |header: &Header, current_slot: Slot| {
        cryptarchia
            .verify_proposal_header(header, current_slot)
            .map_err(|err| crate::api::apply_block_api_error(err, header.id()))
    };

    check(header, slot).expect("the genuine header must pass");
    assert_eq!(cryptarchia.tip(), genesis_id);

    // The `msg` of the proof covers the body root, so this header fails on the
    // proof alone: its parent and slot are those of the genuine one.
    let forged = Header::new(
        genesis_id,
        ContentId::from([1; 32]),
        slot,
        header.leader_proof().clone(),
    );
    assert!(matches!(
        check(&forged, slot),
        Err(crate::api::ApiError::InvalidLeaderProof(id)) if id == forged.id()
    ));

    let unknown_parent: HeaderId = [1; 32].into();
    let orphan = Header::new(
        unknown_parent,
        *header.body_root(),
        slot,
        header.leader_proof().clone(),
    );
    assert!(matches!(
        check(&orphan, slot),
        Err(crate::api::ApiError::ParentMissing { parent, .. }) if parent == unknown_parent
    ));

    // The forged header reaches this error only if the wallclock check runs
    // before the proof.
    assert!(matches!(
        check(&forged, Slot::genesis()),
        Err(crate::api::ApiError::FutureBlock { block_slot, current_slot })
            if block_slot == slot && current_slot == Slot::genesis()
    ));
}

/// Creates a transfer tx with a fake sig.
fn transfer_tx_with_fake_sig(
    note: &Note,
    fake_key: &ZkKey,
) -> SignedOps<Preverified, StandardMode> {
    let mut output_note = Note::new(1, Fr::ZERO, fake_key.to_public_key());
    let gas_context = OpsGasContext::from_channels(&Channels::new(), GasPrices::default());
    let fees = transfer_tx(note, output_note, fake_key, 0)
        .op_refs()
        .total_gas_cost::<MainnetGasProfile>(&gas_context)
        .unwrap();
    output_note.value = note.value - fees.into_inner();

    transfer_tx(note, output_note, fake_key, fees.into_inner())
        .preverify()
        .expect("a fake signature is only caught by the stateful checks")
}

/// The Operation spending `note` with no outputs.
fn burn_op(note: &Note, secret_key: Fr, path: &MerklePath) -> Op {
    Op::Transfer(TransferOp::new(
        [derive_note_nullifier(
            &note.commitment(),
            &ZkKey::new(secret_key).nullifier_key(),
        )]
        .into(),
        OutputCommitments::default(),
        path.root::<ZkHasher>(note.commitment().0),
        note.value,
    ))
}

/// The Operations of a tx spending `note` with no outputs.
fn burn_ops(note: &Note, secret_key: Fr, path: &MerklePath) -> Ops {
    Ops::from([burn_op(note, secret_key, path)])
}

/// Creates a tx spending `note` to pay for `config_op`, signed by `signers`
/// with the channel key indexes of their positions.
fn channel_config_tx(
    note: &Note,
    secret_key: Fr,
    path: &MerklePath,
    config_op: ChannelConfigOp,
    signers: &[&Ed25519Key],
) -> SignedOps<Preverified, StandardMode> {
    let ops = Ops::from([
        burn_op(note, secret_key, path),
        Op::ChannelConfig(config_op),
    ]);
    let tx_hash = ops.hash();
    let signatures = IndexedSignatures::try_from_iter(
        (0..)
            .zip(signers)
            .map(|(index, key)| (index, key.sign_payload(tx_hash.as_signing_bytes()))),
    )
    .unwrap();
    let op_proofs = OpProofs::from([
        OpProof::ZkTransfer(ZkTransferProof::new(prove_transfer(
            &[(*note, secret_key, path.clone())],
            &[],
            path.root::<ZkHasher>(note.commitment().0),
            tx_hash.to_fr(),
        ))),
        OpProof::ChannelMultiSigProof(ChannelMultiSigProof::new(signatures)),
    ]);
    SignedOps::from_parts(ops, op_proofs)
        .unwrap()
        .preverify()
        .unwrap()
}

/// Creates a tx spending `note` with no outputs, so its whole value pays the
/// fee.
fn burn_tx(note: &Note, secret_key: Fr, path: &MerklePath) -> SignedOps<Preverified, StandardMode> {
    let root = path.root::<ZkHasher>(note.commitment().0);
    let ops = burn_ops(note, secret_key, path);
    let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(prove_transfer(
        &[(*note, secret_key, path.clone())],
        &[],
        root,
        ops.hash().to_fr(),
    )))]);
    SignedOps::from_parts(ops, op_proofs)
        .unwrap()
        .preverify()
        .unwrap()
}

// Spends the note of a one note genesis by the nullifier `key` derives, with a
// zero proof: `key` cannot prove the spend, only the deferred proof check
// catches it.
fn transfer_tx(
    note: &Note,
    output_note: Note,
    key: &ZkKey,
    excess_value: Value,
) -> SignedOps<Unverified, StandardMode> {
    let transfer_op = TransferOp::new(
        [derive_note_nullifier(
            &note.commitment(),
            &key.nullifier_key(),
        )]
        .into(),
        [output_note.commitment()].into(),
        commitment_paths(std::slice::from_ref(note))[0].root::<ZkHasher>(note.commitment().0),
        excess_value,
    );
    let ops = Ops::from([Op::Transfer(transfer_op)]);
    let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(
        CompressedGroth16Proof::from_bytes(&[0u8; 128]),
    ))]);
    SignedOps::from_parts(ops, op_proofs).expect("")
}

fn test_chain_with_next_block() -> (Cryptarchia, Block<SignedOps<Preverified, StandardMode>>) {
    let k = 3.try_into().unwrap();
    let config = ledger_config(k);
    let genesis_id = [0; 32].into();
    let (secret_key, note) = note();
    let cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_notes([note], &config, 0),
        single_era(config),
        lb_cryptarchia_engine::State::Online,
    );
    let (block, _) = try_build_block(
        &cryptarchia,
        cryptarchia.tip(),
        &note,
        secret_key,
        Slot::new(1),
        UncleHeaders::empty(),
    )
    .unwrap();

    (cryptarchia, block)
}

/// A reward config with claiming disabled, standing in for a real deployment
/// config in tests.
fn disabled_reward_config() -> RewardPoWConfig {
    RewardPoWConfig {
        minimum_difficulty: ModulusShift::new::<26>(),
        ema_smoothing_factor: 9,
        ema_smoothing_precision: core::num::NonZeroU64::new(10).unwrap(),
        target_claims_per_block: 100,
        rate_num: 0,
        rate_den: core::num::NonZeroU64::MIN,
        target_claim_per_block: core::num::NonZeroU64::MIN,
        pow_share: 0,
        share_den: core::num::NonZeroU64::MIN,
        slot_window: core::num::NonZeroU64::new(100).unwrap(),
    }
}

/// `genesis` from genesis, then each config of `after_genesis` from the epoch
/// it is paired with.
#[must_use]
pub fn schedule(
    genesis: lb_ledger::Config,
    after_genesis: impl IntoIterator<Item = (u32, lb_ledger::Config)>,
) -> Arc<EraScheduledConfig> {
    let timing = |config: &lb_ledger::Config| EraTiming {
        slot_duration: std::time::Duration::from_secs(1),
        epoch_length: EpochLength::epoch_length(config),
    };
    let after_genesis = after_genesis.into_iter().collect::<Vec<_>>();
    let timings_after_genesis =
        EraEntriesAfterGenesis::try_from_iter(after_genesis.iter().map(|(first_epoch, config)| {
            (
                NonZero::new(*first_epoch).expect("an era after genesis starts after epoch 0"),
                timing(config),
            )
        }))
        .expect("each epoch starts one era at most");
    let configs = std::iter::once(genesis)
        .chain(after_genesis.into_iter().map(|(_, config)| config))
        .collect::<Vec<_>>();
    Arc::new(
        EraSchedule::new(
            time::OffsetDateTime::UNIX_EPOCH,
            timing(&configs[0]),
            timings_after_genesis,
        )
        .expect("the eras resolve")
        .map(|era| configs[usize::from(era.era.into_inner())].clone()),
    )
}

/// `config` as the only era, from genesis.
#[must_use]
pub fn single_era(config: lb_ledger::Config) -> Arc<EraScheduledConfig> {
    schedule(config, [])
}

/// `config` as the consensus config of the only era, from genesis.
#[must_use]
pub fn single_consensus_era(
    config: lb_cryptarchia_engine::Config,
) -> Arc<EraSchedule<lb_cryptarchia_engine::Config>> {
    let timing = EraTiming {
        slot_duration: std::time::Duration::from_secs(1),
        epoch_length: NonZero::<u64>::MIN,
    };
    Arc::new(
        EraSchedule::new(
            time::OffsetDateTime::UNIX_EPOCH,
            timing,
            EraEntriesAfterGenesis::empty(),
        )
        .expect("a single era resolves")
        .map(move |_| config.clone()),
    )
}

#[must_use]
pub fn ledger_config(security_param: NonZero<u32>) -> lb_ledger::Config {
    ledger_config_with_uncles(security_param, 12, 4)
}

/// [`ledger_config`] with `W`, the uncle reference window in blocks.
#[must_use]
pub fn ledger_config_with_uncle_reference_window(
    security_param: NonZero<u32>,
    uncle_reference_window_in_block: u32,
) -> lb_ledger::Config {
    ledger_config_with_uncles(security_param, uncle_reference_window_in_block, 4)
}

/// [`ledger_config`], with blocks carrying at most `max_uncles` uncles.
#[must_use]
pub fn ledger_config_with_max_uncles(
    security_param: NonZero<u32>,
    max_uncles: u8,
) -> lb_ledger::Config {
    ledger_config_with_uncles(security_param, 12, max_uncles)
}

/// [`ledger_config`] with `W`, the uncle reference window in blocks, and
/// blocks carrying at most `max_uncles` uncles.
fn ledger_config_with_uncles(
    security_param: NonZero<u32>,
    uncle_reference_window_in_block: u32,
    max_uncles: u8,
) -> lb_ledger::Config {
    let mut service_params = HashMap::new();
    service_params.insert(
        lb_core::sdp::ServiceType::BlendNetwork,
        ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        },
    );
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
        epoch_period_nonce_buffer: 3.try_into().unwrap(),
        epoch_period_nonce_stabilization: 4.try_into().unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        security_param,
        NonNegativeRatio::new(1, 10.try_into().unwrap()),
        1.0.try_into().unwrap(),
        uncle_reference_window_in_block,
        max_uncles,
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    lb_ledger::Config {
        epoch_config,
        consensus_config,
        sdp_config: lb_ledger::mantle::sdp::Config {
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
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
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<19>(),
                damping_den_offset: 0,
                damping_num: 1.try_into().unwrap(),
                max_step: 1.try_into().unwrap(),
                target_transactions_per_block: 1.try_into().unwrap(),
            },
            reward: disabled_reward_config(),
        },
    }
}

/// A proof of leadership verifies from the header alone, so a block overstating
/// its reward passes it, and fails once its transactions have run. The error is
/// not one of a bad copy: the block ID covers the value and the transactions.
#[test]
fn a_block_overstating_its_reward_is_condemned_after_a_valid_proof() {
    let (secret_key, note) = note();
    let genesis_id = GENESIS_ID.into();
    let mut cryptarchia =
        genesis_cryptarchia_on(note, single_era(ledger_config(3.try_into().unwrap())));
    let (_, aged_path) = CommitmentMmr::new()
        .push_with_paths(note.commitment(), &mut [])
        .unwrap();
    let (block, _) = build_block_paying(
        &cryptarchia,
        genesis_id,
        &note,
        secret_key,
        &aged_path,
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::empty(),
        1,
    )
    .expect("should find a winning slot");
    let (id, slot) = (block.header().id(), block.header().slot());

    cryptarchia
        .verify_proposal_header(block.header(), slot)
        .expect("the proof holds for the value the header carries");
    let err = cryptarchia.try_apply_block(block, slot).err().unwrap();
    assert!(matches!(
        err,
        Error::Ledger(lb_ledger::LedgerError::WrongRewardValue { .. })
    ));
    assert!(matches!(
        crate::api::apply_block_api_error(err, id),
        crate::api::ApiError::Unexpected(_)
    ));
}

/// Builds a block with no trasaction by grinding through slots
pub fn try_build_block(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    note: &Note,
    secret_key: Fr,
    start_slot: Slot,
    uncle_headers: UncleHeaders,
    // The reward key of the block is returned with it: its header does not show it.
) -> Option<(Block<SignedOps<Preverified, StandardMode>>, ZkPublicKey)> {
    // The aged path of the note of a one note genesis.
    let (_, aged_path) = CommitmentMmr::new()
        .push_with_paths(note.commitment(), &mut [])
        .unwrap();
    try_build_block_with_transactions(
        cryptarchia,
        parent,
        note,
        secret_key,
        &aged_path,
        start_slot,
        uncle_headers,
        BlockTransactions::empty(),
    )
}

/// Builds a block carrying `transactions` by grinding through slots
#[expect(clippy::too_many_arguments, reason = "a test helper")]
pub fn try_build_block_with_transactions(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    note: &Note,
    secret_key: Fr,
    aged_path: &MerklePath,
    start_slot: Slot,
    uncle_headers: UncleHeaders,
    transactions: BlockTransactions<SignedOps<Preverified, StandardMode>>,
    // The reward key of the block is returned with it: its header does not show it.
) -> Option<(Block<SignedOps<Preverified, StandardMode>>, ZkPublicKey)> {
    build_block_paying(
        cryptarchia,
        parent,
        note,
        secret_key,
        aged_path,
        start_slot,
        uncle_headers,
        transactions,
        0,
    )
}

// Applies a header before its block is proven.
struct UnprovenHeader;

impl LeaderProof for UnprovenHeader {
    fn verify(&self, _public_inputs: &LeaderPublic) -> bool {
        true
    }

    fn verify_genesis(&self) -> bool {
        false
    }

    fn entropy(&self) -> Fr {
        Fr::ZERO
    }

    fn reward_commitment(&self) -> NoteCommitment {
        NoteCommitment(Fr::ZERO)
    }

    fn reward_value(&self) -> Value {
        0
    }
}

// The reward of the block, or 0 for a block the test wants invalid before that.
fn computed_reward_value(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    slot: Slot,
    uncle_headers: &UncleHeaders,
    transactions: &BlockTransactions<SignedOps<Preverified, StandardMode>>,
) -> Value {
    let eras = cryptarchia.ledger.era_schedule();
    cryptarchia
        .ledger
        .state(&parent)
        .expect("the parent of the block is applied")
        .clone()
        .try_apply_header::<_, HeaderId>(slot, &UnprovenHeader, &uncle_headers.slots(), eras)
        .and_then(|(state, _)| {
            state.try_apply_block_transactions::<_, HeaderId, MainnetGasProfile>(
                eras,
                transactions.iter().cloned(),
            )
        })
        .and_then(|(state, gas_and_fees, _, _)| state.leader_reward(&gas_and_fees, eras))
        .unwrap_or(0)
}

/// Builds a block whose header overstates its reward by `excess_reward`.
#[expect(clippy::too_many_arguments, reason = "a test helper")]
fn build_block_paying(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    note: &Note,
    secret_key: Fr,
    aged_path: &MerklePath,
    start_slot: Slot,
    uncle_headers: UncleHeaders,
    transactions: BlockTransactions<SignedOps<Preverified, StandardMode>>,
    excess_reward: Value,
) -> Option<(Block<SignedOps<Preverified, StandardMode>>, ZkPublicKey)> {
    let start_slot: u64 = start_slot.into();
    let nullifier_key = ZkKey::new(secret_key).nullifier_key();
    let nullifier = derive_note_nullifier(&note.commitment(), &nullifier_key);
    for slot in start_slot..=(start_slot + 1000) {
        let epoch_state = cryptarchia.epoch_state_for_slot(slot.into()).unwrap();
        let tip_state = cryptarchia.ledger.state(&cryptarchia.tip()).unwrap();
        let public_inputs = LeaderPublic::new(
            epoch_state.eligible_root(),
            tip_state.latest_note_sets().eligible_root(),
            epoch_state.nonce,
            slot,
            epoch_state.lottery_0,
            epoch_state.lottery_1,
        );

        if !check_winning(note, public_inputs, nullifier_key, nullifier_key) {
            continue;
        }

        // A fresh reward note, so that two forks won at one slot get distinct
        // reward commitments, and so distinct IDs.
        let reward_key = ZkPublicKey::new(Fr::from(thread_rng().next_u64()));
        let reward = Note::new(
            computed_reward_value(
                cryptarchia,
                parent,
                slot.into(),
                &uncle_headers,
                &transactions,
            ) + excess_reward,
            Fr::from(thread_rng().next_u64()),
            reward_key,
        );
        let mut wallet = pol_wallet_inputs(
            note,
            LEDGER_SET,
            &nullifier,
            aged_path,
            &epoch_state.note_sets,
            tip_state.latest_note_sets(),
        )
        .unwrap();
        wallet.nullifier_key = nullifier_key;
        wallet.staking_secret_key = nullifier_key;
        wallet.reward_public_key = *reward.public_key.as_fr();
        wallet.reward_nonce = reward.nonce;
        // The block ID covers the entropy contribution, set before proving.
        let unbound = LeaderPrivate::new(public_inputs, wallet, Fr::ZERO, reward.value);
        let id = block_id(
            &parent,
            slot.into(),
            &body_root(&uncle_headers, transactions.as_slice()),
            &reward.commitment(),
            reward.value,
            &unbound.entropy_contribution(),
        );
        let proof = Groth16LeaderProof::prove(unbound.with_msg(pol_msg(&id))).unwrap();

        let block = Block::create(parent, slot.into(), uncle_headers, proof, transactions).unwrap();
        return Some((block, reward_key));
    }

    None
}

/// A chain `G --- B1` with a fork block `U1` also extending `G`, at the
/// same slot as `B1`.
#[expect(clippy::type_complexity, reason = "a test helper")]
pub fn chain_with_fork() -> (
    Cryptarchia,
    Block<SignedOps<Preverified, StandardMode>>,
    Block<SignedOps<Preverified, StandardMode>>,
    ZkPublicKey,
    Fr,
    Note,
) {
    chain_with_fork_on(single_era(ledger_config(3.try_into().unwrap())))
}

/// The chain of [`chain_with_fork`], over the eras of `era_schedule`.
#[expect(clippy::type_complexity, reason = "a test helper")]
pub fn chain_with_fork_on(
    era_schedule: Arc<EraScheduledConfig>,
) -> (
    Cryptarchia,
    Block<SignedOps<Preverified, StandardMode>>,
    Block<SignedOps<Preverified, StandardMode>>,
    ZkPublicKey,
    Fr,
    Note,
) {
    let genesis_id = GENESIS_ID.into();
    let (secret_key, note) = note();
    let mut cryptarchia = genesis_cryptarchia_on(note, era_schedule);

    // Both extend the genesis, and the same key wins the same slot, so the
    // two blocks differ only in their (randomly generated) reward commitments.
    let (u1, u1_key) = try_build_block(
        &cryptarchia,
        genesis_id,
        &note,
        secret_key,
        Slot::new(1),
        UncleHeaders::empty(),
    )
    .unwrap();
    let (b1, _) = try_build_block(
        &cryptarchia,
        genesis_id,
        &note,
        secret_key,
        Slot::new(1),
        UncleHeaders::empty(),
    )
    .unwrap();
    let b1_header_slot = b1.header().slot();
    cryptarchia
        .try_apply_block(b1.clone(), b1_header_slot)
        .unwrap();

    (cryptarchia, b1, u1, u1_key, secret_key, note)
}

pub const GENESIS_ID: [u8; 32] = [0; 32];

/// A chain with only the genesis block, holding `note`.
pub fn genesis_cryptarchia(note: Note) -> Cryptarchia {
    genesis_cryptarchia_on(note, single_era(ledger_config(3.try_into().unwrap())))
}

/// [`genesis_cryptarchia`], over the eras of `era_schedule`.
pub fn genesis_cryptarchia_on(note: Note, era_schedule: Arc<EraScheduledConfig>) -> Cryptarchia {
    let genesis_config = era_schedule.genesis().parameters.clone();
    Cryptarchia::from_genesis(
        GENESIS_ID.into(),
        LedgerState::from_notes([note], &genesis_config, 0),
        era_schedule,
        lb_cryptarchia_engine::State::Bootstrapping,
    )
}

/// A genesis block distributing the value of each of `notes` to its key and
/// what the supply rule of Mantle Ledger Initialization leaves to another one.
pub fn genesis_block_with(notes: &[Note]) -> GenesisBlock {
    let distributed = notes.iter().map(|note| note.value).sum::<u64>();
    GenesisBlockBuilder::new()
        .try_add_notes(
            notes
                .iter()
                .map(|note| GenesisAllocation {
                    value: note.value,
                    public_key: note.public_key,
                })
                .chain([GenesisAllocation {
                    value: GENESIS_STAKE_SUPPLY - distributed,
                    public_key: ZkPublicKey::new(Fr::from(1u64)),
                }]),
        )
        .unwrap()
        .set_inscription(InscriptionOp {
            channel_id: NULL_CHANNEL_ID,
            inscription: Inscription::new_unchecked(
                GenesisParameters {
                    chain_id: "test-chain".to_owned().try_into().unwrap(),
                    start_time: GenesisTime::new(0),
                    epoch_nonce: Fr::ZERO,
                }
                .encode_to_vec(),
            ),
            parent: MsgId::root(),
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            mini_blocks: MiniBlocks::default(),
        })
        .build()
        .unwrap()
}

/// The paths of `notes` in a commitment MMR holding them, in order.
pub fn commitment_paths(notes: &[Note]) -> Vec<MerklePath> {
    let mut mmr = CommitmentMmr::new();
    let mut paths = Vec::new();
    for note in notes {
        let (next, path) = mmr.push_with_paths(note.commitment(), &mut paths).unwrap();
        mmr = next;
        paths.push(path);
    }
    paths
}

pub fn signed_header(block: &Block<SignedOps<Preverified, StandardMode>>) -> Header {
    block.header().clone()
}

// A note under a random secret key, so that the notes of one test differ.
pub fn note() -> (Fr, Note) {
    let secret_key = Fr::from(thread_rng().next_u64());
    // 10_000 LGO, enough to pay a transfer at the genesis gas prices.
    let note = Note::new(
        10_000_000_000_000,
        Fr::from(1u64),
        ZkKey::new(secret_key).to_public_key(),
    );
    (secret_key, note)
}

pub fn spawn_storage_service(mut rx: mpsc::Receiver<StorageMsg>) -> (JoinHandle<()>, TempDir) {
    let db_dir = TempDir::new().unwrap();
    let mut backend = RocksBackend::new(RocksBackendSettings {
        db_path: db_dir.path().join("db"),
        read_only: false,
        column_family: None,
    })
    .unwrap();

    let handle = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            StorageService::<TestRuntimeServiceId>::handle_storage_message(msg, &mut backend).await;
        }
    });

    (handle, db_dir)
}

pub struct TestRuntimeServiceId;

impl
    AsServiceId<CryptarchiaConsensus<SignedOps<Preverified, StandardMode>, SystemTimeBackend, Self>>
    for TestRuntimeServiceId
{
    const SERVICE_ID: Self = Self;
}

impl Display for TestRuntimeServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TestRuntimeServiceId")
    }
}
