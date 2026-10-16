pub mod active;
pub mod declare;
pub mod withdraw;

pub use active::{SDPActiveExecutionContext, SDPActiveValidationContext};
pub use declare::{SDPDeclareExecutionContext, SDPDeclareVerificationContext};
use lb_cryptarchia_engine::Epoch;
use thiserror::Error;
pub use withdraw::{SDPWithdrawExecutionContext, SDPWithdrawValidationContext};

use crate::{
    mantle::NoteId,
    sdp::{Declaration, DeclarationId, Nonce, ProviderId, ServiceType},
};

pub type SDPDeclareOp = crate::sdp::DeclarationMessage;
pub type SDPWithdrawOp = crate::sdp::WithdrawMessage;
pub type SDPActiveOp = crate::sdp::ActiveMessage;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SdpError {
    #[error("Note: {0:?} isn't in the ledger")]
    InexistingNote(NoteId),
    #[error("Note {0:?} is a channel note and cannot be used as service collateral")]
    ChannelNote(NoteId),
    #[error("Invalid SDP declare ZkSignature")]
    InvalidZkSignature,
    #[error("Invalid SDP declare EDDSA signature")]
    InvalidEddsaSignature,
    #[error("Duplicate sdp declaration id: {0:?}")]
    DuplicateDeclaration(DeclarationId),
    #[error("Duplicate provider_id within service {service_type:?}: {provider_id:?}")]
    DuplicateProviderId {
        service_type: ServiceType,
        provider_id: Box<ProviderId>,
    },
    #[error("Note {note_id:?} insufficient value: {value}")]
    NoteInsufficientValue { note_id: NoteId, value: u64 },
    #[error("Note {note_id:?} already used for service {service_type:?}")]
    NoteAlreadyUsedForService {
        note_id: NoteId,
        service_type: ServiceType,
    },
    #[error(
        "An unexpected error occurred during sdp declare execution, please validate the op before executing"
    )]
    UnexpectedError,
    #[error("Sdp declaration id could not be found: {0:?}")]
    DeclarationNotFound(DeclarationId),
    #[error(
        "SDP declaration ID mismatch: operation_declaration_id={operation_declaration_id:?}, supplied_declaration_id={supplied_declaration_id:?}"
    )]
    DeclarationIdMismatch {
        operation_declaration_id: DeclarationId,
        supplied_declaration_id: DeclarationId,
    },
    #[error("Service type could not be found: {0:?}")]
    ServiceNotFound(ServiceType),
    #[error(
        "Sdp declaration has been already scheduled to be withdrawn: {declaration_id:?} at epoch {withdraw_at:?}"
    )]
    DeclarationWithdrawn {
        declaration_id: DeclarationId,
        withdraw_at: Epoch,
    },
    #[error(
        "Invalid SDP nonce lifecycle: nonce_lifecycle_epoch={nonce_lifecycle_epoch:?}, declaration_created_epoch={declaration_created_epoch:?}"
    )]
    InvalidNonceLifecycle {
        nonce_lifecycle_epoch: Epoch,
        declaration_created_epoch: Epoch,
    },
    #[error(
        "Invalid SDP nonce sequence: nonce_sequence={nonce_sequence}, declaration_sequence={declaration_sequence}"
    )]
    InvalidNonceSequence {
        nonce_sequence: u32,
        declaration_sequence: u32,
    },
    #[error("Note is not a service note: {0:?}")]
    NotAServiceNote(NoteId),
    #[error("Note {note_id:?} not used for {service_type:?}")]
    NoteNotUsedForService {
        note_id: NoteId,
        service_type: ServiceType,
    },
}

/// Ensures a caller-supplied declaration is the one named by the operation.
fn validate_declaration_id(
    operation_declaration_id: DeclarationId,
    declaration: &Declaration,
) -> Result<(), SdpError> {
    let supplied_declaration_id = declaration.id();
    if operation_declaration_id != supplied_declaration_id {
        return Err(SdpError::DeclarationIdMismatch {
            operation_declaration_id,
            supplied_declaration_id,
        });
    }

    Ok(())
}

/// Validates the lifecycle and monotonic sequence carried by an Active or
/// Withdraw nonce against the declaration it targets.
fn validate_nonce(candidate: Nonce, declaration: &Declaration) -> Result<(), SdpError> {
    let nonce_lifecycle_epoch = candidate.lifecycle_epoch();
    if nonce_lifecycle_epoch != declaration.created {
        return Err(SdpError::InvalidNonceLifecycle {
            nonce_lifecycle_epoch,
            declaration_created_epoch: declaration.created,
        });
    }

    let nonce_sequence = candidate.sequence();
    let declaration_sequence = declaration.nonce.sequence();
    if nonce_sequence <= declaration_sequence {
        return Err(SdpError::InvalidNonceSequence {
            nonce_sequence,
            declaration_sequence,
        });
    }

    Ok(())
}
