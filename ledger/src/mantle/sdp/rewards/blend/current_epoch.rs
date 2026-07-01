use std::collections::HashMap;

use lb_blend_crypto::merkle::sort_nodes_and_build_merkle_tree;
use lb_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    encap::ProofsVerifier as ProofsVerifierTrait, reward::EpochRandomness,
};
use lb_blend_proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs};
use lb_core::{
    crypto::ZkHash,
    mantle::Value,
    sdp::{Declaration, ProviderId, ServiceType},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;
use rpds::HashTrieMapSync;
use tracing::debug;

use crate::{
    EpochState,
    mantle::sdp::rewards::blend::{LOG_TARGET, RewardsParameters, target_epoch::TargetEpochState},
};

/// Immutable state of the current epoch.
/// The epoch is `E` if `E-1` is the target epoch for which rewards
/// are being calculated.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CurrentEpochState {
    epoch: Epoch,
    /// Current epoch randomness
    epoch_randomness: EpochRandomness,
    /// The leader input derived from the current epoch state.
    /// These will be used to create the proof verifier after the next
    /// epoch update.
    leader_input: LeaderInputs,
}

impl CurrentEpochState {
    pub fn new(epoch_state: &EpochState, settings: &RewardsParameters) -> Self {
        Self {
            epoch: epoch_state.epoch(),
            epoch_randomness: (*epoch_state.nonce()).into(),
            leader_input: settings.leader_inputs(epoch_state),
        }
    }

    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub const fn epoch_randomness(&self) -> EpochRandomness {
        self.epoch_randomness
    }
}

/// Collects income for the current epoch.
/// The current epoch is `E` if `E-1` is the target epoch for which rewards
/// are being calculated.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CurrentEpochTracker {
    /// Collecting service rewards over the epoch
    epoch_income: Value,
}

impl CurrentEpochTracker {
    pub fn new() -> Self {
        Self {
            epoch_income: Value::default(),
        }
    }

    pub(crate) const fn add_block_rewards(&self, block_rewards: Value) -> Self {
        Self {
            epoch_income: self.epoch_income + block_rewards,
        }
    }

    /// Finalizes the current epoch tracker.
    ///
    /// It returns [`CurrentEpochTrackerOutput::WithTargetEpoch`] by
    /// creating a [`TargetEpochState`] using the collected information,
    /// if the following conditions are met:
    /// - The network size of the new target epoch is not below the minimum.
    /// - No multi-epoch jump has occurred.
    ///
    /// Otherwise, it returns [`CurrentEpochTrackerOutput::WithoutTargetEpoch`].
    pub fn finalize<ProofsVerifier>(
        &self,
        current_reward_epoch_state: &CurrentEpochState,
        last_epoch_state: &EpochState,
        next_epoch_state: &EpochState,
        settings: &RewardsParameters,
    ) -> CurrentEpochTrackerOutput<ProofsVerifier>
    where
        ProofsVerifier: ProofsVerifierTrait,
    {
        assert_eq!(
            last_epoch_state.epoch,
            current_reward_epoch_state.epoch(),
            "last_epoch_state.epoch({}) must match current_reward_epoch_state.epoch({})",
            last_epoch_state.epoch,
            current_reward_epoch_state.epoch(),
        );
        assert!(
            last_epoch_state.epoch < next_epoch_state.epoch,
            "next_epoch_state.epoch({}) must be greater than last_epoch_state.epoch({})",
            next_epoch_state.epoch,
            last_epoch_state.epoch,
        );

        // On a multi-epoch jump, skip target epoch setup.
        // See the details in the [`Rewards::update_epoch`] documentation.
        if next_epoch_state.epoch > last_epoch_state.epoch.strict_add(1.into()) {
            debug!(
                target: LOG_TARGET,
                "Multi-epoch jump from {} to {}. Switching to WithoutTargetEpoch mode",
                last_epoch_state.epoch,
                next_epoch_state.epoch,
            );
            return CurrentEpochTrackerOutput::WithoutTargetEpoch {
                current_epoch_state: CurrentEpochState::new(next_epoch_state, settings),
                current_epoch_tracker: Self::new(),
            };
        }

        let maybe_declarations = last_epoch_state
            .active_declarations
            .for_service(&ServiceType::BlendNetwork);

        let declaration_count = maybe_declarations.map_or(0, HashMap::len);
        if declaration_count < settings.minimum_network_size.get() as usize {
            debug!(target: LOG_TARGET, "Declaration count({}) is below minimum network size({}). Switching to WithoutTargetEpoch mode",
                declaration_count,
                settings.minimum_network_size.get()
            );
            return CurrentEpochTrackerOutput::WithoutTargetEpoch {
                current_epoch_state: CurrentEpochState::new(next_epoch_state, settings),
                current_epoch_tracker: Self::new(),
            };
        }

        let (providers, zk_root) = Self::providers_and_zk_root(
            maybe_declarations
                .expect("declaration set must exist since it's larger than minimum network size")
                .values(),
        );

        let (core_quota, token_evaluation) = settings.core_quota_and_token_evaluation(
            providers.size() as u64,
        ).expect("evaluation parameters shouldn't overflow. panicking since we can't process the new epoch");

        let proof_verifier = Self::create_proof_verifier(
            current_reward_epoch_state.leader_input,
            zk_root,
            core_quota,
        );

        CurrentEpochTrackerOutput::WithTargetEpoch {
            target_epoch_state: TargetEpochState::new(
                last_epoch_state.epoch(),
                providers,
                token_evaluation,
                proof_verifier,
                self.epoch_income,
            ),
            current_epoch_state: CurrentEpochState::new(next_epoch_state, settings),
            current_epoch_tracker: Self::new(),
        }
    }

    #[expect(single_use_lifetimes, reason = "lifetime is required")]
    fn providers_and_zk_root<'d>(
        declarations: impl Iterator<Item = &'d Declaration>,
    ) -> (HashTrieMapSync<ProviderId, (ZkPublicKey, u64)>, ZkHash) {
        let mut providers = declarations
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

    fn create_proof_verifier<ProofsVerifier: ProofsVerifierTrait>(
        leader_input: LeaderInputs,
        zk_root: ZkHash,
        core_quota: u64,
    ) -> ProofsVerifier {
        ProofsVerifier::new(PoQVerificationInputsMinusSigningKey {
            core: CoreInputs {
                zk_root,
                quota: core_quota,
            },
            leader: leader_input,
        })
    }
}

/// Result of finalizing the [`CurrentEpochTracker`].
pub enum CurrentEpochTrackerOutput<ProofsVerifier> {
    /// Target epoch has been built with the information collected by
    /// the current epoch tracker.
    /// Also, the new current epoch state and tracker have been initialized.
    WithTargetEpoch {
        target_epoch_state: TargetEpochState<ProofsVerifier>,
        current_epoch_state: CurrentEpochState,
        current_epoch_tracker: CurrentEpochTracker,
    },
    /// No target epoch has been built because the network size in the
    /// epoch is below the minimum required.
    WithoutTargetEpoch {
        current_epoch_state: CurrentEpochState,
        current_epoch_tracker: CurrentEpochTracker,
    },
}
