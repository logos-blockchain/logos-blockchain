//! Inbound path for applying channel history to participant-local state.

use lb_zone_sdk::{
    node_types::ChannelId,
    sequencer::{
        ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, InscriptionInfo,
        channel_inscriptions,
    },
};

use crate::{
    db::Databases,
    error::Error,
    protocol::{self, ChannelInscription, TxId},
};

const TARGET: &str = lb_log_targets::logos_sql::APPLIER;

#[derive(Clone, Copy, Debug)]
enum WriteState {
    Adopted,
    Finalized,
}

impl WriteState {
    fn apply(self, db: &mut Databases, write: &ChannelInscription) -> Result<(), Error> {
        match self {
            Self::Adopted => db.apply_adopted_write(write),
            Self::Finalized => db.apply_finalized_write(write),
        }
    }
}

/// Handles one sequencer event.
///
/// Every `BlocksProcessed` checkpoint is persisted, including events that
/// contain no `λSQL` writes, so restart resumes from the latest processed
/// block.
/// Finalized writes apply to both finalized and live state, while newly
/// adopted writes apply only to live state. Orphan recovery is deferred.
///
/// # Errors
///
/// Returns an error if SQL cannot be applied, a channel payload cannot be
/// decoded, or the checkpoint cannot be persisted.
///
/// # Panics
///
/// Panics when an orphaned `λSQL` transaction reaches the unfinished rebuild
/// path.
pub fn on_event(db: &mut Databases, event: &Event, channel_id: ChannelId) -> Result<(), Error> {
    match event {
        Event::BlocksProcessed {
            checkpoint,
            channel_update,
            finalized,
        } => {
            tracing::debug!(
                target: TARGET,
                adopted_txs = channel_update.adopted.len(),
                orphaned_txs = channel_update.orphaned.len(),
                finalized_txs = finalized.len(),
                "blocks processed"
            );

            if orphaned_contains_logos_sql(channel_update, channel_id) {
                todo!("reconcile orphaned \u{3bb}SQL transactions");
            }

            apply_finalized(db, finalized)?;
            apply_adopted(db, &channel_update.adopted, channel_id)?;

            db.persist_checkpoint(checkpoint)?;
        }
        Event::Ready => {
            tracing::info!(target: TARGET, "sequencer ready");
        }
        Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
    }

    Ok(())
}

fn orphaned_contains_logos_sql(channel_update: &ChannelUpdate, channel_id: ChannelId) -> bool {
    channel_update
        .orphaned
        .iter()
        .any(|transaction| transaction_contains_logos_sql(transaction, channel_id))
}

fn apply_finalized(db: &mut Databases, finalized: &[FinalizedTx]) -> Result<(), Error> {
    for transaction in finalized {
        for operation in &transaction.ops {
            let FinalizedOp::Inscription(inscription) = operation else {
                continue;
            };

            apply_inscription(db, inscription, WriteState::Finalized)?;
        }
    }

    Ok(())
}

fn apply_adopted(
    db: &mut Databases,
    adopted: &[ChannelUpdateTx],
    channel_id: ChannelId,
) -> Result<(), Error> {
    for transaction in adopted {
        if let Some(inscription) = transaction.inscription() {
            apply_inscription(db, inscription, WriteState::Adopted)?;
            continue;
        }

        let ChannelUpdateTx::Custom(transaction) = transaction else {
            continue;
        };

        for inscription in channel_inscriptions(transaction, channel_id) {
            apply_inscription(db, &inscription, WriteState::Adopted)?;
        }
    }

    Ok(())
}

fn apply_inscription(
    db: &mut Databases,
    inscription: &InscriptionInfo,
    state: WriteState,
) -> Result<(), Error> {
    let payload = inscription.payload.as_ref();

    if !protocol::is_logos_sql_payload(payload) {
        return Ok(());
    }

    let write = match ChannelInscription::decode(payload) {
        Ok(write) => write,
        Err(error) => return handle_write_error(db, inscription, None, error),
    };

    if let Err(error) = state.apply(db, &write) {
        return handle_write_error(db, inscription, Some(write.tx_id), error);
    }

    tracing::debug!(
        target: TARGET,
        tx_id = ?write.tx_id,
        statements = write.transaction.statements().len(),
        ?state,
        "channel write processed"
    );

    Ok(())
}

fn handle_write_error(
    db: &Databases,
    inscription: &InscriptionInfo,
    tx_id: Option<TxId>,
    error: Error,
) -> Result<(), Error> {
    if is_rejected_write(&error) {
        record_rejection(db, inscription, tx_id, &error)
    } else {
        Err(error)
    }
}

fn record_rejection(
    db: &Databases,
    inscription: &InscriptionInfo,
    tx_id: Option<TxId>,
    error: &Error,
) -> Result<(), Error> {
    db.record_rejected_write(inscription.this_msg, tx_id, &error.to_string())?;

    tracing::warn!(
        target: TARGET,
        this_msg = %inscription.this_msg,
        ?tx_id,
        %error,
        "channel write rejected"
    );

    Ok(())
}

const fn is_rejected_write(error: &Error) -> bool {
    matches!(error, Error::InvalidPayload(_) | Error::RejectedSql(_))
}

fn transaction_contains_logos_sql(transaction: &ChannelUpdateTx, channel_id: ChannelId) -> bool {
    if let Some(inscription) = transaction.inscription() {
        return protocol::is_logos_sql_payload(inscription.payload.as_ref());
    }

    let ChannelUpdateTx::Custom(transaction) = transaction else {
        return false;
    };

    channel_inscriptions(transaction, channel_id)
        .iter()
        .any(|inscription| protocol::is_logos_sql_payload(inscription.payload.as_ref()))
}

#[cfg(test)]
mod tests {
    use lb_zone_sdk::{
        Ed25519PublicKey,
        node_types::{ChannelId, HeaderId, MsgId, Slot, TxHash},
        sequencer::{
            ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, InscriptionInfo,
            SequencerCheckpoint,
        },
    };
    use rusqlite::types::Value;
    use tempfile::TempDir;

    use super::on_event;
    use crate::{
        db::Databases,
        protocol::{ChannelInscription, EncodedWrite, PAYLOAD_MARKER, Statement, Transaction},
    };

    const CHANNEL_ID: [u8; 32] = [9; 32];

    fn checkpoint(byte: u8, slot: u64) -> SequencerCheckpoint {
        SequencerCheckpoint {
            last_msg_id: MsgId::root(),
            pending_txs: Vec::new(),
            lib: HeaderId::from([byte; 32]),
            lib_slot: Slot::from(slot),
            channel_notes: Vec::new(),
            finalized_config: MsgId::root(),
        }
    }

    fn inscription(payload: &[u8], byte: u8) -> InscriptionInfo {
        InscriptionInfo {
            tx_hash: TxHash::from([byte; 32]),
            parent_msg: MsgId::root(),
            this_msg: MsgId::from([byte; 32]),
            payload: payload
                .to_vec()
                .try_into()
                .expect("test payload should fit an inscription"),
            signer: Some(Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap()),
        }
    }

    fn blocks_processed(
        checkpoint: SequencerCheckpoint,
        adopted: Vec<ChannelUpdateTx>,
        orphaned: Vec<ChannelUpdateTx>,
        finalized: Vec<FinalizedTx>,
    ) -> Event {
        Event::BlocksProcessed {
            checkpoint,
            channel_update: ChannelUpdate { orphaned, adopted },
            finalized,
        }
    }

    fn finalized(payload: &[u8], byte: u8) -> FinalizedTx {
        FinalizedTx {
            tx_hash: TxHash::from([byte; 32]),
            l1_slot: Slot::from(2),
            ops: vec![FinalizedOp::Inscription(inscription(payload, byte))],
        }
    }

    fn transaction(sql: &str, params: Vec<Value>) -> Transaction {
        Transaction::new(vec![
            Statement::new(sql.to_owned(), params).expect("statement should be valid"),
        ])
        .expect("transaction should be valid")
    }

    fn encoded_write(transaction: &Transaction) -> EncodedWrite {
        EncodedWrite::new(transaction).expect("payload should encode")
    }

    fn item_count(path: &std::path::Path) -> i64 {
        let connection = Databases::open_reader(path).expect("database should open for reading");

        connection
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .expect("item count should be readable")
    }

    fn table_exists(path: &std::path::Path, table: &str) -> bool {
        let connection = Databases::open_reader(path).expect("database should open for reading");

        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
                )",
                [table],
                |row| row.get(0),
            )
            .expect("schema should be readable")
    }

    #[test]
    fn adopted_writes_apply_to_live_in_channel_order() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();
        let lib_path = db.lib_path().to_owned();

        let create = encoded_write(&transaction(
            "CREATE TABLE items(value INTEGER NOT NULL)",
            Vec::new(),
        ));
        let insert = encoded_write(&transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Integer(1)],
        ));

        let event = blocks_processed(
            checkpoint(2, 2),
            vec![
                ChannelUpdateTx::Inscription(inscription(&create.payload, 1)),
                ChannelUpdateTx::Inscription(inscription(&insert.payload, 2)),
            ],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("adopted writes should apply");

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("replayed event should be skipped");

        assert_eq!(item_count(&live_path), 1);
        assert!(!table_exists(&lib_path, "items"));
    }

    #[test]
    fn finalized_backfill_applies_to_lib_and_live_once() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();
        let lib_path = db.lib_path().to_owned();

        let create = encoded_write(&transaction(
            "CREATE TABLE items(value INTEGER NOT NULL)",
            Vec::new(),
        ));
        let insert = encoded_write(&transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Integer(1)],
        ));

        let event = blocks_processed(
            checkpoint(2, 2),
            Vec::new(),
            Vec::new(),
            vec![finalized(&create.payload, 1), finalized(&insert.payload, 2)],
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("finalized writes should apply");
        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("replayed event should be skipped");

        assert_eq!(item_count(&lib_path), 1);
        assert_eq!(item_count(&live_path), 1);
    }

    #[test]
    fn replay_after_apply_completes_checkpoint_without_duplicate_effects() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();
        let transaction = Transaction::new(vec![
            Statement::new(
                "CREATE TABLE items(value INTEGER NOT NULL)".to_owned(),
                Vec::new(),
            )
            .expect("statement should be valid"),
            Statement::new(
                "INSERT INTO items(value) VALUES (?1)".to_owned(),
                vec![Value::Integer(1)],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let encoded = encoded_write(&transaction);
        let write = ChannelInscription::decode(&encoded.payload).expect("payload should decode");

        // Simulate a crash after SQL and its applied marker commit but before
        // the ZoneSDK checkpoint is persisted.
        db.apply_adopted_write(&write)
            .expect("write should apply before the simulated crash");
        drop(db);

        let mut db = Databases::open(dir.path()).expect("databases should reopen");

        let expected_checkpoint = checkpoint(2, 2);
        let event = blocks_processed(
            expected_checkpoint.clone(),
            vec![ChannelUpdateTx::Inscription(inscription(
                &encoded.payload,
                1,
            ))],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("replayed event should complete");

        let restored = db
            .load_checkpoint()
            .expect("checkpoint should load")
            .expect("checkpoint should exist");

        assert_eq!(item_count(&live_path), 1);
        assert_eq!(restored.lib, expected_checkpoint.lib);
        assert_eq!(restored.lib_slot, expected_checkpoint.lib_slot);
    }

    #[test]
    fn locally_applied_write_is_not_executed_when_adopted() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let setup = transaction("CREATE TABLE items(value INTEGER NOT NULL)", Vec::new());
        let setup_encoded = EncodedWrite::new(&setup).expect("setup write should encode");

        db.commit_local_write(&setup, &setup_encoded)
            .expect("setup write should commit");
        db.mark_publish_complete(setup_encoded.tx_id)
            .expect("setup publish should be complete");

        let insert = transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Integer(1)],
        );
        let insert_encoded = EncodedWrite::new(&insert).expect("insert write should encode");

        db.commit_local_write(&insert, &insert_encoded)
            .expect("insert write should commit");

        let event = blocks_processed(
            checkpoint(2, 2),
            vec![ChannelUpdateTx::Inscription(inscription(
                &insert_encoded.payload,
                2,
            ))],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("local adoption should be recognized");

        assert_eq!(item_count(&live_path), 1);
    }

    #[test]
    fn rejected_sql_does_not_block_following_writes() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let insert = encoded_write(&transaction(
            "INSERT INTO missing(value) VALUES (?1)",
            vec![Value::Integer(1)],
        ));
        let create = encoded_write(&transaction(
            "CREATE TABLE items(value INTEGER NOT NULL)",
            Vec::new(),
        ));
        let expected_checkpoint = checkpoint(2, 2);
        let event = blocks_processed(
            expected_checkpoint.clone(),
            vec![
                ChannelUpdateTx::Inscription(inscription(&insert.payload, 1)),
                ChannelUpdateTx::Inscription(inscription(&create.payload, 2)),
            ],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("rejected SQL should not halt channel replay");

        let restored = db
            .load_checkpoint()
            .expect("checkpoint should load")
            .expect("checkpoint should exist");

        assert_eq!(restored.lib, expected_checkpoint.lib);
        assert_eq!(restored.lib_slot, expected_checkpoint.lib_slot);
        assert!(table_exists(&live_path, "items"));

        drop(db);
        let db = Databases::open(dir.path()).expect("databases should reopen");

        assert_eq!(
            db.rejected_write_count().expect("rejections should load"),
            1
        );
    }

    #[test]
    fn malformed_payload_does_not_block_following_writes() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let mut malformed = encoded_write(&transaction(
            "CREATE TABLE discarded(value INTEGER)",
            Vec::new(),
        ))
        .payload;
        malformed.pop();

        let create = encoded_write(&transaction(
            "CREATE TABLE items(value INTEGER NOT NULL)",
            Vec::new(),
        ));
        let expected_checkpoint = checkpoint(2, 2);
        let event = blocks_processed(
            expected_checkpoint.clone(),
            vec![
                ChannelUpdateTx::Inscription(inscription(&malformed, 1)),
                ChannelUpdateTx::Inscription(inscription(&create.payload, 2)),
            ],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("malformed payload should not halt channel replay");

        let restored = db
            .load_checkpoint()
            .expect("checkpoint should load")
            .expect("checkpoint should exist");

        assert_eq!(restored.lib, expected_checkpoint.lib);
        assert_eq!(restored.lib_slot, expected_checkpoint.lib_slot);
        assert!(table_exists(&live_path, "items"));
        assert_eq!(
            db.rejected_write_count().expect("rejections should load"),
            1
        );
    }

    #[test]
    fn reused_transaction_id_does_not_block_following_writes() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let first = encoded_write(&transaction(
            "CREATE TABLE first_write(value INTEGER)",
            Vec::new(),
        ));
        let conflicting = ChannelInscription {
            tx_id: first.tx_id,
            transaction: transaction("CREATE TABLE conflicting_write(value INTEGER)", Vec::new()),
        }
        .encode()
        .expect("conflicting write should encode");
        let following = encoded_write(&transaction(
            "CREATE TABLE following_write(value INTEGER)",
            Vec::new(),
        ));
        let expected_checkpoint = checkpoint(2, 2);
        let event = blocks_processed(
            expected_checkpoint.clone(),
            vec![
                ChannelUpdateTx::Inscription(inscription(&first.payload, 1)),
                ChannelUpdateTx::Inscription(inscription(&conflicting, 2)),
                ChannelUpdateTx::Inscription(inscription(&following.payload, 3)),
            ],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("reused transaction ID should not halt channel replay");

        let restored = db
            .load_checkpoint()
            .expect("checkpoint should load")
            .expect("checkpoint should exist");

        assert_eq!(restored.lib, expected_checkpoint.lib);
        assert_eq!(restored.lib_slot, expected_checkpoint.lib_slot);
        assert!(table_exists(&live_path, "first_write"));
        assert!(!table_exists(&live_path, "conflicting_write"));
        assert!(table_exists(&live_path, "following_write"));
        assert_eq!(
            db.rejected_write_count().expect("rejections should load"),
            1
        );
    }

    #[test]
    fn unsupported_protocol_does_not_block_following_writes() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let mut unsupported = encoded_write(&transaction(
            "CREATE TABLE discarded(value INTEGER)",
            Vec::new(),
        ))
        .payload;
        let version_offset = PAYLOAD_MARKER.len();
        unsupported[version_offset..version_offset + size_of::<u16>()]
            .copy_from_slice(&2u16.to_le_bytes());

        let following = encoded_write(&transaction(
            "CREATE TABLE following_write(value INTEGER)",
            Vec::new(),
        ));
        let expected_checkpoint = checkpoint(2, 2);
        let event = blocks_processed(
            expected_checkpoint.clone(),
            vec![
                ChannelUpdateTx::Inscription(inscription(&unsupported, 1)),
                ChannelUpdateTx::Inscription(inscription(&following.payload, 2)),
            ],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("unsupported protocol should not halt channel replay");

        let restored = db
            .load_checkpoint()
            .expect("checkpoint should load")
            .expect("checkpoint should exist");

        assert_eq!(restored.lib, expected_checkpoint.lib);
        assert_eq!(restored.lib_slot, expected_checkpoint.lib_slot);
        assert!(table_exists(&live_path, "following_write"));
        assert_eq!(
            db.rejected_write_count().expect("rejections should load"),
            1
        );
    }

    #[test]
    fn orphaned_local_write_prevents_adopted_apply_until_rebuild() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_encoded = EncodedWrite::new(&local).expect("local write should encode");

        db.commit_local_write(&local, &local_encoded)
            .expect("local write should commit");
        db.mark_publish_complete(local_encoded.tx_id)
            .expect("local publish should be complete");

        let adopted = encoded_write(&transaction(
            "CREATE TABLE adopted_too_early(value INTEGER)",
            Vec::new(),
        ));
        let event = blocks_processed(
            checkpoint(2, 2),
            vec![ChannelUpdateTx::Inscription(inscription(
                &adopted.payload,
                2,
            ))],
            vec![ChannelUpdateTx::Inscription(inscription(
                &local_encoded.payload,
                1,
            ))],
            Vec::new(),
        );

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
        }));

        assert!(
            result.is_err(),
            "orphan recovery should reach its placeholder"
        );
        assert!(!table_exists(&live_path, "adopted_too_early"));
    }
}
