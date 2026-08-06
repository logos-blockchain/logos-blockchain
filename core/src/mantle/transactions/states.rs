/// Marker trait for valid verification states of a transaction
pub trait VerificationState {}

/// Unverified state of a Transaction/Operation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unverified;
impl VerificationState for Unverified {}

/// Partially verified state of a Transaction/Operation
///
/// This state indicates that the transaction has passed stateless checks. That
/// is, checks that do not require access to the blockchain state, such as
/// signature verification and basic transaction format validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preverified;
impl VerificationState for Preverified {}

/// Fully verified state of a Transaction/Operation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified;
impl VerificationState for Verified {}
