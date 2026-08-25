//! Inbound path for applying channel history to participant-local state.

use lb_zone_sdk::{
    node_types::ChannelId,
    sequencer::{
        ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, InscriptionInfo,
        SequencerCheckpoint, channel_inscriptions,
    },
};

use crate::{
    db::{Databases, PendingPublish, SuffixWrite},
    error::Error,
    protocol::{self, ChannelInscription, TxId},
};

const TARGET: &str = lb_log_targets::logos_sql::APPLIER;

/// Selects which replicated database receives a channel write.
#[derive(Clone, Copy, Debug)]
enum ApplyTarget {
    /// Apply only to the current live state.
    Live,
    /// Apply only to finalized state.
    Lib,
    /// Apply to finalized and live state.
    LibAndLive,
}

/// Why this event requires `LIVE.db` to be rebuilt.
enum RebuildCause {
    /// The pending local write executed against live history that this event
    /// changes. Keeping its effect would leave `LIVE.db` in the wrong order or
    /// based on an abandoned branch. Logos SQL does not rerun the application
    /// transaction automatically, so the rebuild removes its effect and
    /// reports it as displaced. This also resumes a rebuild interrupted after
    /// the displacement was recorded.
    PendingWriteInvalidated(PendingPublish),
    /// Canonical channel history changed and `LIVE.db` must drop orphaned SQL.
    ChannelFork,
}

/// Selects how one block event is reflected in `LIVE.db`.
enum ApplicationPlan {
    /// Apply the finalized and adopted changes directly.
    ApplyChanges,
    /// Reconstruct live state because existing effects must be reordered or
    /// removed.
    Rebuild(RebuildCause),
}

impl ApplyTarget {
    fn apply(self, db: &mut Databases, write: &ChannelInscription) -> Result<(), Error> {
        match self {
            Self::Live => db.apply_adopted_write(write),
            Self::Lib => db.apply_finalized_write_to_lib(write),
            Self::LibAndLive => db.apply_finalized_write(write),
        }
    }
}

/// Logos SQL inscriptions carried by one processed-block event.
///
/// Normalizing the `ZoneSDK` event once keeps planning and execution on the
/// same set of writes, regardless of their underlying transaction shape.
struct SqlChanges {
    adopted: Vec<InscriptionInfo>,
    orphaned: Vec<InscriptionInfo>,
    finalized: Vec<InscriptionInfo>,
}

/// Handles one sequencer event.
///
/// Every `BlocksProcessed` checkpoint is persisted, including events that
/// contain no `λSQL` writes, so restart resumes from the latest processed
/// block.
/// Finalized writes apply to both finalized and live state, while newly
/// adopted writes apply only to live state. A branch change reconstructs live
/// state from finalized history and the retained canonical suffix.
///
/// # Errors
///
/// Returns an error if SQL cannot be applied, a channel payload cannot be
/// decoded, or the checkpoint cannot be persisted.
pub fn on_event(db: &mut Databases, event: &Event, channel_id: ChannelId) -> Result<(), Error> {
    match event {
        Event::BlocksProcessed {
            checkpoint,
            channel_update,
            finalized,
        } => process_blocks(db, checkpoint, channel_update, finalized, channel_id)?,
        Event::Ready => {
            tracing::info!(target: TARGET, "sequencer ready");
        }
        Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
    }

    Ok(())
}

/// Applies one block event before advancing its checkpoint.
///
/// If any state change fails, the checkpoint remains behind and `ZoneSDK`
/// delivers the same channel position again after retry or restart.
fn process_blocks(
    db: &mut Databases,
    checkpoint: &SequencerCheckpoint,
    channel_update: &ChannelUpdate,
    finalized: &[FinalizedTx],
    channel_id: ChannelId,
) -> Result<(), Error> {
    let changes = SqlChanges::from_block(channel_update, finalized, channel_id);

    tracing::debug!(
        target: TARGET,
        adopted_writes = changes.adopted.len(),
        orphaned_writes = changes.orphaned.len(),
        finalized_writes = changes.finalized.len(),
        "blocks processed"
    );

    let plan = changes.application_plan(db)?;

    match plan {
        ApplicationPlan::ApplyChanges => changes.apply(db)?,
        ApplicationPlan::Rebuild(cause) => changes.rebuild(db, &cause)?,
    }

    db.persist_checkpoint(checkpoint)
}

impl SqlChanges {
    /// Extracts only Logos SQL inscriptions from the three channel-history
    /// sets.
    fn from_block(
        channel_update: &ChannelUpdate,
        finalized: &[FinalizedTx],
        channel_id: ChannelId,
    ) -> Self {
        let finalized = finalized
            .iter()
            .flat_map(|transaction| &transaction.ops)
            .filter_map(|operation| match operation {
                FinalizedOp::Inscription(inscription) => Some(inscription.clone()),
                _ => None,
            })
            .filter(is_logos_sql_inscription)
            .collect();

        Self {
            adopted: Self::collect_channel_inscriptions(&channel_update.adopted, channel_id),
            orphaned: Self::collect_channel_inscriptions(&channel_update.orphaned, channel_id),
            finalized,
        }
    }

    fn collect_channel_inscriptions(
        transactions: &[ChannelUpdateTx],
        channel_id: ChannelId,
    ) -> Vec<InscriptionInfo> {
        transactions
            .iter()
            .flat_map(|transaction| Self::transaction_inscriptions(transaction, channel_id))
            .filter(is_logos_sql_inscription)
            .collect()
    }

    fn transaction_inscriptions(
        transaction: &ChannelUpdateTx,
        channel_id: ChannelId,
    ) -> Vec<InscriptionInfo> {
        if let Some(inscription) = transaction.inscription() {
            return vec![inscription.clone()];
        }

        let ChannelUpdateTx::Custom(transaction) = transaction else {
            return Vec::new();
        };

        channel_inscriptions(transaction, channel_id)
    }

    /// Chooses between direct application and reconstructing `LIVE.db`.
    ///
    /// Planning performs no mutations. A rebuild is required whenever the
    /// optimistic local effect must move or disappear, or canonical history
    /// removes an effect that is already present in `LIVE.db`.
    fn application_plan(&self, db: &Databases) -> Result<ApplicationPlan, Error> {
        if let Some(pending) = db.pending_publish()? {
            // The rebuild records the displacement first, then replaces
            // `LIVE.db`, which removes its pending-write record. If both records
            // still exist, the process stopped between those two steps and the
            // rebuild must resume.
            if db.is_write_displaced(pending.tx_id)? {
                return Ok(ApplicationPlan::Rebuild(
                    RebuildCause::PendingWriteInvalidated(pending),
                ));
            }

            if self.changes_pending_base(db)? {
                return Ok(ApplicationPlan::Rebuild(
                    RebuildCause::PendingWriteInvalidated(pending),
                ));
            }
        }

        if !self.orphaned.is_empty() {
            return Ok(ApplicationPlan::Rebuild(RebuildCause::ChannelFork));
        }

        Ok(ApplicationPlan::ApplyChanges)
    }

    /// Reports whether this event changed the history below a pending local
    /// write.
    ///
    /// Adoptions and orphans always change that base. Finalizing a write
    /// already retained in the live suffix only moves the finalized
    /// boundary and does not invalidate the pending write's execution
    /// order.
    fn changes_pending_base(&self, db: &Databases) -> Result<bool, Error> {
        if !self.adopted.is_empty() || !self.orphaned.is_empty() {
            return Ok(true);
        }

        for inscription in &self.finalized {
            if !db.live_suffix_contains(inscription.this_msg)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Applies an event whose existing `LIVE.db` effects remain correctly
    /// ordered.
    fn apply(&self, db: &mut Databases) -> Result<(), Error> {
        Self::apply_inscriptions(db, &self.finalized, ApplyTarget::LibAndLive)?;
        Self::apply_inscriptions(db, &self.adopted, ApplyTarget::Live)?;
        self.apply_history_delta(db)
    }

    /// Records displacement, advances finalized state, and reconstructs LIVE.
    ///
    /// The displacement, when applicable, and suffix are persisted before
    /// replacing `LIVE.db` so an interrupted rebuild can be recognized and
    /// safely repeated.
    fn rebuild(&self, db: &mut Databases, cause: &RebuildCause) -> Result<(), Error> {
        match cause {
            RebuildCause::PendingWriteInvalidated(pending) => {
                db.record_unpublished_displacement(pending)?;
            }
            RebuildCause::ChannelFork => {}
        }

        Self::apply_inscriptions(db, &self.finalized, ApplyTarget::Lib)?;
        self.apply_history_delta(db)?;
        rebuild_live_from_suffix(db)
    }

    fn apply_inscriptions(
        db: &mut Databases,
        inscriptions: &[InscriptionInfo],
        target: ApplyTarget,
    ) -> Result<(), Error> {
        for inscription in inscriptions {
            apply_inscription(db, inscription, target)?;
        }

        Ok(())
    }

    /// Applies this event to the replayable suffix and local displacement
    /// outcomes.
    fn apply_history_delta(&self, db: &mut Databases) -> Result<(), Error> {
        let finalized = self
            .finalized
            .iter()
            .map(|inscription| inscription.this_msg)
            .collect::<Vec<_>>();
        let orphaned = self
            .orphaned
            .iter()
            .map(|inscription| inscription.this_msg)
            .collect::<Vec<_>>();
        let adopted = self.collect_adopted_suffix(db)?;

        db.apply_history_delta(&finalized, &orphaned, &adopted)
    }

    /// Prepares newly adopted writes for durable replay during a later rebuild.
    fn collect_adopted_suffix(&self, db: &Databases) -> Result<Vec<SuffixWrite>, Error> {
        let mut writes = Vec::new();

        for inscription in &self.adopted {
            let payload = inscription.payload.as_ref();
            let write = match ChannelInscription::decode(payload) {
                Ok(write) => write,
                Err(error) => {
                    handle_write_error(db, inscription, None, error)?;
                    continue;
                }
            };

            writes.push(SuffixWrite {
                this_msg: inscription.this_msg,
                tx_id: write.tx_id,
                payload: payload.to_vec(),
                local: false,
            });
        }

        Ok(writes)
    }
}

/// Reconstructs `LIVE.db` as finalized state followed by the canonical suffix.
///
/// Stored payloads are local durable state: malformed or mismatched records
/// indicate corruption. Deterministically rejected SQL is recorded and skipped
/// exactly as it is during normal channel application.
fn rebuild_live_from_suffix(db: &mut Databases) -> Result<(), Error> {
    let suffix = db.live_suffix()?;
    let mut rebuild = db.begin_live_rebuild()?;

    for retained in suffix {
        let write = ChannelInscription::decode(&retained.payload)
            .map_err(|_| Error::InvalidLocalState("stored live suffix payload is malformed"))?;

        if write.tx_id != retained.tx_id {
            return Err(Error::InvalidLocalState(
                "stored live suffix transaction id does not match its payload",
            ));
        }

        match rebuild.apply_write(&write) {
            Ok(()) => {}
            Err(error) if is_rejected_write(&error) => {
                db.record_rejected_write(retained.this_msg, Some(write.tx_id), &error.to_string())?;
            }
            Err(error) => return Err(error),
        }
    }

    db.finish_live_rebuild(rebuild)?;

    tracing::info!(
        target: TARGET,
        "live database rebuilt after channel branch change"
    );

    Ok(())
}

/// Decodes and applies one inscription, recording deterministic rejections.
fn apply_inscription(
    db: &mut Databases,
    inscription: &InscriptionInfo,
    target: ApplyTarget,
) -> Result<(), Error> {
    let payload = inscription.payload.as_ref();

    let write = match ChannelInscription::decode(payload) {
        Ok(write) => write,
        Err(error) => return handle_write_error(db, inscription, None, error),
    };

    if let Err(error) = target.apply(db, &write) {
        return handle_write_error(db, inscription, Some(write.tx_id), error);
    }

    tracing::debug!(
        target: TARGET,
        tx_id = ?write.tx_id,
        statements = write.transaction.statements().len(),
        ?target,
        "channel write processed"
    );

    Ok(())
}

/// Records errors that every replica must reject and propagates local failures.
///
/// Propagated failures leave the checkpoint behind so the event is retried;
/// recording an input rejection allows processing to continue consistently.
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

fn is_logos_sql_inscription(inscription: &InscriptionInfo) -> bool {
    protocol::is_logos_sql_payload(inscription.payload.as_ref())
}

#[cfg(test)]
mod tests {
    use lb_zone_sdk::{
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
        db::{Databases, SuffixWrite},
        protocol::{
            CapturedFunctionCalls, ChannelInscription, EncodedWrite, PAYLOAD_MARKER, Statement,
            Transaction, TxId,
        },
    };

    const CHANNEL_ID: [u8; 32] = [9; 32];

    fn checkpoint(byte: u8, slot: u64) -> SequencerCheckpoint {
        SequencerCheckpoint {
            last_msg_id: MsgId::root(),
            pending_txs: Vec::new(),
            lib: HeaderId::from([byte; 32]),
            lib_slot: Slot::from(slot),
            channel_notes: Vec::new(),
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
        EncodedWrite::new(
            TxId::generate(),
            transaction,
            CapturedFunctionCalls::empty(),
        )
        .expect("payload should encode")
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

    fn journal_mode(path: &std::path::Path) -> String {
        Databases::open_reader(path)
            .expect("database should open for reading")
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode should be readable")
    }

    fn text_values(path: &std::path::Path, table: &str) -> Vec<String> {
        let connection = Databases::open_reader(path).expect("database should open for reading");
        let mut statement = connection
            .prepare(&format!("SELECT value FROM {table} ORDER BY value"))
            .expect("query should prepare");

        statement
            .query_map([], |row| row.get(0))
            .expect("query should execute")
            .collect::<Result<_, _>>()
            .expect("values should be readable")
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
        let setup_tx_id = db
            .commit_local_write(TxId::generate(), &setup)
            .expect("setup write should commit");
        let setup_pending = db
            .pending_publish()
            .expect("setup write should load")
            .expect("setup write should be pending");
        assert_eq!(setup_pending.tx_id, setup_tx_id);
        db.complete_publish(&checkpoint(1, 1), MsgId::from([1; 32]), &setup_pending)
            .expect("setup publish should be complete");

        let insert = transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Integer(1)],
        );
        let insert_tx_id = db
            .commit_local_write(TxId::generate(), &insert)
            .expect("insert write should commit");
        let insert_pending = db
            .pending_publish()
            .expect("pending write should load")
            .expect("pending write should exist");
        assert_eq!(insert_pending.tx_id, insert_tx_id);
        let insert_payload = insert_pending.payload.clone();
        db.complete_publish(&checkpoint(1, 1), MsgId::from([2; 32]), &insert_pending)
            .expect("insert publish should be complete");

        let event = blocks_processed(
            checkpoint(2, 2),
            vec![ChannelUpdateTx::Inscription(inscription(
                &insert_payload,
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
    fn finalizing_an_earlier_local_write_preserves_the_next_pending_write() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();
        let lib_path = db.lib_path().to_owned();

        let create = transaction("CREATE TABLE items(value INTEGER NOT NULL)", Vec::new());
        db.commit_local_write(TxId::generate(), &create)
            .expect("schema write should commit");
        let create_pending = db
            .pending_publish()
            .expect("schema write should load")
            .expect("schema write should be pending");
        let create_payload = create_pending.payload.clone();
        let create_msg = MsgId::from([1; 32]);

        db.complete_publish(&checkpoint(1, 1), create_msg, &create_pending)
            .expect("schema publish should be complete");

        let insert = transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Integer(1)],
        );
        let insert_tx_id = db
            .commit_local_write(TxId::generate(), &insert)
            .expect("next write should commit");

        let finalization = blocks_processed(
            checkpoint(2, 2),
            Vec::new(),
            Vec::new(),
            vec![finalized(&create_payload, 1)],
        );

        on_event(&mut db, &finalization, ChannelId::from(CHANNEL_ID))
            .expect("earlier write should finalize");

        assert_eq!(item_count(&live_path), 1);
        assert_eq!(item_count(&lib_path), 0);
        assert_eq!(
            db.pending_publish()
                .expect("pending write should load")
                .expect("next write should remain pending")
                .tx_id,
            insert_tx_id
        );
        assert!(
            db.displaced_writes()
                .expect("displaced writes should load")
                .is_empty()
        );
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
            captured_function_calls: CapturedFunctionCalls::empty(),
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
            .copy_from_slice(&3u16.to_le_bytes());

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
    fn orphaned_local_write_rebuilds_live_and_reports_displacement() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_tx_id = db
            .commit_local_write(TxId::generate(), &local)
            .expect("local write should commit");
        let local_pending = db
            .pending_publish()
            .expect("pending write should load")
            .expect("pending write should exist");
        let local_payload = local_pending.payload.clone();
        db.complete_publish(&checkpoint(1, 1), MsgId::from([1; 32]), &local_pending)
            .expect("local publish should be complete");

        let reader = Databases::open_reader(&live_path).expect("live reader should open");
        reader
            .execute_batch("BEGIN")
            .expect("reader snapshot should begin");
        let local_visible_before_rebuild: bool = reader
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'local_write'
                )",
                [],
                |row| row.get(0),
            )
            .expect("reader snapshot should be established");

        assert!(local_visible_before_rebuild);

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
            vec![ChannelUpdateTx::Inscription(inscription(&local_payload, 1))],
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("orphaned write should rebuild live state");

        assert!(!table_exists(&live_path, "local_write"));
        assert!(table_exists(&live_path, "adopted_too_early"));
        assert_eq!(journal_mode(&live_path), "wal");
        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![local_tx_id]
        );

        let local_still_visible: bool = reader
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'local_write'
                )",
                [],
                |row| row.get(0),
            )
            .expect("active reader should retain its snapshot");

        assert!(local_still_visible);

        reader
            .execute_batch("COMMIT")
            .expect("reader snapshot should end");

        let adopted_visible: bool = reader
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'adopted_too_early'
                )",
                [],
                |row| row.get(0),
            )
            .expect("reader should observe rebuilt state after its snapshot");

        assert!(adopted_visible);

        let restore_original = blocks_processed(
            checkpoint(3, 3),
            vec![ChannelUpdateTx::Inscription(inscription(&local_payload, 1))],
            vec![ChannelUpdateTx::Inscription(inscription(
                &adopted.payload,
                2,
            ))],
            Vec::new(),
        );

        on_event(&mut db, &restore_original, ChannelId::from(CHANNEL_ID))
            .expect("restored branch should rebuild live state");

        assert!(table_exists(&live_path, "local_write"));
        assert!(!table_exists(&live_path, "adopted_too_early"));
        assert!(
            db.displaced_writes()
                .expect("displaced writes should load")
                .is_empty(),
            "restoring the original channel position clears displacement"
        );
    }

    #[test]
    fn branch_change_preserves_the_unchanged_live_suffix() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let create = encoded_write(&transaction(
            "CREATE TABLE items(value TEXT PRIMARY KEY)",
            Vec::new(),
        ));
        let old_branch = encoded_write(&transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Text("old".to_owned())],
        ));

        let initial = blocks_processed(
            checkpoint(1, 1),
            vec![
                ChannelUpdateTx::Inscription(inscription(&create.payload, 1)),
                ChannelUpdateTx::Inscription(inscription(&old_branch.payload, 2)),
            ],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &initial, ChannelId::from(CHANNEL_ID))
            .expect("initial suffix should apply");

        drop(db);
        let mut db = Databases::open(dir.path()).expect("databases should reopen");

        let replacement = encoded_write(&transaction(
            "INSERT INTO items(value) VALUES (?1)",
            vec![Value::Text("new".to_owned())],
        ));
        let branch_change = blocks_processed(
            checkpoint(2, 2),
            vec![ChannelUpdateTx::Inscription(inscription(
                &replacement.payload,
                3,
            ))],
            vec![ChannelUpdateTx::Inscription(inscription(
                &old_branch.payload,
                2,
            ))],
            Vec::new(),
        );

        on_event(&mut db, &branch_change, ChannelId::from(CHANNEL_ID))
            .expect("replacement suffix should rebuild");
        on_event(&mut db, &branch_change, ChannelId::from(CHANNEL_ID))
            .expect("replayed branch update should be idempotent");

        assert_eq!(text_values(&live_path, "items"), vec!["new"]);
        assert!(
            db.displaced_writes()
                .expect("displaced writes should load")
                .is_empty(),
            "foreign orphaned writes are not application outcomes"
        );
    }

    #[test]
    fn foreign_adoption_displaces_an_unpublished_local_write() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_tx_id = db
            .commit_local_write(TxId::generate(), &local)
            .expect("local write should commit");

        let foreign = encoded_write(&transaction(
            "CREATE TABLE foreign_write(value INTEGER)",
            Vec::new(),
        ));
        let event = blocks_processed(
            checkpoint(2, 2),
            vec![ChannelUpdateTx::Inscription(inscription(
                &foreign.payload,
                2,
            ))],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("foreign adoption should rebuild over unpublished local state");

        assert!(!table_exists(&live_path, "local_write"));
        assert!(table_exists(&live_path, "foreign_write"));
        assert!(
            db.pending_publish()
                .expect("pending publication should load")
                .is_none()
        );
        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![local_tx_id]
        );
    }

    #[test]
    fn rollback_displaces_an_unpublished_write_based_on_removed_state() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let previous = encoded_write(&transaction(
            "CREATE TABLE previous_write(value INTEGER)",
            Vec::new(),
        ));
        let initial_event = blocks_processed(
            checkpoint(1, 1),
            vec![ChannelUpdateTx::Inscription(inscription(
                &previous.payload,
                1,
            ))],
            Vec::new(),
            Vec::new(),
        );
        on_event(&mut db, &initial_event, ChannelId::from(CHANNEL_ID))
            .expect("initial channel state should apply");

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_tx_id = db
            .commit_local_write(TxId::generate(), &local)
            .expect("local write should commit");

        let rollback = blocks_processed(
            checkpoint(2, 2),
            Vec::new(),
            vec![ChannelUpdateTx::Inscription(inscription(
                &previous.payload,
                1,
            ))],
            Vec::new(),
        );
        on_event(&mut db, &rollback, ChannelId::from(CHANNEL_ID))
            .expect("rollback should rebuild without unpublished local state");

        assert!(!table_exists(&live_path, "previous_write"));
        assert!(!table_exists(&live_path, "local_write"));
        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![local_tx_id]
        );
    }

    #[test]
    fn replay_finishes_a_rebuild_after_the_suffix_commit() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_tx_id = db
            .commit_local_write(TxId::generate(), &local)
            .expect("local write should commit");
        let pending = db
            .pending_publish()
            .expect("pending write should load")
            .expect("pending write should exist");

        let foreign = encoded_write(&transaction(
            "CREATE TABLE foreign_write(value INTEGER)",
            Vec::new(),
        ));
        let foreign_inscription = inscription(&foreign.payload, 2);

        // Simulate a crash after the control-state transaction commits but
        // before the replacement LIVE database is installed.
        db.record_unpublished_displacement(&pending)
            .expect("displacement should be recorded");
        db.apply_history_delta(
            &[],
            &[],
            &[SuffixWrite {
                this_msg: foreign_inscription.this_msg,
                tx_id: ChannelInscription::decode(&foreign.payload)
                    .expect("payload should decode")
                    .tx_id,
                payload: foreign.payload.clone(),
                local: false,
            }],
        )
        .expect("suffix update should commit");
        drop(db);

        let mut db = Databases::open(dir.path()).expect("databases should reopen");
        let replayed_event = blocks_processed(
            checkpoint(2, 2),
            vec![ChannelUpdateTx::Inscription(foreign_inscription)],
            Vec::new(),
            Vec::new(),
        );

        on_event(&mut db, &replayed_event, ChannelId::from(CHANNEL_ID))
            .expect("replayed event should finish the rebuild");

        assert!(!table_exists(&live_path, "local_write"));
        assert!(table_exists(&live_path, "foreign_write"));
        assert!(
            db.pending_publish()
                .expect("pending publication should load")
                .is_none()
        );
        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![local_tx_id]
        );
    }

    #[test]
    fn finalized_backfill_finishes_a_rebuild_after_the_suffix_commit() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_tx_id = db
            .commit_local_write(TxId::generate(), &local)
            .expect("local write should commit");
        let pending = db
            .pending_publish()
            .expect("pending write should load")
            .expect("pending write should exist");

        let foreign = encoded_write(&transaction(
            "CREATE TABLE foreign_write(value INTEGER)",
            Vec::new(),
        ));
        let foreign_inscription = inscription(&foreign.payload, 2);

        // Simulate a crash after the control-state transaction commits but
        // before the replacement LIVE database is installed.
        db.record_unpublished_displacement(&pending)
            .expect("displacement should be recorded");
        db.apply_history_delta(
            &[],
            &[],
            &[SuffixWrite {
                this_msg: foreign_inscription.this_msg,
                tx_id: ChannelInscription::decode(&foreign.payload)
                    .expect("payload should decode")
                    .tx_id,
                payload: foreign.payload.clone(),
                local: false,
            }],
        )
        .expect("suffix update should commit");
        drop(db);

        let mut db = Databases::open(dir.path()).expect("databases should reopen");
        let backfilled_event = blocks_processed(
            checkpoint(2, 2),
            Vec::new(),
            Vec::new(),
            vec![FinalizedTx {
                tx_hash: foreign_inscription.tx_hash,
                l1_slot: Slot::from(2),
                ops: vec![FinalizedOp::Inscription(foreign_inscription)],
            }],
        );

        on_event(&mut db, &backfilled_event, ChannelId::from(CHANNEL_ID))
            .expect("finalized backfill should finish the rebuild");

        assert!(!table_exists(&live_path, "local_write"));
        assert!(table_exists(&live_path, "foreign_write"));
        assert!(
            db.pending_publish()
                .expect("pending publication should load")
                .is_none()
        );
        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![local_tx_id]
        );
    }

    #[test]
    fn newly_discovered_finalized_write_displaces_an_unpublished_local_write() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let live_path = db.live_path().to_owned();

        let local = transaction("CREATE TABLE local_write(value INTEGER)", Vec::new());
        let local_tx_id = db
            .commit_local_write(TxId::generate(), &local)
            .expect("local write should commit");

        let foreign = encoded_write(&transaction(
            "CREATE TABLE finalized_write(value INTEGER)",
            Vec::new(),
        ));
        let event = blocks_processed(
            checkpoint(2, 2),
            Vec::new(),
            Vec::new(),
            vec![finalized(&foreign.payload, 2)],
        );

        on_event(&mut db, &event, ChannelId::from(CHANNEL_ID))
            .expect("new finalized history should replace unpublished local state");

        assert!(!table_exists(&live_path, "local_write"));
        assert!(table_exists(&live_path, "finalized_write"));
        assert!(table_exists(db.lib_path(), "finalized_write"));
        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![local_tx_id]
        );
    }
}
