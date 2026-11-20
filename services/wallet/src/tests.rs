use std::{
    collections::HashSet,
    fmt::{self, Display, Formatter},
    sync::Arc,
    time::Duration,
};

use futures::poll;
use lb_binary_codec::{
    bincode::{DeserializeOp as _, SerializeOp as _},
    canonical::BinaryEncode as _,
};
use lb_chain_service::{ConsensusMsg, CryptarchiaInfo, PhaseTag, Query, Slot, State};
use lb_core::{
    block::{BlockTransactions, UncleHeaders},
    events::{BlockEvent, Event},
    mantle::{
        gas::Gas,
        note_set::{CommitmentMmr, NoteSetUpdate},
        notes::derive_note_nullifier,
        ops::{
            sdp::{SDPActiveOp, SDPDeclareOp},
            transfer::TransferOp,
        },
        transactions::{GasPrices, tx_list::ops::OpsGasContext},
    },
    proofs::leader_proof::Groth16LeaderProof,
};
use lb_groth16::CompressedGroth16Proof;
use lb_key_management_system_service::{KMSService, message::KMSMessage};
use lb_storage_service::api::requests::StorageMsg;
use lb_time::era::{Era, EraEntriesAfterGenesis, EraTiming};
use overwatch::services::{
    relay::OutboundRelay,
    state::{NoOperator, NoState},
};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot::error::TryRecvError, watch};

use super::*;

#[test]
fn op_proof_pairs_each_operation_with_its_proof_type() {
    let zk = ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[1u8; 128]));
    let signature = Ed25519Signature::from_bytes(&[2u8; 64]);
    let transfer = TransferOp::sample();
    let declare = SDPDeclareOp::sample();
    let active = SDPActiveOp::sample();
    assert_eq!(
        op_proof(&OpRef::Transfer(&transfer), Some(zk), None).unwrap(),
        OpProof::ZkTransfer(zk)
    );
    assert_eq!(
        op_proof(&OpRef::SDPDeclare(&declare), Some(zk), Some(signature)).unwrap(),
        OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::new(zk, signature))
    );
    assert_eq!(
        op_proof(&OpRef::SDPActive(&active), None, Some(signature)).unwrap(),
        OpProof::Ed25519Sig(signature)
    );
    assert!(op_proof(&OpRef::Transfer(&transfer), None, None).is_err());
}

#[tokio::test]
async fn unavailable_events_leave_a_notified_block_for_backfill() {
    let known_note = KnownNote {
        note: Note::new(25, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64))),
        note_set: NoteSetIndex::Ledger,
        nullifier: NoteNullifier(Fr::from(3u64)),
    };
    let settings = wallet_settings(known_note.note.public_key);
    let genesis = HeaderId::from([0; 32]);
    let (sender, _receiver) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(sender));
    let mut state = wallet_for_backfill(&settings, genesis, known_note, &updater);

    let block = create_block(genesis, 1);
    let block_id = block.header().id();
    let child = create_block(block_id, 2);
    let child_id = child.header().id();
    let commitment = known_note.note.commitment();
    let events = Events::from(Event::Block(BlockEvent::NoteSetUpdated(NoteSetUpdate {
        note_set: NoteSetIndex::Ledger,
        nullifiers: vec![],
        commitments: vec![commitment],
    })));
    let storage = storage_for_backfill([block, child], events, None);
    let cryptarchia = chain_for_backfill(genesis, block_id, child_id);
    let (kms_sender, _) = mpsc::channel(1);
    let kms = KmsServiceApi::new(OutboundRelay::new(kms_sender));

    TestWallet::handle_new_block(
        block_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await;
    assert!(!state.wallet().has_processed_block(block_id));

    // The next notification backfills the missing parent, including its
    // events.
    TestWallet::handle_new_block(
        child_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await;

    assert!(state.wallet().has_processed_block(block_id));
    assert!(state.wallet().has_processed_block(child_id));
    let wallet_state = state.wallet().wallet_state_at(child_id).unwrap();
    assert_single_note_state(&wallet_state, known_note);
}

#[tokio::test]
async fn undecodable_events_stop_backfill_until_they_can_be_loaded() {
    let known_note = KnownNote {
        note: Note::new(25, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64))),
        note_set: NoteSetIndex::Ledger,
        nullifier: NoteNullifier(Fr::from(3u64)),
    };
    let settings = wallet_settings(known_note.note.public_key);
    let genesis = HeaderId::from([0; 32]);
    let (sender, _receiver) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(sender));
    let mut state = wallet_for_backfill(&settings, genesis, known_note, &updater);

    let block = create_block(genesis, 1);
    let block_id = block.header().id();
    let child = create_block(block_id, 2);
    let child_id = child.header().id();
    let commitment = known_note.note.commitment();
    let events = Events::from(Event::Block(BlockEvent::NoteSetUpdated(NoteSetUpdate {
        note_set: NoteSetIndex::Ledger,
        nullifiers: vec![],
        commitments: vec![commitment],
    })));
    let storage = storage_for_backfill(
        [block, child],
        events,
        Some(Bytes::from_static(b"invalid events")),
    );
    let cryptarchia = chain_for_backfill(genesis, block_id, child_id);
    let (kms_sender, _) = mpsc::channel(1);
    let kms = KmsServiceApi::new(OutboundRelay::new(kms_sender));

    let result = TestWallet::backfill_missing_blocks(
        child_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await;
    assert!(matches!(
        result,
        Err(WalletServiceError::BlockEventsUnavailable(id)) if id == block_id
    ));
    assert!(!state.wallet().has_processed_block(block_id));
    assert!(!state.wallet().has_processed_block(child_id));

    TestWallet::backfill_missing_blocks(
        child_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await
    .unwrap();

    assert!(state.wallet().has_processed_block(block_id));
    assert!(state.wallet().has_processed_block(child_id));
    let wallet_state = state.wallet().wallet_state_at(child_id).unwrap();
    assert_single_note_state(&wallet_state, known_note);
}

#[tokio::test]
async fn change_waits_for_storage_and_is_spendable_after_restore() {
    // A 100-value input pays 40 to another key and returns 60 in change.
    let key = ZkKey::new(Fr::from(7u64));
    let input = known_note(&key, 100, 1);
    let settings = wallet_settings(input.note.public_key);

    let (checkpoint_sender, checkpoint_updates) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(checkpoint_sender));
    let mut state = wallet_state(&settings, &[input], &updater);

    let owner = input.note.public_key;
    let nonce = Fr::from(2u64);
    let tx = MantleTxBuilder::new()
        .add_ledger_output(Note::new(
            40,
            Fr::from(3u64),
            ZkPublicKey::new(Fr::from(4u64)),
        ))
        .unwrap();
    let funded = state
        .fund_tx(
            HeaderId::from([0; 32]),
            &tx,
            owner,
            nonce,
            [owner],
            &free_context(),
            0,
        )
        .unwrap();
    let (storage, mut writes) = storage_api();
    let kms = kms_api(key);

    // A failed atomic write must not register the change in memory.
    let expected_change = Note::new(60, nonce, owner);
    let (failed, failed_records) = complete_opening_write(
        TestWallet::add_change_note(&funded, owner, nonce, &mut state, &storage, &kms),
        &mut writes,
        Err("write failed".into()),
    )
    .await;
    assert!(matches!(failed, Err(WalletServiceError::PersistedState(_))));
    assert_eq!(failed_records.len(), 0);
    assert!(
        !state
            .wallet()
            .known_notes()
            .contains_key(&expected_change.commitment())
    );

    // Retry the save, then rebuild the wallet and spend the change.
    let (saved, records) = complete_opening_write(
        TestWallet::add_change_note(&funded, owner, nonce, &mut state, &storage, &kms),
        &mut writes,
        Ok(()),
    )
    .await;
    saved.unwrap();
    assert_eq!(
        state.wallet().known_notes()[&expected_change.commitment()].note,
        expected_change
    );
    assert!(checkpoint_updates.borrow().is_none());
    assert_restored_notes_are_spendable(&settings, input, &records, &funded, &kms).await;
}

#[tokio::test]
async fn split_response_waits_for_an_atomic_save_and_a_failed_write_can_be_retried() {
    let key = ZkKey::new(Fr::from(7u64));
    let input = known_note(&key, 100, 1);
    let settings = wallet_settings(input.note.public_key);

    let (checkpoint_sender, _checkpoint_updates) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(checkpoint_sender));
    let mut state = wallet_state(&settings, &[input], &updater);

    let kms = kms_api(key);

    // The entire split save fails; neither output is registered.
    let (failed, failed_records) = split_with_storage_result(
        &mut state,
        &settings,
        input.note.public_key,
        &kms,
        Err("write failed".into()),
    )
    .await;
    assert!(matches!(failed, Err(WalletServiceError::PersistedState(_))));
    assert_eq!(failed_records.len(), 0);
    assert_eq!(state.wallet().known_notes().len(), 1); // Only the original input.

    // Retry on the same wallet. Both outputs must be saved before it replies.
    let (succeeded, saved) =
        split_with_storage_result(&mut state, &settings, input.note.public_key, &kms, Ok(())).await;
    let (tx, exact) = succeeded.unwrap();
    assert_eq!(exact.note.value, 25);

    // Restore with the successful save and spend the split outputs.
    assert_restored_notes_are_spendable(&settings, input, &saved, &tx, &kms).await;
}

#[tokio::test]
async fn leader_reward_waits_for_storage_before_becoming_known() {
    let key = ZkKey::new(Fr::from(7u64));
    let input = known_note(&key, 100, 1);
    let settings = wallet_settings(input.note.public_key);

    let (checkpoint_sender, _checkpoint_updates) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(checkpoint_sender));
    let mut state = wallet_state(&settings, &[input], &updater);

    let (storage, mut writes) = storage_api();
    let kms = kms_api(key);

    let (reward, records) = complete_opening_write(
        TestWallet::new_leader_reward(20, input.note.public_key, &mut state, &storage, &kms),
        &mut writes,
        Ok(()),
    )
    .await;
    let reward = reward.unwrap();
    let stored = storage_with_records(records)
        .load_wallet_known_notes()
        .await
        .unwrap();
    assert_eq!(stored[&reward.commitment()].note, reward);
    assert_eq!(
        state.wallet().known_notes().get(&reward.commitment()),
        stored.get(&reward.commitment())
    );
}

#[tokio::test]
async fn imported_note_waits_for_storage_before_becoming_known() {
    let key = ZkKey::new(Fr::from(7u64));
    let input = known_note(&key, 100, 1);
    let settings = wallet_settings(input.note.public_key);

    let (checkpoint_sender, _checkpoint_updates) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(checkpoint_sender));
    let mut state = wallet_state(&settings, &[input], &updater);

    let (storage, mut writes) = storage_api();
    let kms = kms_api(key);

    let note = Note::new(20, Fr::from(8u64), input.note.public_key);

    let (imported, records) = complete_opening_write(
        TestWallet::import_note(note, NoteSetIndex::Ledger, &mut state, &storage, &kms),
        &mut writes,
        Ok(()),
    )
    .await;
    let imported = imported.unwrap();
    let stored = storage_with_records(records)
        .load_wallet_known_notes()
        .await
        .unwrap();
    assert_eq!(stored.get(&note.commitment()), Some(&imported));
    assert_eq!(
        state.wallet().known_notes().get(&note.commitment()),
        Some(&imported)
    );
}

#[tokio::test]
async fn reward_keys_wait_for_the_saved_count_and_are_registered_again_on_startup() {
    let key = ZkKey::new(Fr::from(7u64));
    let settings = wallet_settings(key.to_public_key());

    // Save the wallet before any reward key has been generated.
    let (checkpoint_sender, checkpoint_updates) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(checkpoint_sender));
    let mut state = wallet_state(&settings, &[], &updater);
    state.advance_lib(HeaderId::from([0; 32]), 0);
    let checkpoint_bytes = checkpoint_updates
        .borrow()
        .clone()
        .unwrap()
        .to_bytes()
        .unwrap();

    // Keep storage replies under the test's control.
    let (storage, mut writes) = storage_api();
    let cryptarchia = chain_api();
    let kms = kms_api(key.clone());

    let mut saved_count_record = None;

    // A failed write uses index 0 without returning a key. The next two keys
    // are returned only after their counts are saved, including the skipped
    // index.
    for (expected_count, write_result) in
        [(1, Err("write failed".into())), (2, Ok(())), (3, Ok(()))]
    {
        let write_succeeds = write_result.is_ok();
        let (resp_tx, mut response) = oneshot::channel();
        let era = settings.era_schedule.get(Era::genesis()).unwrap();
        let mut handle = Box::pin(TestWallet::handle_wallet_message(
            WalletMsg::GenerateRewardKey { resp_tx },
            &mut state,
            &settings.voucher_master_key_id,
            &storage,
            &cryptarchia,
            &kms,
            &settings.era_schedule,
            era.parameters,
        ));
        let message = tokio::select! {
            () = &mut handle => panic!("responded before the key count was written"),
            message = writes.recv() => message.unwrap(),
        };
        let StorageMsg::StoreConfirmed {
            key,
            value,
            reply_channel,
        } = message
        else {
            panic!("expected a confirmed reward-key count write");
        };
        assert_eq!(key, Bytes::from_static(b"wallet/reward_key_count"));
        assert_eq!(u64::from_bytes(&value).unwrap(), expected_count);
        assert!(poll!(handle.as_mut()).is_pending());
        assert!(matches!(response.try_recv(), Err(TryRecvError::Empty)));
        if write_succeeds {
            saved_count_record = Some((key, value));
        }
        reply_channel.send(write_result).unwrap();
        handle.await;
        let result = response.await.unwrap();
        if write_succeeds {
            assert!(state.wallet().known_keys().contains_key(&result.unwrap()));
        } else {
            assert!(matches!(result, Err(WalletServiceError::PersistedState(_))));
        }
    }

    let expected_keys = state.wallet().known_keys().clone();
    let storage = storage_with_records(HashMap::from([saved_count_record.unwrap()]));
    let fresh_kms = kms_api(key);
    let restored = TestWallet::start_state(
        RecoveryState::from_bytes(&checkpoint_bytes).unwrap(),
        &settings,
        &updater,
        &cryptarchia,
        &fresh_kms,
        &storage,
    )
    .await
    .unwrap();
    assert_eq!(restored.reward_key_count(), 3);
    assert_eq!(restored.wallet().known_keys(), &expected_keys);
}

// Storage acknowledgements and recovery assertions.

// The tests control only the batch acknowledgement. Keys and values belong to
// the storage API and are kept opaque until a restore reads them through it.
async fn complete_opening_write<Output>(
    operation: impl Future<Output = Result<Output, WalletServiceError>>,
    writes: &mut mpsc::Receiver<StorageMsg>,
    write_result: Result<(), String>,
) -> (Result<Output, WalletServiceError>, HashMap<Bytes, Bytes>) {
    let mut operation = Box::pin(operation);
    let message = tokio::select! {
        _ = &mut operation => panic!("returned before the opening save"),
        message = writes.recv() => message.unwrap(),
    };
    let StorageMsg::BulkStoreConfirmed {
        items,
        reply_channel,
    } = message
    else {
        panic!("expected an atomic opening save");
    };
    assert!(poll!(operation.as_mut()).is_pending());
    let saved = if write_result.is_ok() {
        items
    } else {
        HashMap::new()
    };
    reply_channel.send(write_result).unwrap();
    (operation.await, saved)
}

// Sends a real PrepareExactNote request. Its reply must wait for the one
// atomic save containing the openings and their ownership information.
async fn split_with_storage_result(
    state: &mut ServiceState<'_>,
    settings: &WalletServiceSettings,
    owner: ZkPublicKey,
    kms: &KmsServiceApi<TestKms, TestRuntimeServiceId>,
    write_result: Result<(), String>,
) -> (
    Result<(MantleTxBuilder, KnownNote), WalletServiceError>,
    HashMap<Bytes, Bytes>,
) {
    let (storage, mut writes) = storage_api();
    let cryptarchia = chain_api();
    let era = settings.era_schedule.get(Era::genesis()).unwrap();
    let (resp_tx, mut response) = oneshot::channel();
    let mut handle = Box::pin(TestWallet::handle_wallet_message(
        WalletMsg::PrepareExactNote {
            tip: Some(HeaderId::from([0; 32])),
            tx_builder: MantleTxBuilder::new(),
            value: 25,
            owner,
            funding_pks: vec![owner],
            resp_tx,
        },
        state,
        &settings.voucher_master_key_id,
        &storage,
        &cryptarchia,
        kms,
        &settings.era_schedule,
        era.parameters,
    ));
    let message = tokio::select! {
        () = &mut handle => panic!("responded before saving the split"),
        message = writes.recv() => message.unwrap(),
    };
    let StorageMsg::BulkStoreConfirmed {
        items,
        reply_channel,
    } = message
    else {
        panic!("expected an atomic split save");
    };
    assert!(poll!(handle.as_mut()).is_pending());
    assert!(matches!(response.try_recv(), Err(TryRecvError::Empty)));
    let saved = if write_result.is_ok() {
        items
    } else {
        HashMap::new()
    };
    reply_channel.send(write_result).unwrap();
    handle.await;
    (response.await.unwrap().map(|reply| reply.response), saved)
}

async fn assert_restored_notes_are_spendable(
    settings: &WalletServiceSettings,
    input: KnownNote,
    saved: &HashMap<Bytes, Bytes>,
    tx: &MantleTxBuilder,
    kms: &KmsServiceApi<TestKms, TestRuntimeServiceId>,
) {
    let (checkpoint_sender, checkpoint_updates) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(checkpoint_sender));
    let mut before = wallet_state(settings, &[input], &updater);
    before.advance_lib(HeaderId::from([0; 32]), 0);
    let checkpoint_bytes = checkpoint_updates
        .borrow()
        .clone()
        .unwrap()
        .to_bytes()
        .unwrap();
    drop(before);

    let (storage, mut writes) = storage_api();
    let (saved_input, mut records) = complete_opening_write(
        async {
            storage
                .store_wallet_known_notes(&[input])
                .await
                .map_err(WalletServiceError::PersistedState)
        },
        &mut writes,
        Ok(()),
    )
    .await;
    saved_input.unwrap();
    records.extend(saved.clone());
    let storage = storage_with_records(records);

    let cryptarchia = chain_api();

    // Restore once through the service's checkpoint startup, and once from
    // genesis.
    let from_checkpoint = TestWallet::start_state(
        RecoveryState::from_bytes(&checkpoint_bytes).unwrap(),
        settings,
        &updater,
        &cryptarchia,
        kms,
        &storage,
    )
    .await
    .unwrap();
    let from_genesis = ServiceState::from_genesis(
        storage.load_wallet_known_notes().await.unwrap(),
        0,
        settings,
        HeaderId::from([0; 32]),
        &[input.note],
        &[],
        &updater,
    )
    .unwrap();
    for mut restored in [from_checkpoint, from_genesis] {
        let block_id = HeaderId::from([1; 32]);
        let outputs = tx
            .openings()
            .into_values()
            .flat_map(|opening| opening.outputs)
            .collect::<Vec<_>>();
        restored
            .apply_block(&WalletBlock {
                id: block_id,
                parent: HeaderId::from([0; 32]),
                epoch: Epoch::new(0),
                revealed: vec![],
                withdrawals: vec![],
                updates: vec![NoteSetUpdate {
                    note_set: NoteSetIndex::Ledger,
                    nullifiers: vec![input.nullifier],
                    commitments: outputs.iter().map(Note::commitment).collect(),
                }],
            })
            .unwrap();
        let state = restored.wallet().wallet_state_at(block_id).unwrap();
        let owned_outputs = outputs
            .iter()
            .filter(|note| note.public_key == input.note.public_key)
            .collect::<Vec<_>>();
        assert_eq!(state.notes.size(), owned_outputs.len());
        for note in owned_outputs {
            let owned = state.notes.get(&note.commitment()).unwrap();
            assert_eq!(owned.note, *note);
            assert_eq!(
                owned.nullifier,
                restored.wallet().known_notes()[&note.commitment()].nullifier
            );
        }
        let funded = state
            .fund_tx(
                &MantleTxBuilder::new()
                    .add_ledger_output(Note::new(1, Fr::from(99u64), input.note.public_key))
                    .unwrap(),
                input.note.public_key,
                Fr::from(100u64),
                [input.note.public_key],
                &free_context(),
                &HashSet::new(),
                0,
            )
            .unwrap();
        assert_ne!(funded.funding_inputs(), []);
    }
}

// Service stubs: only the messages these tests use.

type TestTx = SignedOps<Preverified, StandardMode>;
type TestKms = KMSService<KmsBackend, TestRuntimeServiceId>;
type TestWallet = WalletService<TestKms, DummyCryptarchia, TestTx, (), TestRuntimeServiceId>;

struct DummyCryptarchia;

impl ServiceData for DummyCryptarchia {
    type Settings = ();
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ConsensusMsg<TestTx>;
}

#[derive(Debug)]
struct TestRuntimeServiceId;

impl AsServiceId<DummyCryptarchia> for TestRuntimeServiceId {
    const SERVICE_ID: Self = Self;
}

impl AsServiceId<TestKms> for TestRuntimeServiceId {
    const SERVICE_ID: Self = Self;
}

impl Display for TestRuntimeServiceId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "TestRuntimeServiceId")
    }
}

pub fn storage_with_records(records: HashMap<Bytes, Bytes>) -> StorageApi<TestTx> {
    let (sender, mut requests) = mpsc::channel(1);
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            match request {
                StorageMsg::Load { key, reply_channel } => {
                    drop(reply_channel.send(records.get(&key).cloned()));
                }
                StorageMsg::LoadPrefix {
                    prefix,
                    reply_channel,
                    ..
                } => {
                    let values = records
                        .iter()
                        .filter(|(key, _)| key.starts_with(&prefix))
                        .map(|(_, value)| value.clone())
                        .collect();
                    drop(reply_channel.send(values));
                }
                _ => panic!("unexpected storage request"),
            }
        }
    });
    StorageApi::new(OutboundRelay::new(sender))
}

fn storage_api() -> (StorageApi<TestTx>, mpsc::Receiver<StorageMsg>) {
    let (sender, receiver) = mpsc::channel(1);
    (StorageApi::new(OutboundRelay::new(sender)), receiver)
}

fn chain_api() -> CryptarchiaServiceApi<DummyCryptarchia> {
    let (sender, mut receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        while let Some(ConsensusMsg::Query(Query::Info { reply_channel })) = receiver.recv().await {
            let genesis = HeaderId::from([0; 32]);
            reply_channel
                .send(ChainServiceInfo {
                    cryptarchia_info: CryptarchiaInfo {
                        lib: genesis,
                        lib_slot: Slot::from(0),
                        tip: genesis,
                        slot: Slot::from(0),
                        height: 0,
                        state: State::Online,
                    },
                    phase: PhaseTag::Following,
                })
                .unwrap();
        }
    });
    CryptarchiaServiceApi::new(OutboundRelay::new(sender))
}

fn kms_api(key: ZkKey) -> KmsServiceApi<TestKms, TestRuntimeServiceId> {
    let (sender, mut receiver) = mpsc::channel::<KMSMessage<KmsBackend>>(1);
    tokio::spawn(async move {
        let mut keys = HashMap::from([
            ("owner".to_owned(), Key::Zk(key.clone())),
            ("voucher-master".to_owned(), Key::Zk(key)),
        ]);
        while let Some(message) = receiver.recv().await {
            match message {
                KMSMessage::Execute {
                    key_id,
                    operator,
                    reply_channel,
                } => {
                    reply_channel
                        .send(keys[&key_id].execute(operator).await.map_err(Into::into))
                        .unwrap();
                }
                KMSMessage::Register {
                    key_id,
                    key_type,
                    reply_channel,
                } => {
                    let public_key = key_type.as_public_key();
                    keys.insert(key_id.clone(), key_type);
                    reply_channel.send(Ok((key_id, public_key))).unwrap();
                }
                _ => panic!("unexpected KMS request"),
            }
        }
    });
    KmsServiceApi::new(OutboundRelay::new(sender))
}

// Shared note, wallet and era data.

pub fn known_note(key: &ZkKey, value: Value, nonce: u64) -> KnownNote {
    let note = Note::new(value, Fr::from(nonce), key.to_public_key());
    KnownNote {
        note,
        note_set: NoteSetIndex::Ledger,
        nullifier: derive_note_nullifier(&note.commitment(), &key.nullifier_key()),
    }
}

fn wallet_settings(owner: ZkPublicKey) -> WalletServiceSettings {
    WalletServiceSettings {
        known_keys: HashMap::from([("owner".into(), owner)]),
        voucher_master_key_id: "voucher-master".into(),
        recovery_data: RecoveryData::default(),
        pending_note_expiry_blocks: 10,
        fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
        era_schedule: era_schedule(),
    }
}

fn wallet_state<'u>(
    settings: &WalletServiceSettings,
    notes: &[KnownNote],
    updater: &'u StateUpdater<Option<RecoveryState>>,
) -> ServiceState<'u> {
    let known_notes = notes
        .iter()
        .map(|note| (note.note.commitment(), *note))
        .collect();
    let genesis_notes = notes.iter().map(|note| note.note).collect::<Vec<_>>();
    ServiceState::from_genesis(
        known_notes,
        0,
        settings,
        HeaderId::from([0; 32]),
        &genesis_notes,
        &[],
        updater,
    )
    .unwrap()
}

fn free_context() -> OpsContext {
    OpsContext {
        gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0), GAS_PROFILE),
    }
}

/// A schedule of one era, with epochs of 100 slots and a `k` of 5.
pub fn era_schedule() -> ServiceEraSchedule {
    let timing = EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: NonZero::new(100).unwrap(),
    };
    EraSchedule::new(
        OffsetDateTime::UNIX_EPOCH,
        timing,
        EraEntriesAfterGenesis::empty(),
    )
    .unwrap()
    .map(|_| WalletEraSettings {
        epoch_length: timing.epoch_length,
        security_param: NonZero::new(5).unwrap(),
        fork_digest: ForkDigest::new([0x11; 32]),
        gas_profile: GAS_PROFILE,
    })
}

/// The costs of the deployments' current gas profile.
pub const GAS_PROFILE: GasProfile = GasProfile {
    transfer: Gas::new(10),
    program_register: Gas::new(15114),
    channel_config: Gas::new(24),
    channel_inscribe: Gas::new(26),
    channel_deposit: Gas::new(26),
    channel_withdraw: Gas::new(33),
    sdp_declare: Gas::new(1693),
    sdp_withdraw: Gas::new(0),
    sdp_active: Gas::new(15117),
    claim_pow_reward: Gas::new(1506),
    kilobyte: Gas::new(30),
    note_set: Gas::new(8961),
    proof: Gas::new(4177),
    root: Gas::new(2332),
    buffered: Gas::new(60),
    nullifier: Gas::new(1758),
    commitment: Gas::new(58),
    signature: Gas::new(557),
    user_step: Gas::new(28),
    program_deposit: Gas::new(144),
    program_step: Gas::new(13528),
    opening: Gas::new(470),
};

fn wallet_for_backfill<'u>(
    settings: &WalletServiceSettings,
    genesis: HeaderId,
    known_note: KnownNote,
    updater: &'u StateUpdater<Option<RecoveryState>>,
) -> ServiceState<'u> {
    ServiceState::from_genesis(
        HashMap::from([(known_note.note.commitment(), known_note)]),
        0,
        settings,
        genesis,
        &[],
        &[],
        updater,
    )
    .unwrap()
}

fn assert_single_note_state(state: &WalletState, known_note: KnownNote) {
    let commitment = known_note.note.commitment();
    assert_eq!(state.notes.size(), 1);
    let owned_note = state.notes.get(&commitment).unwrap();
    assert_eq!(owned_note.note, known_note.note);
    assert_eq!(owned_note.nullifier, known_note.nullifier);
    assert_eq!(
        state.balance(known_note.note.public_key).unwrap().balance,
        known_note.note.value,
    );
    let expected = CommitmentMmr::new().push(commitment).unwrap();
    assert_eq!(
        state
            .commitments
            .get(&NoteSetIndex::Ledger)
            .unwrap()
            .frontier_root(),
        expected.frontier_root(),
    );
}

fn create_block(parent: HeaderId, slot: u64) -> Block<TestTx> {
    Block::create(
        parent,
        Slot::new(slot),
        UncleHeaders::empty(),
        Groth16LeaderProof::genesis(),
        BlockTransactions::empty(),
        u32::MAX,
    )
    .unwrap()
}

fn storage_for_backfill(
    blocks: [Block<TestTx>; 2],
    block_events: Events,
    first_event_response: Option<Bytes>,
) -> StorageApi<TestTx> {
    let [block, child] = blocks;
    let block_id = block.header().id();
    let child_id = child.header().id();
    let blocks = HashMap::from([
        (block_id, Bytes::from(block.encode_to_vec())),
        (child_id, Bytes::from(child.encode_to_vec())),
    ]);
    let events = HashMap::from([
        (block_id, Bytes::try_from(block_events).unwrap()),
        (child_id, Bytes::try_from(Events::new()).unwrap()),
    ]);
    let (msg_sender, mut msg_receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut fail_first_event_read = true;
        while let Some(msg) = msg_receiver.recv().await {
            match msg {
                StorageMsg::GetBlock {
                    header_id,
                    response_tx,
                } => response_tx.send(blocks.get(&header_id).cloned()).unwrap(),
                StorageMsg::GetBlockEvents {
                    header_id,
                    response_tx,
                } => {
                    let response = if header_id == block_id && fail_first_event_read {
                        fail_first_event_read = false;
                        first_event_response.clone()
                    } else {
                        events.get(&header_id).cloned()
                    };
                    response_tx.send(response).unwrap();
                }
                _ => panic!("Unexpected storage request"),
            }
        }
    });
    StorageApi::new(OutboundRelay::new(msg_sender))
}

fn chain_for_backfill(
    genesis: HeaderId,
    block: HeaderId,
    child: HeaderId,
) -> CryptarchiaServiceApi<DummyCryptarchia> {
    let (msg_sender, mut msg_receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        while let Some(msg) = msg_receiver.recv().await {
            let ConsensusMsg::Query(Query::GetHeaders {
                from_descendant,
                to_ancestor,
                reply_channel,
            }) = msg
            else {
                panic!("Unexpected chain query");
            };
            assert_eq!(from_descendant, Some(child));
            assert_eq!(to_ancestor, Some(genesis));
            let headers = futures::stream::iter([Ok(child), Ok(block), Ok(genesis)]);
            assert!(reply_channel.send(Box::pin(headers)).is_ok());
        }
    });
    CryptarchiaServiceApi::new(OutboundRelay::new(msg_sender))
}
