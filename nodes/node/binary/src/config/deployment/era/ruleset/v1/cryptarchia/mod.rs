use core::num::{NonZero, NonZeroU32, NonZeroU64};

use lb_binary_codec::canonical::BTreeMap;
use lb_chain_service::Epoch;
use lb_core::sdp::{InactivityPeriod, MinStake, ServiceType};
use lb_cryptarchia_engine::{
    Config as ConsensusConfig, average_slots_for_blocks, base_period_length,
    expected_blocks_per_epoch, time::epoch_length,
};
use lb_groth16::ModulusShift;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
use serde::{Deserialize, Serialize};

pub(crate) mod codec;
mod unvalidated;

pub use self::unvalidated::{UncleReferenceWindowTooWide, UnvalidatedSettings};

/// Cryptarchia settings.
///
/// Use [`UnvalidatedSettings::try_into`] to build this with
/// invariant checks.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(try_from = "UnvalidatedSettings")]
pub struct Settings {
    epoch_config: EpochConfig,
    security_param: NonZeroU32,
    slot_activation_coeff: NonNegativeRatio,
    learning_rate: NonNegativeF64,
    /// `W`, the uncle reference window in expected block-intervals.
    uncle_reference_window_in_block: NonZeroU32,
    sdp_config: SdpConfig,
    pow_config: PoWConfig,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error(
        "uncle reference window ({window} slots) must not exceed a fifth of the slot \
         security parameter s = {previous_slot_security_param} of the era before"
    )]
    UncleReferenceWindowBeyondPreviousSlotSecurityParam {
        window: u64,
        previous_slot_security_param: u64,
    },
}

impl Settings {
    /// Check the invariants between these settings and those of the next era.
    pub fn validate_successor(&self, next: &Self) -> Result<(), Error> {
        // Check `5⌊W_n/f_n⌋ ≤ s_{n−1}`
        let window = next
            .consensus_config()
            .uncle_reference_window_in_slot()
            .get();
        let previous_slot_security_param = self.consensus_config().slot_security_param().get();
        if u128::from(window) * 5 > u128::from(previous_slot_security_param) {
            return Err(Error::UncleReferenceWindowBeyondPreviousSlotSecurityParam {
                window,
                previous_slot_security_param,
            });
        }
        Ok(())
    }

    #[must_use]
    pub const fn slots_per_epoch(&self) -> u64 {
        epoch_length(
            self.epoch_config.epoch_stake_distribution_stabilization,
            self.epoch_config.epoch_period_nonce_buffer,
            self.epoch_config.epoch_period_nonce_stabilization,
            base_period_length(self.security_param, self.slot_activation_coeff),
        )
    }

    #[must_use]
    pub const fn average_slots_per_block(&self) -> u64 {
        average_slots_for_blocks(
            NonZero::<u32>::new(1).expect("must be non-zero"),
            self.slot_activation_coeff,
        )
        .get()
    }

    /// `N_b`: the number of blocks an epoch is expected to produce, derived
    /// from the schedule rather than configured. The `PoW` payout rate is
    /// denominated in it — see `lb_ledger::config::Config`.
    #[must_use]
    pub const fn expected_blocks_per_epoch(&self) -> NonZeroU64 {
        expected_blocks_per_epoch(self.slots_per_epoch(), self.slot_activation_coeff)
    }

    #[must_use]
    pub fn consensus_config(&self) -> ConsensusConfig {
        ConsensusConfig::new(
            self.security_param,
            self.slot_activation_coeff,
            self.learning_rate,
            self.uncle_reference_window_in_block,
        )
    }

    #[must_use]
    pub const fn security_param(&self) -> NonZeroU32 {
        self.security_param
    }

    #[must_use]
    pub const fn slot_activation_coeff(&self) -> NonNegativeRatio {
        self.slot_activation_coeff
    }

    /// `W`, the uncle reference window in expected block-intervals.
    #[must_use]
    pub const fn uncle_reference_window_in_block(&self) -> NonZeroU32 {
        self.uncle_reference_window_in_block
    }

    #[must_use]
    pub const fn epoch_config(&self) -> &EpochConfig {
        &self.epoch_config
    }

    #[must_use]
    pub const fn sdp_config(&self) -> &SdpConfig {
        &self.sdp_config
    }

    #[must_use]
    pub const fn pow_config(&self) -> &PoWConfig {
        &self.pow_config
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochConfig {
    // The stake distribution is always taken at the beginning of the previous epoch.
    // This parameters controls how many slots to wait for it to be stabilized
    // The value is computed as epoch_stake_distribution_stabilization * int(floor(k / f))
    pub epoch_stake_distribution_stabilization: NonZero<u8>,
    // This parameter controls how many slots we wait after the stake distribution
    // snapshot has stabilized to take the nonce snapshot.
    pub epoch_period_nonce_buffer: NonZero<u8>,
    // This parameter controls how many slots we wait for the nonce snapshot to be considered
    // stabilized
    pub epoch_period_nonce_stabilization: NonZero<u8>,
}

// The same as `lb_ledger::mantle::sdp::Config`, minus the
// `service_rewards_params` values, which are taken from the Blend deployment
// config instead.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SdpConfig {
    /// Ordered by service type, which is the order the canonical encoding lists
    /// them in.
    pub service_params: BTreeMap<ServiceType, ServiceParameters>,
    pub min_stake: MinStake,
}

// The same as `lb_core::sdp::ServiceParameters`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServiceParameters {
    pub inactivity_period: InactivityPeriod,
    pub epoch: Epoch,
}

// The same as `lb_ledger::config::PoWConfig`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PoWConfig {
    pub blend: BlendPoWConfig,
    pub reward: RewardPoWConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BlendPoWConfig {
    pub base_difficulty: ModulusShift,
    pub target_transactions_per_block: NonZeroU64,
    pub max_step: NonZeroU64,
    pub damping_num: NonZeroU32,
    pub damping_den_offset: u32,
}

// The reward parameters are used verbatim, so the ledger type is reused
// directly: deserializing the deployment config runs its invariant checks
// (`RewardPoWConfig::validate`), rejecting an invalid config at load time.
pub use lb_ledger::config::RewardPoWConfig;

#[cfg(test)]
mod tests {
    use lb_utils::math::NonNegativeRatio;

    use super::{Error, Settings, UncleReferenceWindowTooWide, UnvalidatedSettings};
    use crate::config::DeploymentSettings;

    #[test]
    fn uncle_reference_window_accepted() {
        let settings = settings(10, 20, 6);

        let yaml = serde_yaml::to_string(&settings).unwrap();
        assert!(serde_yaml::from_str::<Settings>(&yaml).is_ok());
    }

    #[test]
    fn uncle_reference_window_rejected() {
        // Raise `W` past the bound in the YAML of valid settings.
        let mut yaml = serde_yaml::to_value(settings(10, 20, 6)).unwrap();
        yaml["uncle_reference_window_in_block"] = 7.into();
        let error = serde_yaml::from_value::<Settings>(yaml).unwrap_err();
        assert!(
            error.to_string().contains(
                &UncleReferenceWindowTooWide {
                    security_param: 10.try_into().unwrap(),
                    window: 7.try_into().unwrap(),
                    max: 6,
                }
                .to_string()
            ),
            "{error}"
        );
    }

    #[test]
    fn next_era_uncle_reference_window_accepted() {
        let previous = settings(10, 20, 6);
        let next = settings(30, 20, 6);
        // `W` is accepted because <= s/5.
        assert_eq!(previous.validate_successor(&next), Ok(()));
    }

    #[test]
    fn next_era_uncle_reference_window_rejected() {
        // prev_slot_security_param: `s = 3*⌊10*20⌋ = 600` slots
        let previous = settings(10, 20, 6);
        // new_uncle_ref_window: `W = 7` (= 140 slot)
        let next = settings(30, 20, 7);
        // `W` is rejected because > s/5.
        assert_eq!(
            previous.validate_successor(&next),
            Err(Error::UncleReferenceWindowBeyondPreviousSlotSecurityParam {
                window: 140,
                previous_slot_security_param: 600,
            })
        );
    }

    /// The default settings with `k`, `f = 1/f_denominator` and `W` replaced.
    fn settings(security_param: u32, f_denominator: u32, window: u32) -> Settings {
        let mut unvalidated = UnvalidatedSettings::from(
            DeploymentSettings::default()
                .genesis_era_parameters()
                .cryptarchia
                .clone(),
        );
        unvalidated.security_param = security_param.try_into().unwrap();
        unvalidated.slot_activation_coeff =
            NonNegativeRatio::new(1, f_denominator.try_into().unwrap());
        unvalidated.uncle_reference_window_in_block = window.try_into().unwrap();

        unvalidated.try_into().unwrap()
    }
}
