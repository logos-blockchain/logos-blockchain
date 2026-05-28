use lb_blend_proofs::{quota, selection};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Encapsulation count exceeded")]
    EncapsulationCountExceeded,
    #[error("Empty encapsulation inputs")]
    EmptyEncapsulationInputs,
    #[error("Payload too large")]
    PayloadTooLarge,
    #[error("Proof of selection verification failed")]
    ProofOfSelectionVerificationFailed(selection::Error),
    #[error("Proof of quota verification failed")]
    ProofOfQuotaVerificationFailed(quota::Error),
    #[error("Encapsulated message deserialization failed")]
    MessageDeserializationFailed,
    #[error("Payload deserialization failed")]
    PayloadDeserializationFailed,
    #[error("Private header deserialization failed")]
    PrivateHeaderDeserializationFailed,
    #[error("Invalid payload length")]
    InvalidPayloadLength,
    #[error("Signature verification failed")]
    SignatureVerificationFailed,
    #[error("Node is not a core node")]
    NotCoreNodeReceiver,
    #[error("Node has generated the maximum number of allowed Proof of Quota this session")]
    NoMoreProofOfQuotas,
    #[error("Attempted to generate a leadership proof without any secret PoL info provided.")]
    NoLeadershipInfoProvided,
    #[error("Invalid shared secret")]
    InvalidSharedSecret,
}
