use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    mantle::{Note, NoteId},
    sdp::{MinStake, ServiceType},
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
    locked_notes: rpds::HashTrieMapSync<NoteId, LockedNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LockedNote {
    note: Note,
    services: HashSet<ServiceType>,
}

impl LockedNotes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            locked_notes: rpds::HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn get(&self, id: &NoteId) -> Option<&Note> {
        self.locked_notes.get(id).map(|ln| &ln.note)
    }

    #[must_use]
    pub fn contains(&self, id: &NoteId) -> bool {
        self.locked_notes.contains_key(id)
    }

    pub fn lock(
        mut self,
        min_stake: &MinStake,
        service_type: ServiceType,
        note: Note,
        note_id: &NoteId,
    ) -> Result<Self, Error> {
        if note.value < min_stake.threshold {
            return Err(Error::NoteInsufficientValue {
                note_id: *note_id,
                value: note.value,
            });
        }

        if let Some(locked) = self.locked_notes.get_mut(note_id) {
            if locked.services.contains(&service_type) {
                return Err(Error::NoteAlreadyUsedForService {
                    note_id: *note_id,
                    service_type,
                });
            }
            locked.services.insert(service_type);
        } else {
            let services = [service_type].into();
            self.locked_notes = self
                .locked_notes
                .insert(*note_id, LockedNote { note, services });
        }

        Ok(self)
    }

    #[must_use]
    pub fn is_locked_for_service(&self, note_id: &NoteId, service_type: &ServiceType) -> bool {
        if let Some(locked) = self.locked_notes.get(note_id) {
            if locked.services.contains(service_type) {
                return true;
            }
            return false;
        }
        false
    }

    pub fn unlock(&mut self, service_type: ServiceType, note_id: &NoteId) -> Result<Note, Error> {
        if let Some(note) = self.locked_notes.get_mut(note_id) {
            if !note.services.remove(&service_type) {
                return Err(Error::NoteNotLockedForService {
                    note_id: *note_id,
                    service_type,
                });
            }
            let res = note.note;
            if note.services.is_empty() {
                self.locked_notes = self.locked_notes.remove(note_id);
            }

            Ok(res)
        } else {
            Err(Error::NoteNotLocked(*note_id))
        }
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
            .lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &note_id)
            .expect("Should be able to lock for BN service");

        assert!(locked_notes_bn.contains(&note_id));
        assert_eq!(
            locked_notes_bn
                .locked_notes
                .get(&note_id)
                .map(|ln| &ln.services),
            Some(&HashSet::from([ServiceType::BlendNetwork]))
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
            .lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &note_id)
            .unwrap();

        let result =
            locked_notes_once.lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &note_id);

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

        let result = locked_notes.lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &note_id);

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

        let result = locked_notes.lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &note_id);

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
            .lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &note_id)
            .unwrap();

        locked
            .unlock(ServiceType::BlendNetwork, &note_id)
            .expect("Should unlock the last service");

        assert!(!locked.contains(&note_id));
        assert!(locked.locked_notes.is_empty());
    }

    #[test]
    fn test_unlock_note_not_locked() {
        let note_id = utxo().id();
        let mut empty_notes = LockedNotes::new();
        let result = empty_notes.unlock(ServiceType::BlendNetwork, &note_id);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NoteNotLocked(note_id));
    }
}
