/// Marker trait for valid verification states of a transaction
pub trait VerificationState {}

/// Unverified state of a Transaction
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unverified;
impl VerificationState for Unverified {}

/// Partially verified state of a Transaction
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preverified;
impl VerificationState for Preverified {}
