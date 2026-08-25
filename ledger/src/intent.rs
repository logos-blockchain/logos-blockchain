use crate::LedgerState;

/// An intended effect on the ledger that a submitter can watch for.
///
/// For example, the submitter can rebuild/resubmit a transaction if the intent
/// is not applied in time.
pub trait Intent {
    fn status(&self, ledger: &LedgerState) -> IntentStatus;
}

/// How an intent stands against a ledger state.
#[derive(Debug, Clone, Copy)]
pub enum IntentStatus {
    /// The intent has been applied to the ledger.
    Applied,
    /// The intent has not been applied to the ledger yet.
    NotApplied,
    /// The intent cannot be applied to the ledger.
    /// e.g., because its target is gone.
    Inapplicable,
}
