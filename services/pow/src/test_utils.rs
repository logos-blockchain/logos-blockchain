//! Test-only helpers shared by the crate's tests.

use std::{
    num::{NonZero, NonZeroU64},
    time::Duration,
};

use lb_core::{
    era::ForkDigest,
    mantle::gas::{Gas, GasProfile},
};
use lb_time::era::{EraEntriesAfterGenesis, EraSchedule, EraTiming};
use time::OffsetDateTime;

use crate::{PoWEraSettings, service::ServiceEraSchedule};

/// The number of slots in an epoch of the schedules [`era_schedule`] builds.
pub const SLOTS_PER_EPOCH: u64 = 100;

/// The size of a Blend payload body in the eras [`era_schedule`] builds: the
/// largest block proposal of an era that allows four uncles.
pub const PAYLOAD_BODY_SIZE: usize = 18_187;

/// The costs of the deployments' current gas profile.
pub const GAS_PROFILE: GasProfile = GasProfile {
    transfer: Gas::new(590),
    channel_config: Gas::new(56),
    channel_inscribe: Gas::new(56),
    channel_deposit: Gas::new(590),
    channel_withdraw: Gas::new(56),
    channel_transfer: Gas::new(56),
    sdp_declare: Gas::new(646),
    sdp_withdraw: Gas::new(590),
    sdp_active: Gas::new(590),
    leader_claim: Gas::new(580),
    claim_pow_reward: Gas::new(1),
};

/// A schedule with an era per reward window of `reward_windows`, era `i`
/// starting at epoch `i` with the reward window `reward_windows[i]`, rewards
/// enabled.
pub fn era_schedule(reward_windows: &[u64]) -> ServiceEraSchedule {
    let timing = EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: NonZero::new(SLOTS_PER_EPOCH).unwrap(),
    };
    let after_genesis =
        EraEntriesAfterGenesis::try_from_iter((1..reward_windows.len()).map(|first_epoch| {
            let first_epoch = NonZero::new(u32::try_from(first_epoch).unwrap()).unwrap();
            (first_epoch, timing)
        }))
        .unwrap();
    EraSchedule::new(OffsetDateTime::UNIX_EPOCH, timing, after_genesis)
        .unwrap()
        .map(|era| PoWEraSettings {
            slot_window: NonZeroU64::new(reward_windows[usize::from(era.era.into_inner())])
                .unwrap(),
            rewards_enabled: true,
            max_data_payload_body_size: PAYLOAD_BODY_SIZE,
            fork_digest: ForkDigest::new([0x11; 32]),
            gas_profile: GAS_PROFILE,
        })
}
