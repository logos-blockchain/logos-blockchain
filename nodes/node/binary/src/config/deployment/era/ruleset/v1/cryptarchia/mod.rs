use core::num::{NonZero, NonZeroU32, NonZeroU64};

use lb_binary_codec::canonical::BTreeMap;
use lb_chain_service::Epoch;
use lb_core::sdp::{InactivityPeriod, MinStake, ServiceType};
use lb_cryptarchia_engine::{
    Config as ConsensusConfig, average_slots_for_blocks, base_period_length,
    expected_blocks_per_epoch, time::epoch_length,
};
use lb_groth16::ModulusShift;
use lb_ledger::cryptarchia::STAKE_INFERENCE_PRECISION;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator as _;

pub(crate) mod codec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub epoch_config: EpochConfig,
    pub security_param: NonZeroU32,
    pub slot_activation_coeff: NonNegativeRatio,
    pub learning_rate: NonNegativeF64,
    /// `W`, the uncle reference window in expected block-intervals.
    pub uncle_reference_window_in_block: u32,
    pub sdp_config: SdpConfig,
    pub pow_config: PoWConfig,
}

impl Settings {
    /// Checks the cryptarchia settings of an era, alone and against those of
    /// the era before it, `previous`, if there is one, panicking on the first
    /// value found invalid.
    pub(super) fn validate(&self, previous: Option<&Self>) {
        let NonNegativeRatio {
            numerator,
            denominator,
        } = self.slot_activation_coeff;
        // Below 1, the lottery's `ln(1 - f)` is finite. At least the inverse of
        // the precision, the expected density the total stake inference
        // divides by is not zero.
        assert!(
            numerator < denominator.get()
                && (self.slot_activation_coeff.as_f64() * STAKE_INFERENCE_PRECISION as f64).trunc()
                    >= 1.0,
            "The slot activation coefficient {numerator}/{denominator} must be below 1, and at \
             least 1/{STAKE_INFERENCE_PRECISION}, the precision the total stake inference \
             measures it with."
        );
        // Panics if `⌊k/4f⌋` leaves the Genesis density window no slot: now
        // rather than at the first fork choice.
        let _ = self.consensus_config().s_gen();

        let window = self.uncle_reference_window_in_block;
        let widest = self.widest_uncle_reference_window();
        assert!(
            window <= widest,
            "The uncle reference window of {window} blocks is wider than ⌊0.6k⌋, {widest} blocks."
        );
        // While the latest immutable block still lies in the previous era, an
        // uncle's parent must not lie below it.
        if let Some(previous) = previous {
            let widest = self.widest_uncle_reference_window_after(previous);
            assert!(
                window <= widest,
                "The uncle reference window of {window} blocks is wider than the {widest} that a \
                 fifth of the previous era's finalization window allows."
            );
        }

        let BlendPoWConfig {
            damping_num,
            damping_den_offset,
            ..
        } = self.pow_config.blend;
        assert!(
            damping_num.checked_add(damping_den_offset).is_some(),
            "The denominator of the Blend PoW damping exponent, {damping_num} + \
             {damping_den_offset}, must fit in a u32."
        );
        self.pow_config
            .reward
            .validate()
            .unwrap_or_else(|error| panic!("Invalid PoW reward parameters: {error}."));

        for service_type in ServiceType::iter() {
            assert!(
                self.sdp_config.service_params.contains_key(&service_type),
                "The SDP service parameters must include those of service type {}.",
                AsRef::<str>::as_ref(&service_type)
            );
        }
    }

    /// `⌊0.6k⌋`: the widest uncle reference window `W` the era allows, in
    /// blocks, whose `W·f⁻¹` slots then span about a fifth of the slots a block
    /// takes to become immutable.
    #[must_use]
    pub const fn widest_uncle_reference_window(&self) -> u32 {
        (3 * self.security_param.get() as u64 / 5) as u32
    }

    /// `⌊s·f/5⌋`: the widest uncle reference window `W`, in blocks, whose
    /// `W·f⁻¹` slots span at most a fifth of the finalization window `s` of
    /// the era before, `previous`.
    fn widest_uncle_reference_window_after(&self, previous: &Self) -> u32 {
        let NonNegativeRatio {
            numerator,
            denominator,
        } = self.slot_activation_coeff;
        let widest = u128::from(previous.finalization_window()) * u128::from(numerator)
            / (5 * u128::from(denominator.get()));
        u32::try_from(widest).unwrap_or(u32::MAX)
    }

    /// `s = 3⌊k/f⌋`: the slots a block takes to become immutable.
    const fn finalization_window(&self) -> u64 {
        base_period_length(self.security_param, self.slot_activation_coeff)
            .get()
            .saturating_mul(3)
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
