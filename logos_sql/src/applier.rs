//! Inbound path for applying channel history to participant-local state.

use lb_zone_sdk::{
    node_types::ChannelId,
    sequencer::{
        ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, channel_inscriptions,
    },
};

use crate::{db::Databases, error::Error, protocol};

const TARGET: &str = lb_log_targets::logos_sql::APPLIER;

/// Handles one sequencer event.
///
/// Checkpoints advance over traffic that does not change `λSQL` state.
/// Encountering an adopted, orphaned, or finalized `λSQL` transaction
/// currently stops at the unfinished replay path.
///
/// # Errors
///
/// Returns an error if the checkpoint cannot be persisted or a channel payload
/// cannot be decoded.
///
/// # Panics
///
/// Panics when a `λSQL` transaction reaches an unfinished apply path.
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

            // TODO: Apply finalized transactions to LIB and reconcile LIVE with
            // adopted and orphaned channel transactions before persisting the
            // checkpoint.
            if update_contains_logos_sql(channel_update, channel_id) {
                todo!("reconcile adopted and orphaned \u{3bb}SQL transactions");
            }

            if let Some(payload) = find_logos_sql_payload(finalized) {
                let decoded = protocol::ChannelInscription::decode(payload)?;

                tracing::debug!(
                    target: TARGET,
                    tx_id = ?decoded.tx_id,
                    statements = decoded.transaction.statements().len(),
                    "\u{3bb}SQL transaction requires replay"
                );

                todo!("apply finalized \u{3bb}SQL transactions");
            }

            db.persist_checkpoint(checkpoint)?;
        }
        Event::Ready => {
            tracing::info!(target: TARGET, "sequencer ready");
        }
        Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
    }

    Ok(())
}

fn update_contains_logos_sql(channel_update: &ChannelUpdate, channel_id: ChannelId) -> bool {
    channel_update
        .orphaned
        .iter()
        .chain(&channel_update.adopted)
        .any(|transaction| transaction_contains_logos_sql(transaction, channel_id))
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

fn find_logos_sql_payload(finalized: &[FinalizedTx]) -> Option<&[u8]> {
    finalized
        .iter()
        .flat_map(|transaction| &transaction.ops)
        .find_map(|operation| {
            let FinalizedOp::Inscription(info) = operation else {
                return None;
            };

            let payload = info.payload.as_ref();

            protocol::is_logos_sql_payload(payload).then_some(payload)
        })
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
    use tempfile::TempDir;

    use super::{on_event, update_contains_logos_sql};
    use crate::{
        db::Databases,
        protocol::{EncodedWrite, Statement, Transaction, Value},
    };

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

    fn inscription(payload: &[u8]) -> InscriptionInfo {
        InscriptionInfo {
            tx_hash: TxHash::from([1; 32]),
            parent_msg: MsgId::root(),
            this_msg: MsgId::root(),
            payload: payload
                .to_vec()
                .try_into()
                .expect("test payload should fit an inscription"),
        }
    }

    fn blocks_processed(checkpoint: SequencerCheckpoint, payload: &[u8]) -> Event {
        Event::BlocksProcessed {
            checkpoint,
            channel_update: ChannelUpdate {
                orphaned: Vec::new(),
                adopted: Vec::new(),
            },
            finalized: vec![FinalizedTx {
                tx_hash: TxHash::from([1; 32]),
                l1_slot: Slot::from(2),
                ops: vec![FinalizedOp::Inscription(inscription(payload))],
            }],
        }
    }

    fn orphaned(checkpoint: SequencerCheckpoint, payload: &[u8]) -> Event {
        Event::BlocksProcessed {
            checkpoint,
            channel_update: ChannelUpdate {
                orphaned: vec![ChannelUpdateTx::Inscription(inscription(payload))],
                adopted: Vec::new(),
            },
            finalized: Vec::new(),
        }
    }

    fn encoded_write() -> Vec<u8> {
        let transaction = Transaction::new(vec![
            Statement::new(
                "INSERT INTO items(value) VALUES (?1)".to_owned(),
                vec![Value::Integer(1)],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        EncodedWrite::new(&transaction)
            .expect("payload should encode")
            .payload
    }

    #[test]
    #[should_panic(expected = "apply finalized \u{3bb}SQL transactions")]
    fn finalized_logos_sql_payload_reaches_replay_placeholder() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let path = dir.path().join("zone");
        let first = checkpoint(1, 1);
        let second = checkpoint(2, 2);
        let mut db = Databases::open(&path).expect("databases should open");

        db.persist_checkpoint(&first)
            .expect("initial checkpoint should commit");

        let payload = encoded_write();

        let event = blocks_processed(second, &payload);
        on_event(&mut db, &event, ChannelId::from([9; 32]))
            .expect("event should reach replay placeholder");
    }

    #[test]
    #[should_panic(expected = "reconcile adopted and orphaned \u{3bb}SQL transactions")]
    fn orphaned_logos_sql_payload_reaches_reconciliation_placeholder() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let path = dir.path().join("zone");
        let mut db = Databases::open(&path).expect("databases should open");
        let payload = encoded_write();
        let event = orphaned(checkpoint(2, 2), &payload);

        on_event(&mut db, &event, ChannelId::from([9; 32]))
            .expect("event should reach reconciliation placeholder");
    }

    #[test]
    fn adopted_logos_sql_payload_is_detected() {
        let payload = encoded_write();
        let channel_update = ChannelUpdate {
            orphaned: Vec::new(),
            adopted: vec![ChannelUpdateTx::Inscription(inscription(&payload))],
        };

        assert!(update_contains_logos_sql(
            &channel_update,
            ChannelId::from([9; 32])
        ));
    }
}
