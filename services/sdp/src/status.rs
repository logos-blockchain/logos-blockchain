use lb_chain_service::{BlendDeclarationStatus, Epoch};
use lb_core::sdp::{Declaration, DeclarationId};
use lb_ledger::mantle::sdp::rewards::blend::RewardEstimate;

use crate::{
    Participation,
    activity_submission::{ActivityProofReport, ActivityProofSubmissionError},
};

/// Where the node's Blend declaration is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationState {
    /// No declaration posted.
    NotDeclared,
    /// Posted, but not in the ledger yet.
    Pending,
    /// In the ledger, but not in an epoch snapshot yet.
    Activating,
    /// In the current epoch's snapshot.
    Active,
    /// Past `first_epoch_in_snapshot`, but not in the current epoch's snapshot.
    Inactive,
    /// Withdrawal posted. Once it lands, the declaration is withdrawn at
    /// `withdraw_at`, and the locked note unlocks the epoch after.
    Withdrawn,
}

impl DeclarationState {
    /// # Arguments
    ///
    /// - `declared`: Whether the SDP service holds a live declaration ID.
    /// - `declaration`: The declaration in the ledger, if any.
    /// - `current_epoch`: The epoch of the chain tip.
    /// - `is_in_snapshot`: Whether the declaration is in the current epoch's
    ///   snapshot.
    #[must_use]
    fn new(
        declared: bool,
        declaration: Option<&Declaration>,
        current_epoch: Epoch,
        is_in_snapshot: bool,
    ) -> Self {
        declaration.map_or(
            if declared {
                Self::Pending
            } else {
                Self::NotDeclared
            },
            |declaration| {
                if !declared || declaration.withdraw_at.is_some() {
                    Self::Withdrawn
                } else if is_in_snapshot {
                    Self::Active
                } else if current_epoch < declaration.first_epoch_in_snapshot() {
                    Self::Activating
                } else {
                    Self::Inactive
                }
            },
        )
    }
}

/// Why the node owes no activity proof for the activity epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofNotRequiredReason {
    /// The chain is in its first epoch, so there's no activity epoch.
    FirstEpoch,
    /// The activity epoch had fewer providers than the minimum network size,
    /// so the ledger accepts no proofs for it.
    NetworkBelowMinimum,
    /// The node wasn't a provider in the activity epoch, as in its first
    /// active epoch.
    NotInActivityEpoch,
}

/// Why the node's activity proof for the activity epoch isn't in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofFailureReason {
    /// No proof submitted. Also seen briefly at the start of an epoch, before
    /// the proof is submitted.
    NoProof,
    /// The node's declaration wasn't found when submitting the proof.
    DeclarationNotFound,
    /// The proof transaction couldn't be built.
    TransactionBuildFailed,
    /// The mempool rejected the proof transaction.
    MempoolRejected,
}

/// The state of the node's activity proof for the activity epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofState {
    /// Submitted, but not in the ledger yet.
    Pending,
    /// In the ledger.
    Accepted,
    /// Not in the ledger, and not pending.
    Failed(ProofFailureReason),
    /// No proof owed.
    NotRequired(ProofNotRequiredReason),
}

impl ProofState {
    /// # Arguments
    ///
    /// - `current_epoch`: The epoch of the chain tip.
    /// - `ledger_proof_status`: The ledger's view of the proof. `None` when
    ///   there's no activity epoch.
    /// - `last_activity_proof_report`: The node's last proof submission.
    #[must_use]
    fn new(
        current_epoch: Epoch,
        ledger_proof_status: Option<LedgerProofStatus>,
        last_activity_proof_report: Option<ActivityProofReport>,
    ) -> Self {
        let Some(ledger_proof_status) = ledger_proof_status else {
            if current_epoch == Epoch::new(0) {
                return Self::NotRequired(ProofNotRequiredReason::FirstEpoch);
            }
            return Self::NotRequired(ProofNotRequiredReason::NetworkBelowMinimum);
        };
        if !ledger_proof_status.in_activity_epoch {
            return Self::NotRequired(ProofNotRequiredReason::NotInActivityEpoch);
        }
        if ledger_proof_status.did_submit_proof {
            return Self::Accepted;
        }
        let Some(report) = last_activity_proof_report else {
            return Self::Failed(ProofFailureReason::NoProof);
        };
        if report.epoch != ledger_proof_status.activity_epoch {
            return Self::Failed(ProofFailureReason::NoProof);
        }
        match report.submission {
            Ok(_) => Self::Pending,
            Err(ActivityProofSubmissionError::DeclarationNotFound) => {
                Self::Failed(ProofFailureReason::DeclarationNotFound)
            }
            Err(ActivityProofSubmissionError::TransactionBuildFailed) => {
                Self::Failed(ProofFailureReason::TransactionBuildFailed)
            }
            Err(ActivityProofSubmissionError::MempoolRejected) => {
                Self::Failed(ProofFailureReason::MempoolRejected)
            }
        }
    }
}

/// The ledger's view of the node's activity proof.
#[derive(Clone, Copy)]
struct LedgerProofStatus {
    /// The epoch being paid out.
    activity_epoch: Epoch,
    /// Whether the node was a provider in `activity_epoch`.
    in_activity_epoch: bool,
    /// Whether the ledger holds the node's proof.
    did_submit_proof: bool,
}

impl From<&RewardEstimate> for LedgerProofStatus {
    fn from(estimate: &RewardEstimate) -> Self {
        Self {
            activity_epoch: estimate.activity_epoch,
            in_activity_epoch: estimate.in_activity_epoch,
            did_submit_proof: estimate.did_submit_proof,
        }
    }
}

/// The node's Blend declaration in the ledger.
#[derive(Debug, Clone)]
pub struct DeclarationStatus {
    pub id: DeclarationId,
    pub declaration: Declaration,
    /// The first epoch whose snapshot includes the declaration.
    pub first_epoch_in_snapshot: Epoch,
}

impl DeclarationStatus {
    #[must_use]
    const fn new(id: DeclarationId, declaration: Declaration) -> Self {
        Self {
            id,
            first_epoch_in_snapshot: declaration.first_epoch_in_snapshot(),
            declaration,
        }
    }
}

/// The node's Blend activity.
#[derive(Debug, Clone, Copy)]
pub struct ActivityStatus {
    /// The epoch being paid out. `None` when there's no activity epoch.
    pub activity_epoch: Option<Epoch>,
    /// The state of the activity proof for `activity_epoch`.
    pub proof_state: ProofState,
    /// The epoch in which the node's latest activity proof landed. Starts at
    /// the declaration's `first_epoch_in_snapshot`.
    pub last_active_epoch: Epoch,
}

impl ActivityStatus {
    /// # Arguments
    ///
    /// - `current_epoch`: The epoch of the chain tip.
    /// - `status`: The declaration's status in the ledger.
    /// - `last_activity_proof_report`: The node's last proof submission.
    #[must_use]
    fn new(
        current_epoch: Epoch,
        status: &BlendDeclarationStatus,
        last_activity_proof_report: Option<ActivityProofReport>,
    ) -> Self {
        let ledger_proof_status = status.estimate.as_ref().map(LedgerProofStatus::from);
        Self {
            activity_epoch: ledger_proof_status
                .map(|ledger_proof_status| ledger_proof_status.activity_epoch),
            proof_state: ProofState::new(
                current_epoch,
                ledger_proof_status,
                last_activity_proof_report,
            ),
            last_active_epoch: status.declaration.active,
        }
    }
}

/// The node's Blend declaration and activity.
#[derive(Debug, Clone)]
pub struct BlendStatus {
    /// The epoch of the chain tip.
    pub current_epoch: Epoch,
    pub declaration_state: DeclarationState,
    /// `None` while the declaration isn't in the ledger.
    pub declaration: Option<DeclarationStatus>,
    /// `None` while the declaration isn't in the ledger, or while it's
    /// activating.
    pub activity: Option<ActivityStatus>,
}

impl BlendStatus {
    /// # Arguments
    ///
    /// - `participation`: The node's participation, if any.
    /// - `last_activity_proof_report`: The node's last proof submission.
    /// - `chain_status`: The ledger's Blend view at the chain tip.
    #[must_use]
    pub fn new(
        participation: Option<Participation>,
        last_activity_proof_report: Option<ActivityProofReport>,
        chain_status: lb_chain_service::BlendStatus,
    ) -> Self {
        let lb_chain_service::BlendStatus {
            epoch: current_epoch,
            declaration_status,
        } = chain_status;

        let declaration_state = DeclarationState::new(
            matches!(participation, Some(Participation::Live(_))),
            declaration_status
                .as_ref()
                .map(|status| &status.declaration),
            current_epoch,
            declaration_status
                .as_ref()
                .is_some_and(|status| status.is_in_snapshot),
        );
        let activity = declaration_status.as_ref().and_then(|status| {
            if declaration_state == DeclarationState::Activating {
                None
            } else {
                Some(ActivityStatus::new(
                    current_epoch,
                    status,
                    last_activity_proof_report,
                ))
            }
        });
        let declaration =
            if let (Some(participation), Some(status)) = (participation, declaration_status) {
                Some(DeclarationStatus::new(
                    participation.declaration_id(),
                    status.declaration,
                ))
            } else {
                None
            };

        Self {
            current_epoch,
            declaration_state,
            declaration,
            activity,
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_core::{
        header::HeaderId,
        mantle::{NoteId, TxHash},
        sdp::{Locator, Nonce, ProviderId, SNAPSHOT_FINALIZATION_DELAY, ServiceType},
    };
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};

    use super::*;
    use crate::activity_submission::ActivitySubmission;

    fn declaration(created: Epoch, withdraw_at: Option<Epoch>) -> Declaration {
        Declaration {
            service_type: ServiceType::BlendNetwork,
            provider_id: ProviderId(Ed25519Key::from_bytes(&[1; 32]).public_key()),
            service_note_id: NoteId(*ZkPublicKey::zero().as_fr()),
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            zk_id: ZkPublicKey::zero(),
            created,
            active: created.strict_add(SNAPSHOT_FINALIZATION_DELAY),
            withdraw_at,
            nonce: Nonce::new(created, 0),
        }
    }

    #[test]
    fn withdrawn_declaration_stays_withdrawn_until_the_ledger_drops_it() {
        let current_epoch = Epoch::new(5);
        let withdrawing = declaration(Epoch::new(0), Some(Epoch::new(6)));
        let withdrawal_not_landed = declaration(Epoch::new(0), None);

        assert_eq!(
            DeclarationState::new(false, Some(&withdrawing), current_epoch, true),
            DeclarationState::Withdrawn
        );
        assert_eq!(
            DeclarationState::new(false, Some(&withdrawal_not_landed), current_epoch, true),
            DeclarationState::Withdrawn
        );
        assert_eq!(
            DeclarationState::new(true, Some(&withdrawing), current_epoch, true),
            DeclarationState::Withdrawn
        );
        assert_eq!(
            DeclarationState::new(false, None, current_epoch, false),
            DeclarationState::NotDeclared
        );
    }

    #[test]
    fn live_declaration_moves_from_pending_to_active() {
        let current_epoch = Epoch::new(1);
        let landed = declaration(Epoch::new(0), None);

        assert_eq!(
            DeclarationState::new(true, None, current_epoch, false),
            DeclarationState::Pending
        );
        assert_eq!(
            DeclarationState::new(true, Some(&landed), current_epoch, false),
            DeclarationState::Activating
        );
        assert_eq!(
            DeclarationState::new(true, Some(&landed), current_epoch, true),
            DeclarationState::Active
        );
        assert_eq!(
            DeclarationState::new(true, Some(&landed), Epoch::new(2), false),
            DeclarationState::Inactive
        );
    }

    #[test]
    fn activity_is_not_required_in_the_first_epoch() {
        assert_eq!(
            ProofState::new(Epoch::new(0), None, None),
            ProofState::NotRequired(ProofNotRequiredReason::FirstEpoch)
        );
    }

    #[test]
    fn activity_without_an_activity_epoch_later_is_below_minimum() {
        assert_eq!(
            ProofState::new(Epoch::new(3), None, None),
            ProofState::NotRequired(ProofNotRequiredReason::NetworkBelowMinimum)
        );
    }

    #[test]
    fn activity_is_not_required_outside_the_activity_epoch() {
        assert_eq!(
            ProofState::new(
                Epoch::new(3),
                Some(LedgerProofStatus {
                    activity_epoch: Epoch::new(2),
                    in_activity_epoch: false,
                    did_submit_proof: false
                }),
                None
            ),
            ProofState::NotRequired(ProofNotRequiredReason::NotInActivityEpoch)
        );
    }

    #[test]
    fn activity_follows_the_ledger_then_the_last_report() {
        let current_epoch = Epoch::new(5);
        let epoch = Epoch::new(4);
        let report = |epoch, submission| ActivityProofReport { epoch, submission };
        let submitted = Ok(ActivitySubmission {
            tip: HeaderId::from([0; 32]),
            tx_id: TxHash::default(),
        });

        assert_eq!(
            ProofState::new(
                current_epoch,
                Some(LedgerProofStatus {
                    activity_epoch: epoch,
                    in_activity_epoch: true,
                    did_submit_proof: true
                }),
                Some(report(epoch, submitted))
            ),
            ProofState::Accepted
        );
        assert_eq!(
            ProofState::new(
                current_epoch,
                Some(LedgerProofStatus {
                    activity_epoch: epoch,
                    in_activity_epoch: true,
                    did_submit_proof: false
                }),
                Some(report(epoch, submitted))
            ),
            ProofState::Pending
        );
        assert_eq!(
            ProofState::new(
                current_epoch,
                Some(LedgerProofStatus {
                    activity_epoch: epoch,
                    in_activity_epoch: true,
                    did_submit_proof: false
                }),
                Some(report(
                    epoch,
                    Err(ActivityProofSubmissionError::MempoolRejected)
                ))
            ),
            ProofState::Failed(ProofFailureReason::MempoolRejected)
        );
        assert_eq!(
            ProofState::new(
                current_epoch,
                Some(LedgerProofStatus {
                    activity_epoch: epoch,
                    in_activity_epoch: true,
                    did_submit_proof: false
                }),
                Some(report(Epoch::new(3), submitted))
            ),
            ProofState::Failed(ProofFailureReason::NoProof)
        );
    }
}
