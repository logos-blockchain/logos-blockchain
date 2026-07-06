/// Marker trait for valid verification states of a transaction
pub trait VerificationState {}

/// Unverified state of a Transaction
pub struct Unverified;
impl VerificationState for Unverified {}

/// Partially verified state of a Transaction
pub struct Preverified {
    pub pending: Vec<usize>,
}
impl VerificationState for Preverified {}

/// Fully verified state of a Transaction
pub struct Verified;
impl VerificationState for Verified {}
