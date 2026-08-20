//! Outbound write path for transactions initiated by the local application.
//!
//! Transactions arriving through channel history are handled by the applier.

use lb_zone_sdk::node_types::Inscription;

use crate::{
    db::Databases,
    error::Error,
    protocol::{EncodedWrite, IdempotencyKey, Transaction, TxId},
};

pub fn commit(
    db: &mut Databases,
    transaction: &Transaction,
    idempotency_key: &IdempotencyKey,
    writer_id: &[u8; 32],
) -> Result<TxId, Error> {
    let encoded = EncodedWrite::new(transaction, idempotency_key, writer_id)?;

    if let Some(tx_id) = db.existing_write(idempotency_key, &encoded.transaction_digest)? {
        return Ok(tx_id);
    }

    if encoded.payload.len() > Inscription::MAX {
        return Err(Error::InscriptionTooLarge);
    }

    db.commit_local_write(transaction, idempotency_key, &encoded)
}
