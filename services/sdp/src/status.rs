use lb_chain_service::{BlendDeclarationStatus, Epoch};
use lb_core::sdp::{Declaration, DeclarationId, SNAPSHOT_FINALIZATION_DELAY};
use lb_ledger::mantle::sdp::rewards::blend::RewardEstimate;

use crate::{
    Participation,
    activity_submission::{ActivityProofReport, ActivityProofSubmissionError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationState {
    NotDeclared,
    /// Posted, but not in the ledger yet.
    Pending,
    /// In the ledger, but not yet in an epoch snapshot.
    Activating,
    Active,
    Inactive,
    /// Withdrawal posted. The node still serves until `withdraw_at`, and the
    /// locked note unlocks the epoch after.
    Withdrawn,
}

impl DeclarationState {
    /// `declared` is whether the SDP service holds a live declaration id. A
    /// `declaration` without one is a withdrawn declaration still in the
    /// ledger.
    #[must_use]
    fn new(
        declared: bool,
        declaration: Option<&Declaration>,
        current_epoch: Epoch,
        in_snapshot: bool,
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
                } else if in_snapshot {
                    Self::Active
                } else if current_epoch < active_from_epoch(declaration) {
                    Self::Activating
                } else {
                    Self::Inactive
                }
            },
        )
    }
}

/// Why no activity proof is due for the epoch being paid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotRequiredReason {
    /// The chain is in its first epoch, which has no earlier epoch to pay out.
    FirstEpoch,
    /// The epoch being paid out had fewer providers than the minimum network
    /// size, so the ledger accepts no activity.
    NetworkBelowMinimum,
    /// The node was not in the epoch being paid out, as in its first active
    /// epoch.
    NotInActivityEpoch,
}

/// Why the activity proof for the epoch being paid out failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// No activity proof was submitted for the epoch being paid out. Also
    /// reported briefly after an epoch starts, before the proof is submitted.
    NoProof,
    DeclarationNotFound,
    TxBuildFailed,
    PostFailed,
}

/// How this node's activity for the epoch being paid out stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityResult {
    /// Posted, but not accepted by the ledger yet.
    Pending,
    Accepted,
    Failed(FailureReason),
    NotRequired(NotRequiredReason),
}

impl ActivityResult {
    /// `activity_epoch` is `None` when no epoch is being paid out.
    #[must_use]
    fn new(
        current_epoch: Epoch,
        activity_epoch: Option<ActivityEpoch>,
        last_activity_proof_report: Option<ActivityProofReport>,
    ) -> Self {
        let Some(activity_epoch) = activity_epoch else {
            if current_epoch == Epoch::new(0) {
                return Self::NotRequired(NotRequiredReason::FirstEpoch);
            }
            return Self::NotRequired(NotRequiredReason::NetworkBelowMinimum);
        };
        if !activity_epoch.in_activity_epoch {
            return Self::NotRequired(NotRequiredReason::NotInActivityEpoch);
        }
        if activity_epoch.proof_accepted {
            return Self::Accepted;
        }
        let Some(report) = last_activity_proof_report else {
            return Self::Failed(FailureReason::NoProof);
        };
        if report.epoch != activity_epoch.epoch {
            return Self::Failed(FailureReason::NoProof);
        }
        match report.submission {
            Ok(_) => Self::Pending,
            Err(ActivityProofSubmissionError::DeclarationNotFound) => {
                Self::Failed(FailureReason::DeclarationNotFound)
            }
            Err(ActivityProofSubmissionError::TransactionBuildFailed) => {
                Self::Failed(FailureReason::TxBuildFailed)
            }
            Err(ActivityProofSubmissionError::MempoolRejected) => {
                Self::Failed(FailureReason::PostFailed)
            }
        }
    }
}

/// The ledger's view of this node in the epoch being paid out.
#[derive(Clone, Copy)]
struct ActivityEpoch {
    epoch: Epoch,
    in_activity_epoch: bool,
    proof_accepted: bool,
}

impl From<&RewardEstimate> for ActivityEpoch {
    fn from(estimate: &RewardEstimate) -> Self {
        Self {
            epoch: estimate.activity_epoch,
            in_activity_epoch: estimate.in_activity_epoch,
            proof_accepted: estimate.did_submit_proof,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeclarationStatus {
    pub id: DeclarationId,
    pub declaration: Declaration,
    pub active_from_epoch: Epoch,
}

impl DeclarationStatus {
    #[must_use]
    const fn new(id: DeclarationId, declaration: Declaration) -> Self {
        Self {
            id,
            active_from_epoch: active_from_epoch(&declaration),
            declaration,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ActivityStatus {
    pub result: ActivityResult,
    /// The latest epoch an activity message was accepted for.
    /// Starts at the declaration's activation epoch.
    pub last_active_epoch: Epoch,
}

impl ActivityStatus {
    #[must_use]
    fn new(
        current_epoch: Epoch,
        status: &BlendDeclarationStatus,
        last_activity_proof_report: Option<ActivityProofReport>,
    ) -> Self {
        let activity_epoch = status.estimate.as_ref().map(ActivityEpoch::from);
        Self {
            result: ActivityResult::new(current_epoch, activity_epoch, last_activity_proof_report),
            last_active_epoch: status.declaration.active,
        }
    }
}

/// This node's Blend declaration and how its activity for the epoch being
/// paid out stands.
#[derive(Debug, Clone)]
pub struct BlendStatus {
    pub epoch: Epoch,
    pub declaration_state: DeclarationState,
    /// `None` while the declaration is not in the ledger.
    pub declaration: Option<DeclarationStatus>,
    /// `None` while there's no declaration or it's still activating.
    pub activity: Option<ActivityStatus>,
}

impl BlendStatus {
    #[must_use]
    pub fn new(
        participation: Option<Participation>,
        last_activity_proof_report: Option<ActivityProofReport>,
        chain_status: lb_chain_service::BlendStatus,
    ) -> Self {
        let lb_chain_service::BlendStatus {
            epoch: current_epoch,
            declaration_status: declaration,
        } = chain_status;

        let declaration_state = DeclarationState::new(
            matches!(participation, Some(Participation::Live(_))),
            declaration.as_ref().map(|status| &status.declaration),
            current_epoch,
            declaration
                .as_ref()
                .is_some_and(|status| status.is_in_snapshot),
        );
        let activity = declaration.as_ref().and_then(|status| {
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
        let declaration = if let (Some(participation), Some(status)) = (participation, declaration)
        {
            Some(DeclarationStatus::new(
                participation.declaration_id(),
                status.declaration,
            ))
        } else {
            None
        };

        Self {
            epoch: current_epoch,
            declaration_state,
            declaration,
            activity,
        }
    }
}

const fn active_from_epoch(declaration: &Declaration) -> Epoch {
    declaration.created.strict_add(SNAPSHOT_FINALIZATION_DELAY)
}

#[cfg(test)]
mod tests {
    use lb_core::{
        header::HeaderId,
        mantle::{NoteId, TxHash},
        sdp::{Locator, Nonce, ProviderId, ServiceType},
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
            ActivityResult::new(Epoch::new(0), None, None),
            ActivityResult::NotRequired(NotRequiredReason::FirstEpoch)
        );
    }

    #[test]
    fn activity_without_an_activity_epoch_later_is_below_minimum() {
        assert_eq!(
            ActivityResult::new(Epoch::new(3), None, None),
            ActivityResult::NotRequired(NotRequiredReason::NetworkBelowMinimum)
        );
    }

    #[test]
    fn activity_is_not_required_outside_the_activity_epoch() {
        assert_eq!(
            ActivityResult::new(
                Epoch::new(3),
                Some(ActivityEpoch {
                    epoch: Epoch::new(2),
                    in_activity_epoch: false,
                    proof_accepted: false
                }),
                None
            ),
            ActivityResult::NotRequired(NotRequiredReason::NotInActivityEpoch)
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
            ActivityResult::new(
                current_epoch,
                Some(ActivityEpoch {
                    epoch,
                    in_activity_epoch: true,
                    proof_accepted: true
                }),
                Some(report(epoch, submitted))
            ),
            ActivityResult::Accepted
        );
        assert_eq!(
            ActivityResult::new(
                current_epoch,
                Some(ActivityEpoch {
                    epoch,
                    in_activity_epoch: true,
                    proof_accepted: false
                }),
                Some(report(epoch, submitted))
            ),
            ActivityResult::Pending
        );
        assert_eq!(
            ActivityResult::new(
                current_epoch,
                Some(ActivityEpoch {
                    epoch,
                    in_activity_epoch: true,
                    proof_accepted: false
                }),
                Some(report(
                    epoch,
                    Err(ActivityProofSubmissionError::MempoolRejected)
                ))
            ),
            ActivityResult::Failed(FailureReason::PostFailed)
        );
        assert_eq!(
            ActivityResult::new(
                current_epoch,
                Some(ActivityEpoch {
                    epoch,
                    in_activity_epoch: true,
                    proof_accepted: false
                }),
                Some(report(Epoch::new(3), submitted))
            ),
            ActivityResult::Failed(FailureReason::NoProof)
        );
    }
}
