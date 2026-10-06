use std::sync::Arc;

use futures::StreamExt as _;
use lb_chain_service::{
    Slot,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_cryptarchia_engine::era::EraSchedule;
use lb_cryptarchia_sync::{GetTipResponse, HeaderId};
use lb_time_service::SlotTick;
use overwatch::DynError;
use tracing::{debug, warn};

use crate::{TipPollConfig, metrics, network::NetworkAdapter, sync::LOG_TARGET};

/// Proactive tip-poll lag watchdog.
///
/// Fires on a slot-tick cadence (≈ one expected block interval in the era of
/// the tick). When the local tip has fallen behind the current slot by more
/// than the lag threshold of that era, it samples a handful of
/// peers with `GetTip` and returns the most-advanced tip that is strictly
/// ahead of the local height. The caller is expected to hand it to the
/// orphan downloader, which performs the actual catch-up download and
/// full validation.
///
/// Polled tips are unauthenticated peer hearsay, but enqueuing one only
/// triggers a download: every downloaded block still goes through normal
/// `apply_block` validation, and a bogus tip is dropped on download
/// failure.
pub async fn poll_peer_tips_if_behind<NetAdapter, Cryptarchia, RuntimeServiceId>(
    network_adapter: &NetAdapter,
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    tick: SlotTick,
    params: &TipPollParams,
) -> Option<PolledTip>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Sync,
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: Send,
    RuntimeServiceId: Send + Sync + 'static,
{
    // Cadence gate: only act roughly once per expected block interval.
    let (cadence_slots, lag_threshold_slots) = params.at_slot(tick.slot);
    let current_slot = u64::from(tick.slot);
    if current_slot % cadence_slots != 0 {
        return None;
    }

    let info = lagging_local_info(cryptarchia, current_slot, lag_threshold_slots).await?;
    metrics::tip_poll_triggered_total();

    let tips: Vec<GetTipResponse> = network_adapter
        .sample_tips(params.max_peers)
        .await
        .collect()
        .await;

    // Pick the most-advanced reported tip that is strictly ahead of us.
    let Some((height, _, tip)) = select_catchup_tip(tips, info.height) else {
        debug!(
            target: LOG_TARGET,
            local_height = info.height,
            "tip poll: no sampled peer reported a tip ahead of us"
        );
        return None;
    };

    Some(PolledTip {
        tip,
        height,
        local: info,
    })
}

/// Read the local chain info and return it only if the tip is lagging the
/// current slot by more than `lag_threshold_slots`. Returns `None` (and
/// logs) when the info can't be read or the node is keeping up.
async fn lagging_local_info<Cryptarchia>(
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    current_slot: u64,
    lag_threshold_slots: u64,
) -> Option<lb_chain_service::CryptarchiaInfo>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: Send,
{
    let info = match cryptarchia.info().await {
        Ok(info) => info.cryptarchia_info,
        Err(e) => {
            warn!(target: LOG_TARGET, %e, "tip poll: failed to read local chain info");
            return None;
        }
    };

    let tip_slot = u64::from(info.slot);
    let lag = current_slot.saturating_sub(tip_slot);
    if lag <= lag_threshold_slots {
        return None;
    }

    debug!(
        target: LOG_TARGET,
        lag, tip_slot, current_slot,
        "Chain tip lagging; polling peers for their tip"
    );
    Some(info)
}

/// From a set of polled tip responses, pick the most-advanced tip that is
/// strictly ahead of `local_height`. Ties on height are broken by the higher
/// slot. `Failure` responses are ignored. Returns `(height, slot, tip)`.
fn select_catchup_tip(
    tips: impl IntoIterator<Item = GetTipResponse>,
    local_height: u64,
) -> Option<(u64, Slot, HeaderId)> {
    tips.into_iter()
        .filter_map(|resp| match resp {
            GetTipResponse::Tip { tip, slot, height } => Some((height, slot, tip)),
            GetTipResponse::Failure(reason) => {
                debug!(target: LOG_TARGET, %reason, "tip poll: peer reported failure");
                None
            }
        })
        .filter(|(height, _, _)| *height > local_height)
        .max_by_key(|(height, slot, _)| (*height, u64::from(*slot)))
}

pub struct PolledTip {
    pub tip: HeaderId,
    pub height: u64,
    pub local: lb_chain_service::CryptarchiaInfo,
}

/// Derived parameters for the proactive tip-poll lag watchdog.
#[derive(Clone)]
pub struct TipPollParams {
    /// The cadence of each era: act every this many slots (≈ one expected
    /// block interval, `1/f`).
    cadence_slots: Arc<EraSchedule<u64>>,
    /// Lag, in expected block intervals, beyond which we proactively poll
    /// peers for their tip.
    pub lag_threshold_blocks: u64,
    /// Maximum number of peers to sample with `GetTip` per poll.
    pub max_peers: usize,
}

impl TipPollParams {
    /// Derive the polling cadence and lag threshold of each era from its
    /// active slot coefficient `f`. The expected number of slots between
    /// blocks is `1/f`, so the cadence is `ceil(1/f)` slots and the lag
    /// threshold is `lag_threshold_blocks` such intervals. Every era must
    /// allow polling, checked once here.
    pub async fn derive<Cryptarchia>(
        config: &TipPollConfig,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) -> Result<Self, DynError>
    where
        Cryptarchia: CryptarchiaServiceData<Tx: Send>,
    {
        let ledger_eras = cryptarchia
            .get_ledger_eras()
            .await
            .map_err(|e| DynError::from(format!("failed to fetch the ledger eras: {e}")))?;
        if ledger_eras
            .iter()
            .any(|era| cadence_slots(era.entry.parameters.consensus_config()).is_none())
        {
            return Err(DynError::from(
                "active slot coefficient f must be > 0 in every era for tip polling",
            ));
        }

        Ok(Self {
            cadence_slots: Arc::new(ledger_eras.map(|era| {
                cadence_slots(era.entry.parameters.consensus_config())
                    .expect("every era was checked to have a cadence")
            })),
            lag_threshold_blocks: config.lag_threshold_blocks.get(),
            max_peers: config.max_peers_to_sample.get(),
        })
    }

    /// The cadence and the lag threshold, in slots, in the era of `slot`.
    fn at_slot(&self, slot: Slot) -> (u64, u64) {
        let cadence_slots = self.cadence_slots.at_slot(slot).entry.parameters;
        (
            cadence_slots,
            cadence_slots.saturating_mul(self.lag_threshold_blocks),
        )
    }
}

/// `ceil(1/f)`, the expected number of slots between blocks under
/// `consensus_config`. `None` when `f` is zero.
fn cadence_slots(consensus_config: &lb_cryptarchia_engine::Config) -> Option<u64> {
    // `f = numerator / denominator`, so `1/f = denominator / numerator`.
    // Compute `ceil(1/f)` with integer arithmetic to avoid float casts.
    let coeff = consensus_config.slot_activation_coeff();
    let numerator = u64::from(coeff.numerator);
    let denominator = core::num::NonZeroU64::from(coeff.denominator).get();
    (numerator != 0).then(|| denominator.div_ceil(numerator).max(1))
}

#[cfg(test)]
mod tests {
    use core::num::NonZero;

    use lb_cryptarchia_sync::GetTipResponseReason;
    use lb_utils::math::NonNegativeRatio;

    use super::*;

    fn consensus_config(f_numerator: u32, f_denominator: u32) -> lb_cryptarchia_engine::Config {
        lb_cryptarchia_engine::Config::new(
            NonZero::new(10).unwrap(),
            NonNegativeRatio::new(f_numerator, f_denominator.try_into().unwrap()),
            1f64.try_into().unwrap(),
            NonZero::new(12).unwrap(),
        )
    }

    #[test]
    fn the_cadence_is_one_expected_block_interval() {
        assert_eq!(cadence_slots(&consensus_config(1, 10)), Some(10));
        assert_eq!(cadence_slots(&consensus_config(1, 2)), Some(2));
        // `ceil(3/2)`.
        assert_eq!(cadence_slots(&consensus_config(2, 3)), Some(2));
    }

    fn tip(height: u64, slot: u64, id: u8) -> GetTipResponse {
        GetTipResponse::Tip {
            tip: HeaderId::from([id; 32]),
            slot: Slot::new(slot),
            height,
        }
    }

    #[test]
    fn select_catchup_tip_ignores_tips_at_or_below_local_height() {
        let tips = vec![tip(10, 100, 1), tip(9, 200, 2)];
        // local height is 10: nothing strictly ahead.
        assert!(select_catchup_tip(tips, 10).is_none());
    }

    #[test]
    fn select_catchup_tip_picks_highest_then_breaks_ties_by_slot() {
        let tips = vec![
            tip(12, 100, 1),
            tip(15, 300, 2), // highest height
            tip(15, 400, 3), // same height, higher slot -> winner
            tip(14, 999, 4),
        ];
        let (height, slot, id) = select_catchup_tip(tips, 11).expect("a tip ahead exists");
        assert_eq!(height, 15);
        assert_eq!(slot, Slot::new(400));
        assert_eq!(id, HeaderId::from([3; 32]));
    }

    #[test]
    fn select_catchup_tip_skips_failures() {
        let tips = vec![
            GetTipResponse::Failure(GetTipResponseReason::NodeNotOnline),
            tip(20, 500, 7),
            GetTipResponse::Failure(GetTipResponseReason::NodeNotOnline),
        ];
        let (height, _, id) = select_catchup_tip(tips, 5).expect("a tip ahead exists");
        assert_eq!(height, 20);
        assert_eq!(id, HeaderId::from([7; 32]));
    }

    #[test]
    fn select_catchup_tip_empty_is_none() {
        assert!(select_catchup_tip(Vec::new(), 0).is_none());
    }
}
