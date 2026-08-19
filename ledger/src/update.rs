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
