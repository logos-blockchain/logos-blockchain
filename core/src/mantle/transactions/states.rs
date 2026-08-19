/// Marker trait for valid verification states of a transaction
pub trait VerificationState {}

/// Unverified state of a Transaction/Operation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unverified;
impl VerificationState for Unverified {}

/// Stateless-verified state of a Transaction/Operation
///
/// This state indicates that the transaction has passed stateless checks. That
/// is, checks that do not require access to the blockchain state, such as
/// signature verification and basic transaction format validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preverified;
impl VerificationState for Preverified {}

/// Stateful-verified state of a Transaction/Operation
///
/// This state indicates that the transaction has passed stateful checks, which
/// require access to the blockchain state.
/// ZK proof verifications are not included in this state, because they are deferred
/// to the batch verification stage, which is performed after all transactions have
/// been statefully verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified;
impl VerificationState for Verified {}
