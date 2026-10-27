use std::num::NonZeroU32;

use lb_cryptarchia_engine::max_allowed_uncle_reference_window_in_block;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
use serde::Deserialize;

use crate::config::deployment::era::ruleset::v1::cryptarchia::{
    EpochConfig, PoWConfig, SdpConfig, Settings,
};

/// Cryptarchia settings as read from a config, before their invariants are
/// checked.
#[derive(Deserialize)]
pub struct UnvalidatedSettings {
    pub epoch_config: EpochConfig,
    pub security_param: NonZeroU32,
    pub slot_activation_coeff: NonNegativeRatio,
    pub learning_rate: NonNegativeF64,
    pub uncle_reference_window_in_block: NonZeroU32,
    pub sdp_config: SdpConfig,
    pub pow_config: PoWConfig,
}

/// Convert [`UnvalidatedSettings`] into [`Settings`] with invariant checks.
impl TryFrom<UnvalidatedSettings> for Settings {
    type Error = UncleReferenceWindowTooWide;

    fn try_from(unvalidated: UnvalidatedSettings) -> Result<Self, Self::Error> {
        let max = max_allowed_uncle_reference_window_in_block(unvalidated.security_param);
        if unvalidated.uncle_reference_window_in_block.get() > max {
            return Err(UncleReferenceWindowTooWide {
                security_param: unvalidated.security_param,
                window: unvalidated.uncle_reference_window_in_block,
                max,
            });
        }

        Ok(Self {
            epoch_config: unvalidated.epoch_config,
            security_param: unvalidated.security_param,
            slot_activation_coeff: unvalidated.slot_activation_coeff,
            learning_rate: unvalidated.learning_rate,
            uncle_reference_window_in_block: unvalidated.uncle_reference_window_in_block,
            sdp_config: unvalidated.sdp_config,
            pow_config: unvalidated.pow_config,
        })
    }
}

#[cfg(any(test, feature = "testing"))]
impl From<Settings> for UnvalidatedSettings {
    fn from(settings: Settings) -> Self {
        Self {
            epoch_config: settings.epoch_config,
            security_param: settings.security_param,
            slot_activation_coeff: settings.slot_activation_coeff,
            learning_rate: settings.learning_rate,
            uncle_reference_window_in_block: settings.uncle_reference_window_in_block,
            sdp_config: settings.sdp_config,
            pow_config: settings.pow_config,
        }
    }
}

/// The uncle reference window `W` exceeds `⌊0.6k⌋`, so an uncle's parent
/// could lie below the latest immutable block.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error(
    "uncle reference window ({window} blocks) must not exceed ⌊0.6k⌋ = {max} blocks \
     for security parameter k = {security_param}"
)]
pub struct UncleReferenceWindowTooWide {
    pub security_param: NonZeroU32,
    pub window: NonZeroU32,
    pub max: u32,
}

#[cfg(test)]
mod tests {
    use crate::config::DeploymentSettings;

    use super::*;

    #[test]
    fn uncle_reference_window_accepted() {
        Settings::try_from(unvalidated(10, 20, 6)).unwrap();
    }

    #[test]
    fn uncle_reference_window_rejected() {
        assert_eq!(
            Settings::try_from(unvalidated(10, 20, 7)).unwrap_err(),
            UncleReferenceWindowTooWide {
                security_param: 10.try_into().unwrap(),
                window: 7.try_into().unwrap(),
                max: 6,
            }
        );
    }

    /// The default settings with `k`, `f = 1/f_denominator` and `W` replaced.
    fn unvalidated(security_param: u32, f_denominator: u32, window: u32) -> UnvalidatedSettings {
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

        unvalidated
    }
}
