use lb_core::{
    events::Events,
    mantle::batch::{self, DeferredProofs},
};

use crate::LedgerState;

pub struct PreparedUpdate<Id> {
    id: Id,
    state: LedgerState,
    events: Events,
    deferred_proofs: DeferredProofs,
}

impl<Id> PreparedUpdate<Id> {
    #[must_use]
    pub const fn new(
        id: Id,
        state: LedgerState,
        events: Events,
        deferred_proofs: DeferredProofs,
    ) -> Self {
        Self {
            id,
            state,
            events,
            deferred_proofs,
        }
    }

    pub fn verify_batch_proofs(self) -> Result<BatchVerifiedUpdate<Id>, batch::Error> {
        self.deferred_proofs.verify()?;
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
        mantle::{batch::DeferredZkTransfer, notes::OutputCommitments},
        sdp::{DeclarationId, ServiceType},
    };
    use lb_groth16::{Field as _, Fr};
    use lb_zktransfer::ZkTransferVerifierInput;

    use super::*;
    use crate::cryptarchia::tests::{GENESIS_POW_REWARD_POOL, config, note};

    const ID: [u8; 32] = [1; 32];

    #[test]
    fn verify_batch_proofs_carries_the_update_through() {
        let state = LedgerState::from_notes([note()], &config(), GENESIS_POW_REWARD_POOL);
        let utxos_root = state.latest_note_sets().eligible_root();
        let update = PreparedUpdate::new(
            ID,
            state,
            Events::from(header_event()),
            std::iter::once(valid_zk_transfer()).collect(),
        );

        let update = update.verify_batch_proofs().expect("must succeed");
        assert_eq!(update.id, ID);
        assert_eq!(update.state.latest_note_sets().eligible_root(), utxos_root);
        assert_eq!(update.events.len(), 1);
    }

    #[test]
    fn verify_batch_proofs_rejects_invalid_deferred_proof() {
        let update = PreparedUpdate::new(
            ID,
            LedgerState::from_notes([note()], &config(), GENESIS_POW_REWARD_POOL),
            Events::new(),
            std::iter::once(invalid_zk_transfer()).collect(),
        );
        assert!(matches!(
            update.verify_batch_proofs(),
            Err(batch::Error::InvalidZkTransfers)
        ));
    }

    fn header_event() -> Event {
        HeaderEvent::SdpWithdrawalReleased {
            service_type: ServiceType::BlendNetwork,
            declaration_id: DeclarationId([2; 32]),
            outputs: OutputCommitments::default(),
        }
        .into()
    }

    fn zk_transfer(msg: Fr) -> DeferredZkTransfer {
        let mut data = lb_zktransfer::test_utils::sample_witness_data();
        data.chain.msg = msg;
        let (proof, inputs) = lb_zktransfer::prove(data.into()).expect("the sample witness proves");
        DeferredZkTransfer::new(proof, inputs)
    }

    fn valid_zk_transfer() -> DeferredZkTransfer {
        zk_transfer(Fr::ONE)
    }

    // A proof of `msg = 1` checked against `msg = 2`.
    fn invalid_zk_transfer() -> DeferredZkTransfer {
        let (proof, inputs) = zk_transfer(Fr::ONE).into_parts();
        DeferredZkTransfer::new(
            proof,
            ZkTransferVerifierInput {
                msg: Fr::from(2u64).into(),
                ..inputs
            },
        )
    }
}
