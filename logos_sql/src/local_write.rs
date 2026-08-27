//! Outbound write path for transactions initiated by the local application.
//!
//! Transactions arriving through channel history are handled by the applier.

use crate::{
    db::Databases,
    error::Error,
    protocol::{EncodedWrite, Transaction, TxId},
};

pub fn commit(db: &mut Databases, transaction: &Transaction) -> Result<TxId, Error> {
    let encoded = EncodedWrite::new(transaction)?;

    db.commit_local_write(transaction, &encoded)
}
