mod storage;
#[cfg(test)]
mod tests;

use futures::StreamExt as _;
use lb_chain_broadcast_service::{ActiveProviders, BlockBroadcastMsg};
use lb_core::{
    header::HeaderId,
    sdp::{Declarations, ProviderInfo, ServiceType},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_ledger::mantle::sdp::SNAPSHOT_FINALIZATION_DELAY;
use overwatch::DynError;
use tracing::{debug, trace};

use crate::{LOG_TARGET, relays::BroadcastRelay};

/// Take/broadcast a SDP snapshot for the current `epoch`.
pub async fn take_and_broadcast_sdp_snapshot<Storage>(
    epoch: Epoch,
    lib_id: HeaderId,
    genesis_declarations: &Declarations,
    config: &lb_ledger::Config,
    storage: &Storage,
    broadcast_relay: &BroadcastRelay,
) -> Result<Declarations, DynError>
where
    Storage: storage::Storage + Sync,
{
    let (snapshot, snapshot_slot) =
        take_sdp_snapshot(epoch, lib_id, genesis_declarations, config, storage).await?;
    broadcast_sdp_snapshot(epoch, &snapshot, broadcast_relay).await;
    debug!(target: LOG_TARGET, ?epoch, ?snapshot_slot, "took/broadcasted SDP snapshot");
    Ok(snapshot)
}

/// Take a SDP snapshot for the current `epoch`.
async fn take_sdp_snapshot<Storage>(
    epoch: Epoch,
    lib_id: HeaderId,
    // TODO: remove this after persisting genesis declarations to storage
    genesis_declarations: &Declarations,
    config: &lb_ledger::Config,
    storage: &Storage,
) -> Result<(Declarations, Slot), DynError>
where
    Storage: storage::Storage + Sync,
{
    if epoch == 0.into() || epoch == 1.into() {
        return Ok((genesis_declarations.clone(), Slot::genesis()));
    }

    let Some((snapshot_id, snapshot_slot)) =
        find_sdp_snapshot_block(epoch, lib_id, config, storage).await
    else {
        return Ok((genesis_declarations.clone(), Slot::genesis()));
    };
    let snapshot = storage
        .sdp_declarations_at(snapshot_id)
        .await?
        .expect("SDP declarations must exist in storage");
    Ok((snapshot, snapshot_slot))
}

/// Take a SDP snapshot for `current_epoch` at the last block of an epoch
/// - which is <= `current_epoch - SNAPSHOT_FINALIZATION_DELAY`
/// - which is older than LIB.
///
/// If LIB is the 'current' last block of an epoch, and if there are still
/// some newer slots in the epoch (i.e. if LIB is not on the last slot of the epoch),
/// skip the epoch because newer blocks may be added to the epoch later.
async fn find_sdp_snapshot_block<Storage>(
    current_epoch: Epoch,
    lib_id: HeaderId,
    config: &lb_ledger::Config,
    storage: &Storage,
) -> Option<(HeaderId, Slot)>
where
    Storage: storage::Storage + Sync,
{
    let mut chain = storage.block_ids(lib_id).await;
    let mut prev_epoch: Option<Epoch> = None;

    while let Some((id, slot)) = chain.next().await {
        let epoch = config.epoch(slot);
        let is_last_block_of_epoch = match prev_epoch {
            Some(prev_epoch) => epoch != prev_epoch, // just crossed an epoch boundary
            None => slot == config.last_slot(epoch), // block is on the last slot of its epoch
        };

        if is_last_block_of_epoch
            && epoch <= current_epoch.saturating_sub(SNAPSHOT_FINALIZATION_DELAY)
        {
            return Some((id, slot));
        }
        prev_epoch = Some(epoch);
    }
    None
}

async fn broadcast_sdp_snapshot(epoch: Epoch, snapshot: &Declarations, relay: &BroadcastRelay) {
    for (service_type, declarations) in snapshot.iter() {
        match service_type {
            ServiceType::BlendNetwork => {
                let providers = ActiveProviders {
                    epoch,
                    providers: declarations
                        .values()
                        .map(|declaration| {
                            (
                                declaration.provider_id,
                                ProviderInfo {
                                    locators: declaration.locators.clone(),
                                    zk_id: declaration.zk_id,
                                },
                            )
                        })
                        .collect(),
                };

                broadcast_blend_providers(relay, providers).await;
            }
        }
    }
}

async fn broadcast_blend_providers(relay: &BroadcastRelay, providers: ActiveProviders) {
    if let Err((err, _)) = relay
        .send(BlockBroadcastMsg::BroadcastBlendProviders(providers))
        .await
    {
        trace!(target: LOG_TARGET, ?err, "Failed to broadcast a new blend SDP snapshot");
    }
}
