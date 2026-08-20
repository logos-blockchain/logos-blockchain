//! Outbound write path for transactions initiated by the local application.
//!
//! Transactions arriving through channel history are handled by the applier.

use lb_zone_sdk::node_types::Inscription;

use crate::{
    db::Databases,
    error::Error,
    protocol::{EncodedWrite, Transaction, TxId},
};

pub fn commit(db: &mut Databases, transaction: &Transaction) -> Result<TxId, Error> {
    let encoded = EncodedWrite::new(transaction)?;

    if encoded.payload.len() > Inscription::MAX {
        return Err(Error::InscriptionTooLarge);
    }

    db.commit_local_write(transaction, &encoded)
}
