use cryptarchia_engine::Epoch;
use key_management_system_keys::keys::ZkPublicKey;
use nomos_blend_crypto::merkle::sort_nodes_and_build_merkle_tree;
use nomos_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    encap::ProofsVerifier as ProofsVerifierTrait, reward::SessionRandomness,
};
use nomos_blend_proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs};
use nomos_core::{
    crypto::ZkHash,
    sdp::{ProviderId, SessionNumber},
};
use rpds::HashTrieMapSync;
use tracing::debug;

use crate::{
    EpochState,
    mantle::sdp::{
        SessionState,
        rewards::blend::{LOG_TARGET, RewardsParameters, target_session::TargetSessionState},
    },
};

/// Immutable state of the current session.
/// The current session is `s` if `s-1` is the target session for which rewards
/// are being calculated.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentSessionState {
    /// Current session randomness
    session_randomness: SessionRandomness,
}

impl CurrentSessionState {
    const fn new(session_randomness: SessionRandomness) -> Self {
        Self { session_randomness }
    }

    pub const fn session_randomness(&self) -> SessionRandomness {
        self.session_randomness
    }
}

/// Collects epoch states seen in the current session.
/// The current session is `s` if `s-1` is the target session for which rewards
/// are being calculated.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentSessionTracker {
    /// Collecting leader inputs derived from epoch states seen in the current
    /// session. These will be used to create proof verifiers after the next
    /// session update.
    leader_inputs: HashTrieMapSync<Epoch, LeaderInputs>,
}

impl CurrentSessionTracker {
    pub fn new(first_epoch_state: &EpochState, settings: &RewardsParameters) -> Self {
        Self {
            leader_inputs: std::iter::once((
                first_epoch_state.epoch,
                settings.leader_inputs(first_epoch_state),
            ))
            .collect(),
        }
    }

    pub fn collect_epoch(&self, epoch_state: &EpochState, settings: &RewardsParameters) -> Self {
        Self {
            leader_inputs: self
                .leader_inputs
                .insert(epoch_state.epoch, settings.leader_inputs(epoch_state)),
        }
    }

    /// Finalizes the current session tracker.
    ///
    /// It returns [`CurrentSessionTrackerOutput::WithTargetSession`] by
    /// creating a [`TargetSessionState`] using the collected information,
    /// if the network size of the new target session is not below the
    /// minimum required. Otherwise, it returns
    /// [`CurrentSessionTrackerOutput::WithoutTargetSession`].
    pub fn finalize<ProofsVerifier>(
        &self,
        last_active_session_state: &SessionState,
        next_session_first_epoch_state: &EpochState,
        settings: &RewardsParameters,
    ) -> CurrentSessionTrackerOutput<ProofsVerifier>
    where
        ProofsVerifier: ProofsVerifierTrait,
    {
        if last_active_session_state.declarations.size()
            < settings.minimum_network_size.get() as usize
        {
            debug!(target: LOG_TARGET, "Declaration count({}) is below minimum network size({}). Switching to WithoutTargetSession mode",
                last_active_session_state.declarations.size(),
                settings.minimum_network_size.get()
            );
            return CurrentSessionTrackerOutput::WithoutTargetSession(Self::new(
                next_session_first_epoch_state,
                settings,
            ));
        }

        let (providers, zk_root) = Self::providers_and_zk_root(last_active_session_state);

        let (core_quota, token_evaluation) = settings.core_quota_and_token_evaluation(
            providers.size() as u64,
        ).expect("evaluation parameters shouldn't overflow. panicking since we can't process the new session");

        let proof_verifiers = Self::create_proof_verifiers(
            self.leader_inputs.values().copied(),
            last_active_session_state.session_n,
            zk_root,
            core_quota,
        );

        CurrentSessionTrackerOutput::WithTargetSession {
            target_session_state: TargetSessionState::new(
                last_active_session_state.session_n,
                providers,
                token_evaluation,
                proof_verifiers,
            ),
            current_session_state: CurrentSessionState::new(SessionRandomness::new(
                last_active_session_state.session_n + 1,
                &next_session_first_epoch_state.nonce,
            )),
            current_session_tracker: Self::new(next_session_first_epoch_state, settings),
        }
    }

    fn providers_and_zk_root(
        session_state: &SessionState,
    ) -> (HashTrieMapSync<ProviderId, (ZkPublicKey, u64)>, ZkHash) {
        let mut providers = session_state
            .declarations
            .values()
            .map(|declaration| (declaration.provider_id, declaration.zk_id))
            .collect::<Vec<_>>();

        let zk_root =
            sort_nodes_and_build_merkle_tree(&mut providers, |(_, zk_id)| zk_id.into_inner())
                .expect("Should not fail to build merkle tree of core nodes' zk public keys")
                .root();

        let providers = providers
            .into_iter()
            .enumerate()
            .map(|(i, (provider_id, zk_id))| {
                (
                    provider_id,
                    (
                        zk_id,
                        u64::try_from(i).expect("provider index must fit in u64"),
                    ),
                )
            })
            .collect();

        (providers, zk_root)
    }

    fn create_proof_verifiers<ProofsVerifier: ProofsVerifierTrait>(
        leader_inputs: impl Iterator<Item = LeaderInputs>,
        session: SessionNumber,
        zk_root: ZkHash,
        core_quota: u64,
    ) -> Vec<ProofsVerifier> {
        leader_inputs
            .map(|leader| {
                ProofsVerifier::new(PoQVerificationInputsMinusSigningKey {
                    session,
                    core: CoreInputs {
                        zk_root,
                        quota: core_quota,
                    },
                    leader,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub fn epoch_count(&self) -> usize {
        self.leader_inputs.size()
    }
}

/// Result of finalizing the [`CurrentSessionTracker`].
pub enum CurrentSessionTrackerOutput<ProofsVerifier> {
    /// Target session has been built with the information collected by
    /// the current session tracker.
    /// Also, the new current session state and tracker have been initialized.
    WithTargetSession {
        target_session_state: TargetSessionState<ProofsVerifier>,
        current_session_state: CurrentSessionState,
        current_session_tracker: CurrentSessionTracker,
    },
    /// No target session has been built because the network size in the
    /// session is below the minimum required.
    WithoutTargetSession(CurrentSessionTracker),
}
