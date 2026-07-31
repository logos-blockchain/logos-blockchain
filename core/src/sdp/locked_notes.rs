use lb_blake2btree::{Blake2bTree, LeafHash};
use lb_groth16::fr_to_bytes;
use lb_utils::bounded::NonEmptyBoundedVec;
use serde::{Deserialize, Serialize};
use strum::EnumCount as _;

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{Note, NoteId},
    sdp::{DeclarationId, MinStake, ServiceType},
};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Note does not exist: {0:?}")]
    NoteDoesNotExist(NoteId),
    #[error("Note {note_id:?} insufficient value: {value}")]
    NoteInsufficientValue { note_id: NoteId, value: u64 },
    #[error("Note {note_id:?} already used for service {service_type:?}")]
    NoteAlreadyUsedForService {
        note_id: NoteId,
        service_type: ServiceType,
    },
    #[error("Note {note_id:?} not locked for {service_type:?}")]
    NoteNotLockedForService {
        note_id: NoteId,
        service_type: ServiceType,
    },
    #[error("Note is not locked: {0:?}")]
    NoteNotLocked(NoteId),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LockedNotes {
    locked_notes: Blake2bTree<NoteId, LockedNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LockedNote {
    note: Note,
    // Declarations locking the note, in the order they appeared on chain, at
    // most one per service.
    declarations: NonEmptyBoundedVec<(ServiceType, DeclarationId), { ServiceType::COUNT }>,
}

impl LockedNote {
    fn sdp_locked_note_hash(&self) -> Hash {
        let mut h = Hasher::new();
        h.update(b"LOCKED_NOTE_HASH_V1");
        h.update(
            u8::try_from(self.declarations.len())
                .expect("a note is locked at most once per service")
                .to_le_bytes(),
        );
        for (_, declaration_id) in &self.declarations {
            h.update(declaration_id.0);
        }
        h.finalize().into()
    }
}

// The note and the services are left out of the leaf: they are derivable from
// the notes and the declarations trees, which commit them already.
impl LeafHash<NoteId> for LockedNote {
    fn leaf_hash(&self, note_id: &NoteId) -> Hash {
        let mut h = Hasher::new();
        h.update(fr_to_bytes(note_id.as_fr()));
        h.update(self.sdp_locked_note_hash());
        h.finalize().into()
    }
}

impl LockedNotes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            locked_notes: Blake2bTree::new(),
        }
    }

    #[must_use]
    pub fn get(&self, id: &NoteId) -> Option<&Note> {
        self.locked_notes.get_ref(id).map(|ln| &ln.note)
    }

    #[must_use]
    pub fn contains(&self, id: &NoteId) -> bool {
        self.locked_notes.contains(id)
    }

    #[must_use]
    pub fn root(&self) -> Hash {
        self.locked_notes.root()
    }

    pub fn lock(
        mut self,
        min_stake: &MinStake,
        service_type: ServiceType,
        declaration_id: DeclarationId,
        note: Note,
        note_id: &NoteId,
    ) -> Result<Self, Error> {
        if note.value < min_stake.threshold {
            return Err(Error::NoteInsufficientValue {
                note_id: *note_id,
                value: note.value,
            });
        }

        if let Some(locked) = self.locked_notes.get_ref(note_id) {
            if locked
                .declarations
                .iter()
                .any(|(service, _)| *service == service_type)
            {
                return Err(Error::NoteAlreadyUsedForService {
                    note_id: *note_id,
                    service_type,
                });
            }
            let mut locked = locked.clone();
            locked
                .declarations
                .try_push((service_type, declaration_id))
                .expect("a note is locked at most once per service");
            self.locked_notes = self
                .locked_notes
                .update(note_id, locked)
                .expect("the note is in the tree");
        } else {
            let declarations = NonEmptyBoundedVec::try_from_iter([(service_type, declaration_id)])
                .expect("a single declaration fits the bounds");
            self.locked_notes = self
                .locked_notes
                .insert(*note_id, LockedNote { note, declarations })
                .0;
        }

        Ok(self)
    }

    #[must_use]
    pub fn is_locked_for_service(&self, note_id: &NoteId, service_type: &ServiceType) -> bool {
        self.locked_notes.get_ref(note_id).is_some_and(|locked| {
            locked
                .declarations
                .iter()
                .any(|(service, _)| service == service_type)
        })
    }

    pub fn unlock(&mut self, service_type: ServiceType, note_id: &NoteId) -> Result<Note, Error> {
        let Some(locked) = self.locked_notes.get_ref(note_id) else {
            return Err(Error::NoteNotLocked(*note_id));
        };
        let Some(index) = locked
            .declarations
            .iter()
            .position(|(service, _)| *service == service_type)
        else {
            return Err(Error::NoteNotLockedForService {
                note_id: *note_id,
                service_type,
            });
        };
        let res = locked.note;

        // Removing the last declaration releases the note
        self.locked_notes = if locked.declarations.len() == 1 {
            self.locked_notes
                .remove(note_id)
                .expect("the note is in the tree")
                .0
        } else {
            let mut locked = locked.clone();
            locked
                .declarations
                .try_remove(index)
                .expect("the note keeps at least one declaration");
            self.locked_notes
                .update(note_id, locked)
                .expect("the note is in the tree")
        };

        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::ZkKey;
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};

    use super::*;
    use crate::mantle::Utxo;

    fn utxo() -> Utxo {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(BigUint::from(0u64));
        Utxo {
            op_id,
            output_index: 0,
            note: Note::new(10000, zk_sk.to_public_key()),
        }
    }

    fn declaration_id(seed: u8) -> DeclarationId {
        DeclarationId([seed; 32])
    }

    #[test]
    fn test_lock_success() {
        let utxo = utxo();
        let note_id = utxo.id();
        let locked_notes = LockedNotes::new();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let locked_notes_bn = locked_notes
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                declaration_id(1),
                utxo.note,
                &note_id,
            )
            .expect("Should be able to lock for BN service");

        assert!(locked_notes_bn.contains(&note_id));
        assert_eq!(
            locked_notes_bn
                .locked_notes
                .get_ref(&note_id)
                .map(|ln| ln.declarations.as_slice()),
            Some([(ServiceType::BlendNetwork, declaration_id(1))].as_slice())
        );
    }

    #[test]
    fn test_lock_fail_already_used() {
        let utxo = utxo();
        let note_id = utxo.id();
        let locked_notes = LockedNotes::new();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let locked_notes_once = locked_notes
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                declaration_id(1),
                utxo.note,
                &note_id,
            )
            .unwrap();

        let result = locked_notes_once.lock(
            &min_stake,
            ServiceType::BlendNetwork,
            declaration_id(2),
            utxo.note,
            &note_id,
        );

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            Error::NoteAlreadyUsedForService {
                note_id,
                service_type: ServiceType::BlendNetwork
            }
        );
    }

    #[test]
    fn lock_fail_insufficient() {
        let utxo = utxo();
        let note_id = utxo.id();
        let locked_notes = LockedNotes::new();
        let min_stake = MinStake {
            threshold: 999_999,
            timestamp: 0,
        };

        let result = locked_notes.lock(
            &min_stake,
            ServiceType::BlendNetwork,
            declaration_id(1),
            utxo.note,
            &note_id,
        );

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            Error::NoteInsufficientValue {
                note_id,
                value: 10000
            }
        );
    }

    #[test]
    fn test_lock_success_for_note() {
        let utxo = utxo();
        let note_id = utxo.id();
        let locked_notes = LockedNotes::new();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let result = locked_notes.lock(
            &min_stake,
            ServiceType::BlendNetwork,
            declaration_id(1),
            utxo.note,
            &note_id,
        );

        assert!(result.is_ok());
    }
    #[test]
    fn test_unlock_last_service_removes_note() {
        let utxo = utxo();
        let note_id = utxo.id();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };
        let mut locked = LockedNotes::new()
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                declaration_id(1),
                utxo.note,
                &note_id,
            )
            .unwrap();

        locked
            .unlock(ServiceType::BlendNetwork, &note_id)
            .expect("Should unlock the last service");

        assert!(!locked.contains(&note_id));
        assert_eq!(locked.locked_notes.size(), 0);
    }

    #[test]
    fn test_unlock_note_not_locked() {
        let note_id = utxo().id();
        let mut empty_notes = LockedNotes::new();
        let result = empty_notes.unlock(ServiceType::BlendNetwork, &note_id);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NoteNotLocked(note_id));
    }

    // Leaf commitment
    fn locked_note(declaration_id: DeclarationId) -> LockedNote {
        LockedNote {
            note: utxo().note,
            declarations: NonEmptyBoundedVec::try_from_iter([(
                ServiceType::BlendNetwork,
                declaration_id,
            )])
            .unwrap(),
        }
    }

    // The encoding is spelled out field by field, so any change to `leaf_hash`
    // has to be mirrored here and in the specification.
    #[test]
    fn locked_note_leaf_matches_the_specified_encoding() {
        let note_id = utxo().id();
        let locked = locked_note(declaration_id(1));

        let mut locked_note_bytes = Vec::new();
        locked_note_bytes.extend_from_slice(b"LOCKED_NOTE_HASH_V1");
        locked_note_bytes.extend_from_slice(&1u8.to_le_bytes()); // declaration count
        locked_note_bytes.extend_from_slice(&declaration_id(1).0);
        let locked_note_hash: Hash = Hasher::digest(&locked_note_bytes).into();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&fr_to_bytes(note_id.as_fr()));
        bytes.extend_from_slice(&locked_note_hash);

        let expected: Hash = Hasher::digest(&bytes).into();
        assert_eq!(locked.leaf_hash(&note_id), expected);
    }

    #[test]
    fn locked_note_leaf_binds_the_note_id() {
        let locked = locked_note(declaration_id(1));
        // `utxo` draws a random `op_id`, so the two notes have distinct ids.
        let note_id = utxo().id();
        let other_note_id = utxo().id();

        assert_ne!(locked.leaf_hash(&note_id), locked.leaf_hash(&other_note_id));
    }

    #[test]
    fn locked_note_leaf_binds_the_declaration() {
        let note_id = utxo().id();

        assert_ne!(
            locked_note(declaration_id(1)).leaf_hash(&note_id),
            locked_note(declaration_id(2)).leaf_hash(&note_id)
        );
    }

    // The note is not committed, only the declarations locking it are.
    #[test]
    fn locked_note_leaf_ignores_the_note() {
        let note_id = utxo().id();
        let locked = locked_note(declaration_id(1));
        let other_note = LockedNote {
            note: Note::new(42, ZkKey::from(BigUint::from(7u64)).to_public_key()),
            ..locked.clone()
        };

        assert_eq!(locked.leaf_hash(&note_id), other_note.leaf_hash(&note_id));
    }

    #[test]
    fn root_tracks_lock_and_unlock() {
        let utxo = utxo();
        let note_id = utxo.id();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let locked_notes = LockedNotes::new();
        let empty_root = locked_notes.root();

        let mut locked_notes = locked_notes
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                declaration_id(1),
                utxo.note,
                &note_id,
            )
            .unwrap();
        assert_ne!(locked_notes.root(), empty_root);

        locked_notes
            .unlock(ServiceType::BlendNetwork, &note_id)
            .unwrap();
        assert_eq!(locked_notes.root(), empty_root);
    }
}
