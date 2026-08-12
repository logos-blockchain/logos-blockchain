use core::{pin::Pin, task::Waker};
use std::sync::Arc;

use futures::stream::FuturesUnordered;
use lb_blend_message::encap::{
    ProofsVerifier,
    validated::{
        EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
    },
};
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::blend;
use lb_utils::tokio::task::spawn_blocking;
use libp2p::{PeerId, swarm::ConnectionId};

const LOG_TARGET: &str = blend::network::core::core::BEHAVIOUR;

/// The `PoQ` verifications a behaviour currently has running on the blocking
/// pool.
pub type PendingPoQVerifications =
    FuturesUnordered<Pin<Box<dyn Future<Output = PoQVerificationOutcome> + Send>>>;

/// The result of verifying the `PoQ` of a message received from a peer.
pub enum PoQVerificationOutcome {
    Verified {
        message: Box<EncapsulatedMessageWithVerifiedPublicHeader>,
        sender: PeerId,
        epoch: Epoch,
    },
    /// The sender could not produce a valid `PoQ`. The connection it came in on
    /// is reported so the behaviour can act on that peer.
    Failed {
        sender: PeerId,
        connection_id: ConnectionId,
    },
}

/// Dispatches the `PoQ` verification of a received message to the blocking
/// pool.
///
/// Verification is a Groth16 proof check. Running it inline would stall the
/// swarm task that drives this behaviour, which is also servicing every other
/// peer's stream, so the message is only acted upon once the outcome is polled
/// out of `pending_verifications`.
pub fn spawn_poq_verification<Verifier>(
    pending_verifications: &PendingPoQVerifications,
    message: EncapsulatedMessageWithVerifiedSignature,
    (sender, connection_id): (PeerId, ConnectionId),
    epoch: Epoch,
    verifier: &Arc<Verifier>,
    waker: &mut Option<Waker>,
) where
    Verifier: ProofsVerifier + Send + Sync + 'static,
{
    let verifier = Arc::clone(verifier);
    pending_verifications.push(Box::pin(async move {
        let verification_result = spawn_blocking("logos/blend/verify-poq", move || {
            // The verification error is rendered here because it is not `Send`, so it
            // cannot cross back to the task polling this behaviour as-is.
            message
                .verify_proof_of_quota(&*verifier)
                .map_err(|e| format!("{e:?}"))
        })
        .await;

        match verification_result {
            Ok(Ok(message)) => PoQVerificationOutcome::Verified {
                message: Box::new(message),
                sender,
                epoch,
            },
            Ok(Err(e)) => {
                tracing::debug!(target: LOG_TARGET, "PoQ verification failed for message received from peer {sender:?} for epoch {epoch:?}: {e}.");
                PoQVerificationOutcome::Failed {
                    sender,
                    connection_id,
                }
            }
            Err(e) => {
                tracing::error!(target: LOG_TARGET, "PoQ verification task for the message received from peer {sender:?} for epoch {epoch:?} failed to complete: {e:?}.");
                PoQVerificationOutcome::Failed {
                    sender,
                    connection_id,
                }
            }
        }
    }));
    if let Some(waker) = waker.take() {
        waker.wake();
    }
}
