use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::{self, Display},
    num::{NonZero, NonZeroU32, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures::StreamExt as _;
use lb_binary_codec::{
    bincode::{DeserializeOp as _, SerializeOp as _},
    canonical::BinaryEncode as _,
};
use lb_core::{
    block::{
        Block, BlockTransactions, SignedHeader, TxInBlock, UncleHeaders,
        genesis::GenesisBlockBuilder,
    },
    era::{AcceptedForkDigests, ForkDigest},
    mantle::{
        GenesisParameters, GenesisTime, Note, Op, OpProof, SignedOps, Utxo,
        channel::Channels,
        gas::TxGasCalculator as _,
        ledger::{BoundedInputs, Outputs, verification_mode::StandardMode},
        ops::{
            channel::{
                ChannelId, MsgId,
                inscribe::{Inscription, InscriptionOp},
            },
            leader_claim::{VoucherCm, VoucherSecret},
            transfer::TransferOp,
        },
        traits::Hashable as _,
        transactions::{
            GasPrices, OpProofs, Ops,
            states::{Preverified, Unverified},
            tx_list::ops::OpsGasContext,
        },
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic, check_winning},
    sdp::ServiceParameters,
};
use lb_cryptarchia_engine::{EpochConfig, EraSchedule, Slot, UncleSlots};
use lb_cryptarchia_sync::HeaderId;
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::{
    Ed25519Key, Ed25519Signature, UnverifiedEd25519PublicKey, ZkKey,
};
use lb_ledger::{
    ConsensusExcludedKeys, LedgerState,
    config::{
        BlendPoWConfig, EraScheduledConfig, ModulusShift, PoWConfig, RewardPoWConfig,
        config_for_slot,
    },
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use lb_services_utils::overwatch::RecoveryData;
use lb_storage_service::{
    KeyValuePair, StorageMsg, StorageService,
    api::StorageApi,
    backend::StorageBackend as _,
    recovery::{load_recovery_data, recovery_key},
    rocksdb::{RocksBackend, RocksBackendSettings},
};
use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraTiming};
use lb_time_service::backends::SystemTimeBackend;
use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};
use overwatch::services::{AsServiceId, relay::OutboundRelay};
use rand::{RngCore as _, thread_rng};
use tempfile::TempDir;
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

use crate::{
    BlockProviderConfig, BlockWithVerifiedHeader, BootstrapConfig, Cryptarchia,
    CryptarchiaConsensus, CryptarchiaSettings, DEFAULT_CHECKPOINT_INTERVAL_BLOCKS, Error,
    LibCheckpoint, OfflineGracePeriodConfig, StartingState, SyncConfig, TipEra,
    relays::CryptarchiaConsensusRelays,
    saved_tip_era,
    service::{get_block_ids, process_block},
    states::{RECOVERY_PROGRESS_KEY_SUFFIX, RecoveryProgress, RecoveryRecorder},
};

#[test]
fn cryptarchia_switch_to_online() {
    let k = NonZero::<u32>::new(1).unwrap();
    let config = ledger_config(k);

    let (zk_key, utxo) = utxo();
    let genesis_id: HeaderId = [0; 32].into();
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_utxos([utxo], &config, 0, &ConsensusExcludedKeys::empty()),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );

    // Add 3 new blocks to the chain
    let mut block_ids = vec![genesis_id];
    let mut slot = Slot::new(1);
    while block_ids.len() < 4 {
        // TODO: Use a mock proof system instead of expensive real proof
        // generation, by refactoring `Cryptarchia`.
        let (block, _) = try_build_block(
            &cryptarchia,
            *block_ids.last().unwrap(),
            utxo,
            &zk_key,
            slot,
            UncleHeaders::empty(),
        )
        .expect("should find a winning slot");

        let block_header_id = block.header().id();
        let block_header_slot = block.header().slot();
        let (pruned_blocks, reorged_blocks, _) = cryptarchia
            .try_apply_block(block, block_header_slot)
            .unwrap();
        // No block should be pruned since LIB is not updated during
        // Bootstrapping
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
    let (zk_key, utxo) = utxo();
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_utxos([utxo], &config, 0, &ConsensusExcludedKeys::empty()),
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
            utxo,
            &zk_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        verify_and_process_block(
            &mut cryptarchia,
            block.clone(),
            block.header().slot(),
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
            utxo,
            &zk_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        verify_and_process_block(
            &mut cryptarchia,
            block.clone(),
            block.header().slot(),
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

/// An old checkpoint sets where replay begins. The compact progress still sets
/// the finalized floor, even after the offline grace period has elapsed.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "exercise restart and both kinds of missing blocks"
)]
async fn recover_latest_lib_and_tip_from_an_older_checkpoint() {
    type Tx = SignedOps<Preverified, StandardMode>;

    let (broadcast_tx, _broadcast_rx) = mpsc::channel(10);
    let (time_tx, _time_rx) = mpsc::channel(10);
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let (storage_task, db_dir) = spawn_storage_service(storage_rx);

    let make_relays = |storage_tx| {
        CryptarchiaConsensusRelays::<Tx>::new(
            OutboundRelay::new(broadcast_tx.clone()),
            StorageApi::new(OutboundRelay::new(storage_tx)),
            OutboundRelay::new(time_tx.clone()),
        )
    };
    let relays = make_relays(storage_tx);

    let (new_block_tx, _new_block_rx) = broadcast::channel(16);
    let (lib_tx, _lib_rx) = broadcast::channel(16);

    let config = ledger_config(NonZeroU32::MIN);
    let next_fork_digest = ForkDigest::new([0x22; 32]);
    let mut next_config = config.clone();
    next_config.accepted_fork_digests =
        AcceptedForkDigests::new(vec![FORK_DIGEST, next_fork_digest]);

    let era_schedule = schedule(config.clone(), [(4, next_config)]);
    let fork_digests = Arc::new(era_schedule.map(|era| {
        if era.era.into_inner() == 0 {
            FORK_DIGEST
        } else {
            next_fork_digest
        }
    }));
    let era_boundary = era_schedule.at_epoch(4.into()).first_slot;

    let (leader_key, leader_utxo) = utxo();
    let (_, spent_utxo) = utxo();
    let mut live = Cryptarchia::from_genesis(
        GENESIS_ID.into(),
        LedgerState::from_utxos(
            [leader_utxo, spent_utxo],
            &config,
            0,
            &ConsensusExcludedKeys::empty(),
        ),
        Arc::clone(&era_schedule),
        lb_cryptarchia_engine::State::Online,
    );

    let no_stale_blocks = HashSet::new();
    let checkpoint_key = recovery_key(b"cryptarchia");
    let mut checkpoint_bytes = None;
    let mut recorder = None;
    let mut next_slot = Slot::new(1);
    let mut blocks = Vec::new();

    for height in 1..=4 {
        let transactions = if height == 3 {
            next_slot = next_slot.max(era_boundary);
            BlockTransactions::try_new(vec![burn_tx(spent_utxo, &leader_key)], u16::MAX).unwrap()
        } else {
            BlockTransactions::empty()
        };

        let (block, _) = try_build_block_with_transactions(
            &live,
            live.tip(),
            leader_utxo,
            &leader_key,
            next_slot,
            UncleHeaders::empty(),
            transactions,
        )
        .unwrap();
        let slot = block.header().slot();
        blocks.push(block.header().id());

        let block = live.verify_block(block, slot).unwrap();
        process_block(
            &mut live,
            block,
            &relays,
            &new_block_tx,
            &lib_tx,
            recorder
                .as_mut()
                .map(|recorder| (recorder, &no_stale_blocks)),
        )
        .await
        .unwrap();

        next_slot = slot.strict_add(1.into());

        if height == 2 {
            let checkpoint = LibCheckpoint::from_cryptarchia_and_unpruned_blocks(
                &live,
                no_stale_blocks.clone(),
                &fork_digests,
            )
            .unwrap();
            assert_eq!(checkpoint.lib, blocks[0]);
            assert_eq!(checkpoint.tip_era.fork_digest, FORK_DIGEST);

            let bytes = checkpoint.to_bytes().unwrap();
            relays
                .storage()
                .store_immutable_block_ids_and_recovery(
                    BTreeMap::new(),
                    vec![KeyValuePair {
                        key: checkpoint_key.clone(),
                        value: bytes.clone(),
                    }],
                )
                .await
                .unwrap();

            checkpoint_bytes = Some(bytes);
            recorder = Some(RecoveryRecorder::new(
                &checkpoint,
                Arc::clone(&fork_digests),
                DEFAULT_CHECKPOINT_INTERVAL_BLOCKS,
            ));
        }
    }

    let lib_id = blocks[2];
    let tip_id = blocks[3];
    assert_eq!(live.lib(), lib_id);
    assert_eq!(live.tip(), tip_id);

    drop(relays);
    storage_task.await.unwrap();

    let db_path = db_dir.path().join("db");
    let recovery_data = load_recovery_data(RocksBackendSettings {
        db_path: db_path.clone(),
        read_only: false,
        column_family: None,
    })
    .unwrap();

    assert_eq!(
        saved_tip_era(&recovery_data).unwrap(),
        Some(TipEra {
            slot: live.tip_branch().slot(),
            fork_digest: next_fork_digest,
        }),
    );

    let saved_checkpoint_bytes = recovery_data.take(&checkpoint_key).unwrap().unwrap();
    assert_eq!(saved_checkpoint_bytes, checkpoint_bytes.unwrap());

    let saved_checkpoint = LibCheckpoint::from_bytes(&saved_checkpoint_bytes).unwrap();
    let mut progress = RecoveryProgress::load(&saved_checkpoint, &recovery_data).unwrap();
    assert_eq!(progress.lib, lib_id);
    assert_eq!(progress.tip, tip_id);

    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_task = spawn_storage_service_at(storage_rx, db_path);
    let relays = make_relays(storage_tx);
    let bootstrap_config = BootstrapConfig {
        prolonged_bootstrap_period: Duration::ZERO,
        force_bootstrap: false,
        offline_grace_period: OfflineGracePeriodConfig::default(),
    };

    let recover = async |progress: &RecoveryProgress| {
        CryptarchiaConsensus::<Tx, SystemTimeBackend, TestRuntimeServiceId>::initialize_cryptarchia(
            &saved_checkpoint,
            progress,
            &bootstrap_config,
            Arc::clone(&era_schedule),
            &relays,
            &new_block_tx,
            &lib_tx,
        )
        .await
    };

    let recovered = recover(&progress).await.unwrap();
    assert!(!recovered.fell_back_to_lib);
    assert_eq!(recovered.cryptarchia.info(), live.info());

    for id in [lib_id, tip_id] {
        assert_eq!(
            recovered.cryptarchia.ledger.state(&id),
            live.ledger.state(&id)
        );
    }

    progress.last_engine_state.as_mut().unwrap().timestamp = SystemTime::UNIX_EPOCH;
    let recovered = recover(&progress).await.unwrap();
    assert!(!recovered.fell_back_to_lib);
    assert!(recovered.cryptarchia.is_bootstrapping());
    assert_eq!(recovered.cryptarchia.lib(), lib_id);
    assert_eq!(recovered.cryptarchia.tip(), tip_id);
    assert_eq!(
        recovered.cryptarchia.ledger.state(&lib_id),
        live.ledger.state(&lib_id)
    );

    relays
        .storage()
        .remove_block(tip_id)
        .await
        .unwrap()
        .unwrap();

    let recovered = recover(&progress).await.unwrap();
    assert!(recovered.fell_back_to_lib);
    assert_eq!(recovered.cryptarchia.tip(), lib_id);
    assert_eq!(
        recovered.cryptarchia.ledger.state(&lib_id),
        live.ledger.state(&lib_id)
    );

    let finalized_block = blocks[1];
    relays
        .storage()
        .remove_block(finalized_block)
        .await
        .unwrap()
        .unwrap();

    let failed = recover(&progress).await;
    assert!(matches!(
        failed,
        Err(Error::ParentIdNotFound(_) | Error::HeaderIdNotFound(_))
    ));
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

    let (cryptarchia, b1, u1, _, zk_key, utxo) = chain_with_fork();
    let (b2, _) = try_build_block(
        &cryptarchia,
        cryptarchia.tip(),
        utxo,
        &zk_key,
        u1.header().slot().strict_add(1.into()),
        UncleHeaders::try_new(vec![signed_header(&u1)], 4).unwrap(),
    )
    .unwrap();
    let (b1_id, b2_id, b2_slot) = (b1.header().id(), b2.header().id(), b2.header().slot());

    // Before the restart: `B1` and `B2` are verified and stored.
    let mut stored = genesis_cryptarchia(utxo);
    for block in [b1.clone(), b2] {
        verify_and_process_block(&mut stored, block, b2_slot, &relays, &new_block_tx, &lib_tx)
            .await
            .expect("the block should be valid before the restart");
    }

    let recovery_state = LibCheckpoint {
        tip: b2_id,
        tip_era: TipEra {
            slot: b2_slot,
            fork_digest: ForkDigest::from([0; 32]),
        },
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
        prolonged_bootstrap_period: Duration::ZERO,
        force_bootstrap: true,
        offline_grace_period: OfflineGracePeriodConfig::default(),
    };
    let initialized = CryptarchiaConsensus::<
        Tx,
        SystemTimeBackend,
        TestRuntimeServiceId,
    >::initialize_cryptarchia(
        &recovery_state,
        &RecoveryProgress::from_checkpoint(&recovery_state),
        &bootstrap_config,
        Arc::clone(stored.ledger.era_schedule()),
        &relays,
        &new_block_tx,
        &lib_tx,
    )
    .await;
    let initialized = initialized.expect("the chain should be recovered");
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

    let result = cryptarchia.verify_block(block.clone(), current_slot);
    assert!(matches!(
        result,
        Err(Error::FutureBlock { block_slot: s, current_slot: c })
            if s == block_slot && c == current_slot
    ));

    let mut cryptarchia = cryptarchia;
    cryptarchia
        .try_apply_block_with_state_retention(&BlockWithVerifiedHeader::new_unchecked(
            block.clone(),
        ))
        .expect("a stored block should be replayed regardless of the current slot");
    assert_eq!(cryptarchia.tip(), block.header().id());
}

/// A copy of a block whose signature does not verify over its header is
/// rejected, as is the block ID it carries, which commits to no signature.
#[test]
fn tampered_signature_is_rejected() {
    let (cryptarchia, block) = test_chain_with_next_block();
    let mut signature = block.signature().to_bytes();
    signature[0] ^= 1;

    let result = cryptarchia.verify_header(
        block.header().clone(),
        block.uncle_headers().clone(),
        Ed25519Signature::from_bytes(&signature),
        block.header().slot(),
    );
    assert!(matches!(result, Err(Error::InvalidSignature)));
}

/// A block whose parent this node does not have is reported as missing its
/// parent, so that it can be handled as an orphan.
#[test]
fn unknown_parent_is_reported_missing() {
    let (fork_chain, b1, _, _, zk_key, utxo) = chain_with_fork();
    let (b2, _) = try_build_block(
        &fork_chain,
        b1.header().id(),
        utxo,
        &zk_key,
        b1.header().slot().strict_add(1.into()),
        UncleHeaders::empty(),
    )
    .unwrap();

    let result = genesis_cryptarchia(utxo).verify_block(b2.clone(), b2.header().slot());
    assert!(matches!(
        result,
        Err(Error::ParentMissing { parent, .. }) if parent == b1.header().id()
    ));
}

/// The header of a block that is already applied is reported as such, without
/// being verified again.
#[test]
fn header_of_applied_block_is_reported_already_applied() {
    let (mut cryptarchia, block) = test_chain_with_next_block();
    let slot = block.header().slot();
    cryptarchia
        .try_apply_block(block.clone(), slot)
        .expect("the block should be applied");

    let result = cryptarchia.verify_header(
        block.header().clone(),
        block.uncle_headers().clone(),
        *block.signature(),
        slot,
    );
    assert!(matches!(
        result,
        Err(Error::AlreadyApplied(id)) if id == block.header().id()
    ));
}

/// A copy of a block carrying other transactions than its header commits to
/// is rejected, even though its header is valid.
#[test]
fn transactions_not_committed_to_are_rejected() {
    let (cryptarchia, block) = test_chain_with_next_block();
    let (zk_key, utxo) = utxo();
    let header = cryptarchia
        .verify_header(
            block.header().clone(),
            block.uncle_headers().clone(),
            *block.signature(),
            block.header().slot(),
        )
        .expect("the header is valid");

    let result = BlockWithVerifiedHeader::new(
        header,
        BlockTransactions::<SignedOps<Preverified, StandardMode>>::try_new(
            vec![burn_tx(utxo, &zk_key)],
            u16::MAX,
        )
        .unwrap(),
    );
    assert!(matches!(
        result,
        Err(lb_core::block::Error::BodyRootMismatch)
    ));
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
    let (zk_key, utxo) = utxo();
    let mut stored = genesis_cryptarchia(utxo);
    let mut blocks = Vec::new();
    let mut slot = Slot::genesis().strict_add(1.into());
    for _ in 0..2 {
        let (block, _) = try_build_block(
            &stored,
            stored.tip(),
            utxo,
            &zk_key,
            slot,
            UncleHeaders::empty(),
        )
        .unwrap();
        slot = block.header().slot().strict_add(1.into());
        verify_and_process_block(
            &mut stored,
            block.clone(),
            block.header().slot(),
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

    let recovery_state = LibCheckpoint {
        tip: b2_id,
        tip_era: TipEra {
            slot: b2.header().slot(),
            fork_digest: ForkDigest::from([0; 32]),
        },
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
        prolonged_bootstrap_period: Duration::ZERO,
        force_bootstrap: true,
        offline_grace_period: OfflineGracePeriodConfig::default(),
    };
    let initialized = CryptarchiaConsensus::<
        Tx,
        SystemTimeBackend,
        TestRuntimeServiceId,
    >::initialize_cryptarchia(
        &recovery_state,
        &RecoveryProgress::from_checkpoint(&recovery_state),
        &bootstrap_config,
        Arc::clone(stored.ledger.era_schedule()),
        &relays,
        &new_block_tx,
        &lib_tx,
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

    let result = verify_and_process_block(
        &mut cryptarchia,
        block,
        block_slot,
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

/// `tx`, carried by a block at `slot`.
const fn tx_in_block<Tx>(tx: Tx, slot: Slot) -> TxInBlock<Tx> {
    TxInBlock { tx, slot }
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
    let (zk_key, leader_utxo) = utxo();
    let (_, funding_utxo_a) = utxo();
    let (_, funding_utxo_b) = utxo();
    let (_, funding_utxo_c) = utxo();
    let genesis = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_utxos(
            [leader_utxo, funding_utxo_a, funding_utxo_b, funding_utxo_c],
            &config,
            0,
            &ConsensusExcludedKeys::empty(),
        ),
        single_era(config),
        lb_cryptarchia_engine::State::Online,
    );
    let tx1 = burn_tx(funding_utxo_a, &zk_key);
    let tx2 = burn_tx(funding_utxo_b, &zk_key);
    let tx3 = burn_tx(funding_utxo_c, &zk_key);

    // Build the fork on its own view, where each fork block is the tip when
    // its child is built.
    let mut fork_view = genesis.clone();
    let (f1, _) = try_build_block_with_transactions(
        &fork_view,
        genesis_id,
        leader_utxo,
        &zk_key,
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::try_new(vec![tx1.clone()], u16::MAX).unwrap(),
    )
    .unwrap();
    fork_view
        .try_apply_block(f1.clone(), f1.header().slot())
        .unwrap();
    let (f2, _) = try_build_block_with_transactions(
        &fork_view,
        f1.header().id(),
        leader_utxo,
        &zk_key,
        f1.header().slot().strict_add(1.into()),
        UncleHeaders::empty(),
        BlockTransactions::try_new(vec![tx2.clone()], u16::MAX).unwrap(),
    )
    .unwrap();

    let mut cryptarchia = genesis;
    let (b1, _) = try_build_block_with_transactions(
        &cryptarchia,
        genesis_id,
        leader_utxo,
        &zk_key,
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::try_new(vec![tx1.clone(), tx3.clone()], u16::MAX).unwrap(),
    )
    .unwrap();

    let mut apply = async |block: Block<SignedOps<Preverified, StandardMode>>| {
        let slot = block.header().slot();
        verify_and_process_block(
            &mut cryptarchia,
            block,
            slot,
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
    assert_eq!(outcome.reorged_txs, vec![tx_in_block(tx3, b1_slot)]);
    assert_eq!(cryptarchia.tip(), f2.header().id());
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_switch_persists_tip_lib_and_pruning_progress() {
    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage = spawn_storage_service(storage_rx);
    let relays = recovery_test_relays(storage_tx);
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);

    let era_schedule = single_era(ledger_config(NonZeroU32::MIN));
    let (_, local, fork, _, key, utxo) = chain_with_fork_on(Arc::clone(&era_schedule));
    let genesis = genesis_cryptarchia_on(utxo, era_schedule);
    let mut fork_view = genesis.clone();
    fork_view
        .try_apply_block(fork.clone(), fork.header().slot())
        .unwrap();

    let fork_id = fork.header().id();
    let (fork_tip, _) = try_build_block(
        &fork_view,
        fork_id,
        utxo,
        &key,
        fork.header().slot().strict_add(1.into()),
        UncleHeaders::empty(),
    )
    .unwrap();

    let (mut chain, _) = genesis.online();
    let fork_digests = Arc::new(chain.ledger.era_schedule().map(|_| FORK_DIGEST));
    let mut checkpoint =
        LibCheckpoint::from_cryptarchia_and_unpruned_blocks(&chain, HashSet::new(), &fork_digests)
            .unwrap();
    checkpoint.last_engine_state = None;

    let mut recorder = RecoveryRecorder::new(
        &checkpoint,
        fork_digests,
        DEFAULT_CHECKPOINT_INTERVAL_BLOCKS,
    );
    let mut apply = async |block: Block<RecoveryTestTx>, pending: &HashSet<HeaderId>| {
        let slot = block.header().slot();
        let block = chain.verify_block(block, slot).unwrap();

        process_block(
            &mut chain,
            block,
            &relays,
            &new_block_tx,
            &lib_tx,
            Some((&mut recorder, pending)),
        )
        .await
        .unwrap();
    };

    let local_id = local.header().id();
    let no_pending = HashSet::new();
    apply(local, &no_pending).await;
    apply(fork, &no_pending).await;

    let progress_key = recovery_key(RECOVERY_PROGRESS_KEY_SUFFIX);
    let tied = relays
        .storage()
        .load::<RecoveryProgress>(progress_key.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tied.tip, local_id);
    assert_eq!(tied.lib, GENESIS_ID.into());
    assert!(tied.storage_blocks_to_remove.is_empty());

    let fork_tip_id = fork_tip.header().id();
    let pending_removal = HeaderId::from([0xff; 32]);
    apply(fork_tip, &HashSet::from([pending_removal])).await;

    let progress = relays
        .storage()
        .load::<RecoveryProgress>(progress_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.tip, fork_tip_id);
    assert_eq!(progress.lib, fork_id);
    assert_eq!(
        progress.storage_blocks_to_remove,
        HashSet::from([local_id, pending_removal])
    );
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "shares one chain across checkpoint rollover and restart"
)]
async fn block_import_rolls_over_checkpoint_and_recovers_the_next_lib() {
    const ROLLOVER_HEIGHT: u64 = DEFAULT_CHECKPOINT_INTERVAL_BLOCKS.get();

    let era_schedule = single_era(ledger_config(NonZeroU32::MIN));
    let leader_key = ZkKey::from(Fr::ZERO);
    let mut settings =
        recovery_test_settings(&era_schedule, Note::new(10_000, leader_key.to_public_key()));
    let StartingState::Genesis { genesis_block } = &settings.starting_state;
    let leader_utxo = genesis_block
        .genesis_tx()
        .transfer()
        .operation()
        .utxos()
        .next()
        .unwrap();

    let initial = LibCheckpoint::load(&settings).unwrap();
    let mut recorder = RecoveryRecorder::new(
        &initial,
        Arc::new(settings.era_schedule.map(|era| era.parameters.1)),
        settings.checkpoint_interval_blocks,
    );
    let mut chain = Cryptarchia::from_genesis(
        initial.genesis_id,
        initial.lib_ledger_state,
        Arc::clone(&era_schedule),
        lb_cryptarchia_engine::State::Online,
    );

    let (storage_tx, storage_rx) = mpsc::channel(10);
    let (storage_task, db_dir) = spawn_storage_service(storage_rx);
    let relays = recovery_test_relays(storage_tx);
    let (new_block_tx, _new_block_rx) = broadcast::channel(10);
    let (lib_tx, _lib_rx) = broadcast::channel(10);
    let pending = HashSet::new();

    let mut append = async |chain: &mut Cryptarchia| {
        let (block, _) = try_build_block(
            chain,
            chain.tip(),
            leader_utxo,
            &leader_key,
            chain.tip_branch().slot().strict_add(1.into()),
            UncleHeaders::empty(),
        )
        .unwrap();
        let slot = block.header().slot();
        let block = chain.verify_block(block, slot).unwrap();

        process_block(
            chain,
            block,
            &relays,
            &new_block_tx,
            &lib_tx,
            Some((&mut recorder, &pending)),
        )
        .await
        .unwrap();
    };

    for _ in 0..ROLLOVER_HEIGHT {
        append(&mut chain).await;
    }
    assert_eq!(chain.lib_branch().length(), ROLLOVER_HEIGHT - 1);

    let checkpoint_key = recovery_key(b"cryptarchia");
    let before_rollover = relays
        .storage()
        .load::<LibCheckpoint>(checkpoint_key.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before_rollover.lib, initial.genesis_id);
    assert_eq!(before_rollover.lib_block_length, 0);

    append(&mut chain).await;

    let db_path = db_dir.path().join("db");
    let refreshed_bytes = load_recovery_data(RocksBackendSettings {
        db_path: db_path.clone(),
        read_only: true,
        column_family: None,
    })
    .unwrap()
    .get(&checkpoint_key)
    .unwrap()
    .unwrap();
    let rolled_over = LibCheckpoint::from_bytes(&refreshed_bytes).unwrap();
    assert_eq!(rolled_over.lib, chain.lib());
    assert_eq!(rolled_over.lib_block_length, ROLLOVER_HEIGHT);
    assert_eq!(
        &rolled_over.lib_ledger_state,
        chain.ledger.state(&chain.lib()).unwrap()
    );

    append(&mut chain).await;
    assert_eq!(chain.lib_branch().length(), ROLLOVER_HEIGHT + 1);
    assert_ne!(chain.lib(), rolled_over.lib);

    drop(relays);
    storage_task.await.unwrap();

    settings.recovery_data = load_recovery_data(RocksBackendSettings {
        db_path: db_path.clone(),
        read_only: false,
        column_family: None,
    })
    .unwrap();
    let saved_bytes = settings
        .recovery_data
        .get(&checkpoint_key)
        .unwrap()
        .unwrap();
    assert!(
        saved_bytes == refreshed_bytes,
        "the next block rewrote the checkpoint"
    );

    let checkpoint = LibCheckpoint::load(&settings).unwrap();
    assert_eq!(checkpoint.lib, rolled_over.lib);
    assert_eq!(checkpoint.lib_block_length, ROLLOVER_HEIGHT);

    let progress = RecoveryProgress::load(&checkpoint, &settings.recovery_data).unwrap();
    assert_eq!(progress.lib, chain.lib());
    assert_eq!(progress.lib_height, ROLLOVER_HEIGHT + 1);
    assert_eq!(progress.tip, chain.tip());

    let (storage_tx, storage_rx) = mpsc::channel(10);
    let _storage_task = spawn_storage_service_at(storage_rx, db_path);
    let relays = recovery_test_relays(storage_tx);
    let restored = CryptarchiaConsensus::<RecoveryTestTx, SystemTimeBackend, TestRuntimeServiceId>::initialize_cryptarchia(
        &checkpoint,
        &progress,
        &settings.bootstrap,
        era_schedule,
        &relays,
        &new_block_tx,
        &lib_tx,
    )
    .await
    .unwrap();

    assert!(!restored.fell_back_to_lib);
    assert_eq!(restored.cryptarchia.info(), chain.info());
    assert_eq!(
        restored.cryptarchia.ledger.state(&chain.tip()),
        chain.ledger.state(&chain.tip())
    );
}

type RecoveryTestTx = SignedOps<Preverified, StandardMode>;

fn recovery_test_relays(
    storage_tx: mpsc::Sender<StorageMsg>,
) -> CryptarchiaConsensusRelays<RecoveryTestTx> {
    let (broadcast_tx, mut broadcast_rx) = mpsc::channel(10);
    let _broadcast_task = tokio::spawn(async move { while broadcast_rx.recv().await.is_some() {} });

    let (time_tx, _time_rx) = mpsc::channel(10);
    CryptarchiaConsensusRelays::new(
        OutboundRelay::new(broadcast_tx),
        StorageApi::new(OutboundRelay::new(storage_tx)),
        OutboundRelay::new(time_tx),
    )
}

fn recovery_test_settings(
    era_schedule: &EraScheduledConfig,
    leader_note: Note,
) -> CryptarchiaSettings {
    let genesis_parameters = GenesisParameters {
        chain_id: "recovery-test".to_owned().try_into().unwrap(),
        start_time: GenesisTime::new(0),
        epoch_nonce: Fr::ZERO,
        pow_reward_pool: 0,
        consensus_excluded_keys: ConsensusExcludedKeys::empty(),
    };
    let genesis_block = GenesisBlockBuilder::new()
        .add_note(leader_note)
        .set_inscription(InscriptionOp {
            channel_id: ChannelId::from([0; 32]),
            inscription: Inscription::new_unchecked(genesis_parameters.encode_to_vec()),
            parent: MsgId::root(),
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        })
        .build()
        .unwrap();

    CryptarchiaSettings {
        era_schedule: Arc::new(era_schedule.map(|era| (era.parameters.clone(), FORK_DIGEST))),
        starting_state: genesis_block.into(),
        bootstrap: BootstrapConfig {
            prolonged_bootstrap_period: Duration::ZERO,
            force_bootstrap: false,
            offline_grace_period: OfflineGracePeriodConfig::default(),
        },
        sync: SyncConfig {
            block_provider: BlockProviderConfig {
                batch_size: NonZeroUsize::MIN,
            },
        },
        checkpoint_interval_blocks: DEFAULT_CHECKPOINT_INTERVAL_BLOCKS,
        recovery_data: RecoveryData::default(),
    }
}

#[test]
fn ledger_is_not_committed_if_block_contains_invalid_deferred_proof() {
    let config = ledger_config(NonZero::<u32>::new(1).unwrap());
    let (zk_key, utxo) = utxo();
    let genesis_id: HeaderId = [0; 32].into();
    let mut cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_utxos([utxo], &config, 0, &ConsensusExcludedKeys::empty()),
        single_era(config),
        lb_cryptarchia_engine::State::Bootstrapping,
    );

    let fake_key = ZkKey::from(Fr::from(42u64));
    assert_ne!(fake_key.to_public_key(), utxo.note.pk);
    let (block, _) = try_build_block_with_transactions(
        &cryptarchia,
        genesis_id,
        utxo,
        &zk_key,
        Slot::new(1),
        UncleHeaders::empty(),
        BlockTransactions::try_new(vec![transfer_tx_with_fake_sig(utxo, &fake_key)], u16::MAX)
            .unwrap(),
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

/// Verifies `block` at `current_slot`, as a block from the network is, and
/// processes it.
async fn verify_and_process_block(
    cryptarchia: &mut Cryptarchia,
    block: Block<SignedOps<Preverified, StandardMode>>,
    current_slot: Slot,
    relays: &CryptarchiaConsensusRelays<SignedOps<Preverified, StandardMode>>,
    new_block_tx: &broadcast::Sender<crate::ProcessedBlockEvent>,
    lib_tx: &broadcast::Sender<crate::LibUpdate>,
) -> Result<crate::service::ProcessBlockOutcome<SignedOps<Preverified, StandardMode>>, Error> {
    let block = cryptarchia.verify_block(block, current_slot)?;
    process_block(cryptarchia, block, relays, new_block_tx, lib_tx, None).await
}

/// Creates a transfer tx with a fake sig.
fn transfer_tx_with_fake_sig(utxo: Utxo, fake_key: &ZkKey) -> SignedOps<Preverified, StandardMode> {
    let mut output_note = Note::new(1, fake_key.to_public_key());
    let gas_context = OpsGasContext::from_channels(
        &Channels::new(),
        GasPrices::default(),
        ledger_config(NonZero::new(1).unwrap()).gas_profile,
    );
    let fees = transfer_tx(utxo, output_note, fake_key)
        .op_refs()
        .total_gas_cost(&gas_context)
        .unwrap();
    output_note.value = utxo.note.value - fees.into_inner();

    transfer_tx(utxo, output_note, fake_key)
        .preverify()
        .expect("a fake signature is only caught by the stateful checks")
}

/// The fork digest the transactions of these tests carry.
const FORK_DIGEST: ForkDigest = ForkDigest::new([0x11; 32]);

/// Creates a tx spending `utxo` with no outputs, so its whole value pays the
/// fee.
fn burn_tx(utxo: Utxo, key: &ZkKey) -> SignedOps<Preverified, StandardMode> {
    let ops = Ops::new(
        FORK_DIGEST,
        [Op::Transfer(TransferOp::new(
            BoundedInputs::from(utxo.id()).into(),
            Outputs::new([]),
        ))],
    );
    let op_proofs = OpProofs::from([OpProof::ZkSig(
        ZkKey::multi_sign(std::slice::from_ref(key), &ops.hash().to_fr()).unwrap(),
    )]);
    SignedOps::from_parts(ops, op_proofs)
        .unwrap()
        .preverify()
        .unwrap()
}

fn transfer_tx(utxo: Utxo, output_note: Note, key: &ZkKey) -> SignedOps<Unverified, StandardMode> {
    let transfer_op = TransferOp::new(
        BoundedInputs::from(utxo.id()).into(),
        Outputs::new([output_note]),
    );
    let ops = Ops::new(FORK_DIGEST, [Op::Transfer(transfer_op)]);
    let op_proofs = OpProofs::from([OpProof::ZkSig(
        ZkKey::multi_sign(std::slice::from_ref(key), &ops.hash().to_fr()).unwrap(),
    )]);
    SignedOps::from_parts(ops, op_proofs).expect("")
}

fn test_chain_with_next_block() -> (Cryptarchia, Block<SignedOps<Preverified, StandardMode>>) {
    let k = 3.try_into().unwrap();
    let config = ledger_config(k);
    let genesis_id = [0; 32].into();
    let (zk_key, utxo) = utxo();
    let cryptarchia = Cryptarchia::from_genesis(
        genesis_id,
        LedgerState::from_utxos([utxo], &config, 0, &ConsensusExcludedKeys::empty()),
        single_era(config),
        lb_cryptarchia_engine::State::Online,
    );
    let (block, _) = try_build_block(
        &cryptarchia,
        cryptarchia.tip(),
        utxo,
        &zk_key,
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
        target_claims_per_block: core::num::NonZeroU64::new(100).unwrap(),
        rate_num: 0,
        rate_den: core::num::NonZeroU64::MIN,
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
        slot_duration: Duration::from_secs(1),
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
        slot_duration: Duration::from_secs(1),
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
        },
    );
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
        epoch_period_nonce_buffer: 3.try_into().unwrap(),
        epoch_period_nonce_stabilization: 4.try_into().unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        security_param,
        ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
        1.0.try_into().unwrap(),
        uncle_reference_window_in_block,
        max_uncles,
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    lb_ledger::Config {
        epoch_config,
        consensus_config,
        sdp_config: lb_ledger::mantle::sdp::Config {
            era: lb_ledger::mantle::sdp::SdpEra::Master,
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN).unwrap(),
                    cover_replication_factor: 0,
                    num_blend_layers: 3.try_into().unwrap(),
                    minimum_network_size: 1.try_into().unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: lb_core::sdp::MinStake { threshold: 1 },
        },
        max_transactions: 1024,
        max_transactions_size: 2 * 1024 * 1024,
        max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
        tx_era: lb_core::mantle::transactions::TxEra::Master,
        execution_market: lb_ledger::config::ExecutionMarketConfig {
            max_gas_per_block: lb_ledger::config::MaxGasPerBlock::new(3_193_460).unwrap(),
            fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
        },
        storage_market: lb_ledger::config::StorageMarketConfig {
            max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
        },
        block_rewards: lb_ledger::config::BlockRewardsConfig::new(
            3_000_000_000,
            120_000_000.try_into().unwrap(),
            10_512,
            NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
            ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
        )
        .unwrap(),
        gas_profile: lb_core::mantle::gas::GasProfile {
            transfer: 590.into(),
            channel_config: 56.into(),
            channel_inscribe: 56.into(),
            channel_deposit: 590.into(),
            channel_withdraw: 56.into(),
            channel_transfer: 56.into(),
            sdp_declare: 646.into(),
            sdp_withdraw: 590.into(),
            sdp_active: 590.into(),
            leader_claim: 580.into(),
            claim_pow_reward: 1.into(),
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
        accepted_fork_digests: AcceptedForkDigests::new(vec![ForkDigest::new([0x11; 32])]),
    }
}

/// Builds a block with no trasaction by grinding through slots
pub fn try_build_block(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    utxo: Utxo,
    key: &ZkKey,
    start_slot: Slot,
    uncle_headers: UncleHeaders,
) -> Option<(Block<SignedOps<Preverified, StandardMode>>, Ed25519Key)> {
    try_build_block_with_transactions(
        cryptarchia,
        parent,
        utxo,
        key,
        start_slot,
        uncle_headers,
        BlockTransactions::empty(),
    )
}

/// Builds a block carrying `transactions` by grinding through slots
pub fn try_build_block_with_transactions(
    cryptarchia: &Cryptarchia,
    parent: HeaderId,
    utxo: Utxo,
    key: &ZkKey,
    start_slot: Slot,
    uncle_headers: UncleHeaders,
    transactions: BlockTransactions<SignedOps<Preverified, StandardMode>>,
) -> Option<(Block<SignedOps<Preverified, StandardMode>>, Ed25519Key)> {
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

        let block = Block::create(
            parent,
            slot.into(),
            uncle_headers,
            proof,
            transactions,
            config_for_slot(cryptarchia.ledger.era_schedule(), slot.into()).max_transactions_size,
            &signing_key,
        )
        .unwrap();
        return Some((block, signing_key));
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
    Ed25519Key,
    ZkKey,
    Utxo,
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
    Ed25519Key,
    ZkKey,
    Utxo,
) {
    let genesis_id = GENESIS_ID.into();
    let (zk_key, utxo) = utxo();
    let mut cryptarchia = genesis_cryptarchia_on(utxo, era_schedule);

    // Both extend the genesis, and the same key wins the same slot, so the
    // two blocks differ only in their (randomly generated) block leaders.
    let (u1, u1_key) = try_build_block(
        &cryptarchia,
        genesis_id,
        utxo,
        &zk_key,
        Slot::new(1),
        UncleHeaders::empty(),
    )
    .unwrap();
    let (b1, _) = try_build_block(
        &cryptarchia,
        genesis_id,
        utxo,
        &zk_key,
        Slot::new(1),
        UncleHeaders::empty(),
    )
    .unwrap();
    let b1_header_slot = b1.header().slot();
    cryptarchia
        .try_apply_block(b1.clone(), b1_header_slot)
        .unwrap();

    (cryptarchia, b1, u1, u1_key, zk_key, utxo)
}

pub const GENESIS_ID: [u8; 32] = [0; 32];

/// A chain with only the genesis block, holding `utxo`.
pub fn genesis_cryptarchia(utxo: Utxo) -> Cryptarchia {
    genesis_cryptarchia_on(utxo, single_era(ledger_config(3.try_into().unwrap())))
}

/// A chain with only the genesis block, holding `utxo`, over the eras of
/// `era_schedule`.
pub fn genesis_cryptarchia_on(utxo: Utxo, era_schedule: Arc<EraScheduledConfig>) -> Cryptarchia {
    let genesis_config = era_schedule.genesis().parameters.clone();
    Cryptarchia::from_genesis(
        GENESIS_ID.into(),
        LedgerState::from_utxos([utxo], &genesis_config, 0, &ConsensusExcludedKeys::empty()),
        era_schedule,
        lb_cryptarchia_engine::State::Bootstrapping,
    )
}

pub fn signed_header(block: &Block<SignedOps<Preverified, StandardMode>>) -> SignedHeader {
    SignedHeader::new(block.header().clone(), *block.signature())
}

pub fn utxo() -> (ZkKey, Utxo) {
    let mut op_id = [0u8; 32];
    thread_rng().fill_bytes(&mut op_id);
    let zk_sk = ZkKey::from(Fr::ZERO);
    let utxo = Utxo {
        op_id,
        output_index: 0,
        note: Note::new(10000, zk_sk.to_public_key()),
    };
    (zk_sk, utxo)
}

pub fn spawn_storage_service(rx: mpsc::Receiver<StorageMsg>) -> (JoinHandle<()>, TempDir) {
    let db_dir = TempDir::new().unwrap();
    let handle = spawn_storage_service_at(rx, db_dir.path().join("db"));
    (handle, db_dir)
}

fn spawn_storage_service_at(
    mut rx: mpsc::Receiver<StorageMsg>,
    db_path: PathBuf,
) -> JoinHandle<()> {
    let backend = RocksBackend::new(RocksBackendSettings {
        db_path,
        read_only: false,
        column_family: None,
    })
    .unwrap();

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            StorageService::<TestRuntimeServiceId>::handle_storage_message(msg, &backend);
        }
    })
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
