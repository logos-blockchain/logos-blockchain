use lb_core::{
    events::Events,
    mantle::batch::{self, DeferredZkpVerifications},
};

use crate::LedgerState;

pub struct PreparedUpdate<Id> {
    id: Id,
    state: LedgerState,
    events: Events,
    deferred_zkps: DeferredZkpVerifications,
}

impl<Id> PreparedUpdate<Id> {
    #[must_use]
    pub const fn new(
        id: Id,
        state: LedgerState,
        events: Events,
        deferred_zkps: DeferredZkpVerifications,
    ) -> Self {
        Self {
            id,
            state,
            events,
            deferred_zkps,
        }
    }

    pub fn verify_batch_proofs(self) -> Result<BatchVerifiedUpdate<Id>, batch::Error> {
        self.deferred_zkps.verify()?;
        Ok(BatchVerifiedUpdate {
            id: self.id,
            state: self.state,
            events: self.events,
        })
    }
}

pub struct BatchVerifiedUpdate<Id> {
    pub id: Id,
    pub state: LedgerState,
    pub events: Events,
}

#[cfg(test)]
mod tests {
    use lb_core::{
        events::{Event, HeaderEvent},
        mantle::batch::DeferredZkpVerification,
        sdp::{DeclarationId, ServiceType},
    };
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{ZkKey, public_inputs_from_pks};
    use num_bigint::BigUint;

    use super::*;
    use crate::cryptarchia::tests::{config, utxo};

    const ID: [u8; 32] = [1; 32];

    #[test]
    fn verify_batch_proofs_carries_the_update_through() {
        let state = LedgerState::from_utxos([utxo()], &config());
        let utxos_root = state.latest_utxos().root();
        let update = PreparedUpdate::new(
            ID,
            state,
            Events::from(header_event()),
            std::iter::once(valid_zk_sig()).collect(),
        );

        let update = update.verify_batch_proofs().expect("must succeed");
        assert_eq!(update.id, ID);
        assert_eq!(update.state.latest_utxos().root(), utxos_root);
        assert_eq!(update.events.len(), 1);
    }

    #[test]
    fn verify_batch_proofs_rejects_invalid_deferred_zkp() {
        let update = PreparedUpdate::new(
            ID,
            LedgerState::from_utxos([utxo()], &config()),
            Events::new(),
            std::iter::once(invalid_zk_sig()).collect(),
        );
        assert!(matches!(
            update.verify_batch_proofs(),
            Err(batch::Error::InvalidZkSignatures)
        ));
    }

    fn header_event() -> Event {
        HeaderEvent::SdpNoteUnlocked {
            note_id: utxo().id(),
            service_type: ServiceType::BlendNetwork,
            declaration_id: DeclarationId([2; 32]),
        }
        .into()
    }

    fn valid_zk_sig() -> DeferredZkpVerification {
        zk_sig(1, 1)
    }

    fn invalid_zk_sig() -> DeferredZkpVerification {
        zk_sig(1, 2)
    }

    /// If `msg == msg_for_input`, a valid sig is produced.
    /// Otherwise, an invalid sig is produced.
    fn zk_sig(msg: u64, msg_for_input: u64) -> DeferredZkpVerification {
        let key = ZkKey::from(BigUint::from(1u8));
        let signature = ZkKey::multi_sign(std::slice::from_ref(&key), &Fr::from(msg)).unwrap();
        let inputs =
            public_inputs_from_pks(Fr::from(msg_for_input).into(), &[key.to_public_key()]).unwrap();
        DeferredZkpVerification::ZkSig(*signature.as_proof(), inputs)
    }
}
