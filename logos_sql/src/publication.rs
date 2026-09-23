//! Packs the oldest queued writes into one bounded channel inscription.

use lb_binary_codec::canonical::BinaryEncode as _;

use crate::{
    db::PendingPublish,
    error::Error,
    protocol::{ChannelBatch, ChannelWrite, MAX_BODY_BYTES},
};

/// The exact queue prefix owned by one SDK publication and checkpoint.
pub struct Publication {
    pub writes: Vec<PendingPublish>,
    pub payload: Vec<u8>,
}

impl Publication {
    pub fn prepare(mut pending: Vec<PendingPublish>) -> Result<Option<Self>, Error> {
        if pending.is_empty() {
            return Ok(None);
        }

        let mut writes = Vec::new();
        let mut body_len = size_of::<u16>();

        for queued in &pending {
            let write = ChannelWrite::decode(&queued.payload)
                .map_err(|_| Error::InvalidLocalState("queued write cannot be decoded"))?;

            if write.tx_id != queued.tx_id {
                return Err(Error::InvalidLocalState(
                    "queued write identity does not match",
                ));
            }

            body_len += write.encoded_length();

            if body_len > MAX_BODY_BYTES {
                break;
            }

            writes.push(write);
        }

        // Compression size cannot be predicted from the individual writes.
        // Halving bounds the number of attempts; remaining writes stay queued.
        loop {
            let batch = ChannelBatch::new(writes.clone())?;

            match batch.encode() {
                Ok(payload) => {
                    pending.truncate(writes.len());

                    return Ok(Some(Self {
                        writes: pending,
                        payload,
                    }));
                }
                Err(Error::InscriptionTooLarge) if writes.len() > 1 => {
                    writes.truncate(writes.len() / 2);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Matches a saved SDK publication even if newer writes remain queued.
    pub fn matches_pending(
        payload: &[u8],
        pending: &[PendingPublish],
    ) -> Result<Option<usize>, Error> {
        let Ok(batch) = ChannelBatch::decode(payload) else {
            return Ok(None);
        };
        let writes = batch.into_writes();

        if writes.len() > pending.len() {
            return Ok(None);
        }

        for (write, queued) in writes.iter().zip(pending) {
            let original = ChannelWrite::decode(&queued.payload)
                .map_err(|_| Error::InvalidLocalState("queued write cannot be decoded"))?;

            if write.tx_id != queued.tx_id || write.content_digest() != original.content_digest() {
                return Ok(None);
            }
        }

        Ok(Some(writes.len()))
    }
}

#[cfg(test)]
mod tests {
    use rand::{RngCore as _, SeedableRng as _, rngs::StdRng};
    use rusqlite::types::Value;

    use super::Publication;
    use crate::{
        db::PendingPublish,
        protocol::{
            CapturedFunctionCalls, ChannelBatch, EncodedWrite, Statement, Transaction, TxId,
        },
    };

    fn queued_write(value: Value) -> PendingPublish {
        let transaction = Transaction::new(vec![
            Statement::new("SELECT ?1".to_owned(), vec![value]).unwrap(),
        ])
        .unwrap();
        let write = EncodedWrite::new(
            TxId::generate(),
            &transaction,
            CapturedFunctionCalls::empty(),
        )
        .unwrap();

        PendingPublish {
            tx_id: write.tx_id,
            payload: write.payload,
        }
    }

    #[test]
    fn queued_writes_share_one_inscription_in_order() {
        let pending = vec![
            queued_write(Value::Integer(1)),
            queued_write(Value::Integer(2)),
        ];
        let ids: Vec<_> = pending.iter().map(|write| write.tx_id).collect();
        let publication = Publication::prepare(pending).unwrap().unwrap();
        let decoded = ChannelBatch::decode(&publication.payload)
            .unwrap()
            .into_writes();

        assert_eq!(
            decoded.iter().map(|write| write.tx_id).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            Publication::matches_pending(&publication.payload, &publication.writes).unwrap(),
            Some(2)
        );
    }

    #[test]
    fn an_oversized_batch_leaves_later_writes_for_the_next_publication() {
        let mut random = StdRng::seed_from_u64(4);
        let mut pending = Vec::new();

        for _ in 0..2 {
            let mut blob = vec![0; 1_100_000];
            random.fill_bytes(&mut blob);
            pending.push(queued_write(Value::Blob(blob)));
        }

        let first = pending[0].tx_id;
        let publication = Publication::prepare(pending).unwrap().unwrap();

        assert_eq!(publication.writes.len(), 1);
        assert_eq!(publication.writes[0].tx_id, first);
        assert_eq!(
            ChannelBatch::decode(&publication.payload)
                .unwrap()
                .into_writes()
                .len(),
            1
        );
    }
}
