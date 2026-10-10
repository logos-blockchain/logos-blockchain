use serde::{Deserialize, Serialize};

use crate::{
    mantle::{Note, NoteId},
    sdp::{DeclarationId, MinStake, ServiceType},
};

type ServiceBindings = rpds::RedBlackTreeMapSync<ServiceType, DeclarationId>;

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
    #[error("Note {note_id:?} not used for {service_type:?}")]
    NoteNotUsedForService {
        note_id: NoteId,
        service_type: ServiceType,
    },
    #[error("Note is not a service note: {0:?}")]
    NotAServiceNote(NoteId),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServiceNotes {
    service_notes: rpds::HashTrieMapSync<NoteId, ServiceNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ServiceNote {
    note: Note,
    services: ServiceBindings,
}

impl ServiceNotes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            service_notes: rpds::HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn get(&self, id: &NoteId) -> Option<&Note> {
        self.service_notes.get(id).map(|ln| &ln.note)
    }

    #[must_use]
    pub fn contains(&self, id: &NoteId) -> bool {
        self.service_notes.contains_key(id)
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

        if let Some(service_note) = self.service_notes.get_mut(note_id) {
            if service_note.services.contains_key(&service_type) {
                return Err(Error::NoteAlreadyUsedForService {
                    note_id: *note_id,
                    service_type,
                });
            }
            service_note
                .services
                .insert_mut(service_type, declaration_id);
        } else {
            let services = ServiceBindings::new_sync().insert(service_type, declaration_id);
            self.service_notes = self
                .service_notes
                .insert(*note_id, ServiceNote { note, services });
        }

        Ok(self)
    }

    #[must_use]
    pub fn is_used_for_service(&self, note_id: &NoteId, service_type: &ServiceType) -> bool {
        self.service_notes
            .get(note_id)
            .is_some_and(|service_note| service_note.services.contains_key(service_type))
    }

    #[must_use]
    pub fn is_used_by_declaration(
        &self,
        note_id: &NoteId,
        service_type: &ServiceType,
        declaration_id: &DeclarationId,
    ) -> bool {
        self.service_notes
            .get(note_id)
            .and_then(|service_note| service_note.services.get(service_type))
            == Some(declaration_id)
    }

    pub fn unlock(
        &mut self,
        service_type: ServiceType,
        declaration_id: DeclarationId,
        note_id: &NoteId,
    ) -> Result<Note, Error> {
        if let Some(note) = self.service_notes.get_mut(note_id) {
            if note.services.get(&service_type) != Some(&declaration_id) {
                return Err(Error::NoteNotUsedForService {
                    note_id: *note_id,
                    service_type,
                });
            }
            note.services.remove_mut(&service_type);
            let res = note.note;
            if note.services.is_empty() {
                self.service_notes = self.service_notes.remove(note_id);
            }

            Ok(res)
        } else {
            Err(Error::NotAServiceNote(*note_id))
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
        let service_notes = ServiceNotes::new();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let service_notes_bn = service_notes
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                DeclarationId([1; 32]),
                utxo.note,
                &note_id,
            )
            .expect("Should be able to lock for BN service");

        assert!(service_notes_bn.contains(&note_id));
        assert_eq!(
            service_notes_bn
                .service_notes
                .get(&note_id)
                .and_then(|ln| ln.services.get(&ServiceType::BlendNetwork)),
            Some(&DeclarationId([1; 32]))
        );
    }

    #[test]
    fn test_lock_fail_already_used() {
        let utxo = utxo();
        let note_id = utxo.id();
        let service_notes = ServiceNotes::new();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let service_notes_once = service_notes
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                DeclarationId([1; 32]),
                utxo.note,
                &note_id,
            )
            .unwrap();

        let result = service_notes_once.lock(
            &min_stake,
            ServiceType::BlendNetwork,
            DeclarationId([2; 32]),
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
        let service_notes = ServiceNotes::new();
        let min_stake = MinStake {
            threshold: 999_999,
            timestamp: 0,
        };

        let result = service_notes.lock(
            &min_stake,
            ServiceType::BlendNetwork,
            DeclarationId([1; 32]),
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
        let service_notes = ServiceNotes::new();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };

        let result = service_notes.lock(
            &min_stake,
            ServiceType::BlendNetwork,
            DeclarationId([1; 32]),
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
        let mut locked = ServiceNotes::new()
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                DeclarationId([1; 32]),
                utxo.note,
                &note_id,
            )
            .unwrap();

        locked
            .unlock(ServiceType::BlendNetwork, DeclarationId([1; 32]), &note_id)
            .expect("Should unlock the last service");

        assert!(!locked.contains(&note_id));
        assert!(locked.service_notes.is_empty());
    }

    #[test]
    fn test_unlock_note_not_a_service_note() {
        let note_id = utxo().id();
        let mut empty_notes = ServiceNotes::new();
        let result =
            empty_notes.unlock(ServiceType::BlendNetwork, DeclarationId([1; 32]), &note_id);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NotAServiceNote(note_id));
    }

    /// The same note remains locked until its final service binding is removed.
    #[test]
    fn shared_note_stays_locked_until_last_service_releases_it() {
        let utxo = utxo();
        let note_id = utxo.id();
        let min_stake = MinStake {
            threshold: 1,
            timestamp: 0,
        };
        let declaration_a = DeclarationId([1; 32]);
        let declaration_b = DeclarationId([2; 32]);
        let mut notes = ServiceNotes::new()
            .lock(
                &min_stake,
                ServiceType::BlendNetwork,
                declaration_a,
                utxo.note,
                &note_id,
            )
            .unwrap()
            .lock(
                &min_stake,
                ServiceType::Test,
                declaration_b,
                utxo.note,
                &note_id,
            )
            .unwrap();

        notes
            .unlock(ServiceType::BlendNetwork, declaration_a, &note_id)
            .unwrap();
        assert!(notes.contains(&note_id));
        assert!(!notes.is_used_for_service(&note_id, &ServiceType::BlendNetwork));
        assert!(notes.is_used_by_declaration(&note_id, &ServiceType::Test, &declaration_b));

        notes
            .unlock(ServiceType::Test, declaration_b, &note_id)
            .unwrap();
        assert!(!notes.contains(&note_id));
    }
}
