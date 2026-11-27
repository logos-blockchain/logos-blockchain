//! Wallet opening records and their database keys. Each record is complete;
//! saving several notes uses one confirmed atomic batch.

use std::collections::HashMap;

use bytes::Bytes;
use lb_binary_codec::bincode::SerializeOp as _;
use lb_groth16::fr_to_bytes;
use lb_wallet::{KnownNote, KnownNotes, NoteId};
use overwatch::DynError;

use super::StorageApi;

const OPENINGS: &[u8] = b"wallet/openings/";
const LEADERSHIP_OPENINGS: &[u8] = b"wallet/leadership_openings/";
const REWARD_KEY_COUNT: &[u8] = b"wallet/reward_key_count";

fn opening_key(prefix: &[u8], id: NoteId) -> Result<Bytes, DynError> {
    let mut key = Vec::with_capacity(prefix.len() + 32);
    key.extend_from_slice(prefix);
    key.extend_from_slice(&id.note_set.to_bytes()?);
    key.extend_from_slice(&fr_to_bytes(&id.commitment.0));
    Ok(key.into())
}

impl<Tx> StorageApi<Tx> {
    /// Atomically stores spending and leadership openings in separate
    /// namespaces.
    pub async fn store_wallet_openings(
        &self,
        spending: &[KnownNote],
        leadership: &[KnownNote],
    ) -> Result<(), DynError> {
        let items = spending
            .iter()
            .map(|note| (OPENINGS, note))
            .chain(leadership.iter().map(|note| (LEADERSHIP_OPENINGS, note)))
            .map(|(prefix, note)| Ok((opening_key(prefix, note.id())?, note.to_bytes()?)))
            .collect::<Result<HashMap<_, _>, DynError>>()?;
        if !items.is_empty() {
            self.bulk_store_confirmed(items).await?;
        }
        Ok(())
    }

    pub async fn load_wallet_leadership_notes(&self) -> Result<KnownNotes, DynError> {
        let notes = self
            .load_prefix::<KnownNote>(Bytes::from_static(LEADERSHIP_OPENINGS))
            .await?;
        Ok(notes.into_iter().map(|note| (note.id(), note)).collect())
    }
    /// Saves every opening in one atomic write, returning the database result.
    pub async fn store_wallet_known_notes(&self, notes: &[KnownNote]) -> Result<(), DynError> {
        self.store_wallet_openings(notes, &[]).await
    }

    pub async fn load_wallet_known_notes(&self) -> Result<KnownNotes, DynError> {
        let notes = self
            .load_prefix::<KnownNote>(Bytes::from_static(OPENINGS))
            .await?;
        Ok(notes.into_iter().map(|note| (note.id(), note)).collect())
    }

    pub async fn store_wallet_reward_key_count(&self, count: u64) -> Result<(), DynError> {
        self.store_confirmed(Bytes::from_static(REWARD_KEY_COUNT), count)
            .await
    }

    pub async fn load_wallet_reward_key_count(&self) -> Result<u64, DynError> {
        self.load(Bytes::from_static(REWARD_KEY_COUNT))
            .await
            .map(Option::unwrap_or_default)
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{Note, note_set::NoteSetIndex, notes::NoteNullifier};
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use overwatch::services::relay::OutboundRelay;
    use tempfile::TempDir;
    use tokio::{sync::mpsc, task::JoinHandle};

    use super::*;
    use crate::{
        StorageMsg,
        backend::StorageBackend as _,
        rocksdb::{RocksBackend, RocksBackendSettings, handle_request},
    };

    #[tokio::test]
    async fn known_notes_are_restored_after_reopen() {
        let directory = TempDir::new().unwrap();
        let settings = settings(&directory);
        let (storage, worker) = connect(RocksBackend::new(settings.clone()).unwrap());
        let notes = [known_note(1), known_note(2)];
        storage.store_wallet_known_notes(&notes).await.unwrap();
        drop(storage);
        worker.await.unwrap();

        let (restored, worker) = connect(RocksBackend::new(settings).unwrap());
        let expected: KnownNotes = notes.into_iter().map(|note| (note.id(), note)).collect();
        assert_eq!(restored.load_wallet_known_notes().await.unwrap(), expected);
        drop(restored);
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn identical_commitments_in_different_sets_survive_reopen() {
        let directory = TempDir::new().unwrap();
        let settings = settings(&directory);
        let (storage, worker) = connect(RocksBackend::new(settings.clone()).unwrap());
        let ledger = known_note(1);
        let sdp = KnownNote {
            note_set: NoteSetIndex::Sdp,
            ..ledger
        };
        storage
            .store_wallet_known_notes(&[ledger, sdp])
            .await
            .unwrap();
        drop(storage);
        worker.await.unwrap();

        let (restored, worker) = connect(RocksBackend::new(settings).unwrap());
        assert_eq!(
            restored.load_wallet_known_notes().await.unwrap(),
            KnownNotes::from_iter([(ledger.id(), ledger), (sdp.id(), sdp)])
        );
        drop(restored);
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_atomic_save_keeps_all_new_openings_out_of_storage() {
        let directory = TempDir::new().unwrap();
        let settings = settings(&directory);
        drop(RocksBackend::new(settings.clone()).unwrap());
        let read_only = RocksBackend::new(RocksBackendSettings {
            read_only: true,
            ..settings
        })
        .unwrap();
        let (storage, worker) = connect(read_only);
        let notes = [known_note(1), known_note(2)];

        assert!(storage.store_wallet_openings(&notes, &notes).await.is_err());
        assert_eq!(storage.load_wallet_known_notes().await.unwrap().size(), 0);
        assert_eq!(
            storage.load_wallet_leadership_notes().await.unwrap().size(),
            0
        );
        drop(storage);
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn undecodable_wallet_records_are_reported_as_errors() {
        let directory = TempDir::new().unwrap();
        let (storage, worker) = connect(RocksBackend::new(settings(&directory)).unwrap());
        let note = known_note(1);
        storage
            .bulk_store_confirmed(HashMap::from([
                (
                    opening_key(OPENINGS, note.id()).unwrap(),
                    Bytes::from_static(b"invalid opening"),
                ),
                (
                    Bytes::from_static(REWARD_KEY_COUNT),
                    Bytes::from_static(b"invalid count"),
                ),
            ]))
            .await
            .unwrap();
        assert!(storage.load_wallet_known_notes().await.is_err());
        assert!(storage.load_wallet_reward_key_count().await.is_err());
        drop(storage);
        worker.await.unwrap();
    }

    fn known_note(nonce: u64) -> KnownNote {
        KnownNote {
            note: Note::new(10, Fr::from(nonce), ZkPublicKey::new(Fr::from(7u64))),
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(nonce)),
        }
    }

    fn settings(directory: &TempDir) -> RocksBackendSettings {
        RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        }
    }

    fn connect(backend: RocksBackend) -> (StorageApi, JoinHandle<()>) {
        let (sender, mut requests) = mpsc::channel::<StorageMsg>(1);
        let worker = tokio::spawn(async move {
            while let Some(request) = requests.recv().await {
                // The handler replies with write failures before returning
                // them.
                drop(handle_request(request, &backend));
            }
        });
        (StorageApi::new(OutboundRelay::new(sender)), worker)
    }
}
