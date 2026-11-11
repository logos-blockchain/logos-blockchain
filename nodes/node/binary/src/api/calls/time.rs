use lb_api_common::{EraTiming, TimeInfo};
use lb_core::era::ForkDigest;
use lb_time_service::TimeServiceMessage;

use super::{Error, Handle, request};
use crate::TimeService;

/// The fork digest, of `fork_digests`, of the era the time service's clock is
/// in.
pub async fn fork_digest_in_force(
    handle: &Handle,
    fork_digests: &[ForkDigest],
) -> Result<ForkDigest, Error> {
    let era =
        request::<TimeService, _>(handle, |sender| TimeServiceMessage::CurrentSlot { sender })
            .await?
            .era;
    fork_digests
        .get(usize::from(era.into_inner()))
        .copied()
        .ok_or_else(|| Error::internal(format!("The deployment has no era {}", era.into_inner())))
}

pub async fn info(handle: &Handle) -> Result<TimeInfo, Error> {
    let info =
        request::<TimeService, _>(handle, |sender| TimeServiceMessage::Info { sender }).await?;
    Ok(TimeInfo {
        genesis_time: info.genesis_time().to_utc(),
        current_slot: u64::from(info.current_slot),
        current_epoch: u32::from(info.current_epoch),
        era_timings: info
            .era_schedules
            .iter()
            .map(|era| {
                (
                    era.first_epoch.into_inner(),
                    EraTiming {
                        slot_duration: era.parameters.slot_duration,
                        slots_per_epoch: era.parameters.epoch_length,
                    },
                )
            })
            .collect(),
    })
}
