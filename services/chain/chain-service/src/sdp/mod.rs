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
use tracing::{error, info};

use crate::{LOG_TARGET, relays::BroadcastRelay};

/// Take/broadcast a SDP snapshot at the most recent block that is older
/// than both the LIB and the last block of epoch `epoch -
/// SNAPSHOT_FINALIZATION_DELAY`.
///
/// The caller supplies LIB's id, slot, and the in-memory declarations at LIB
/// (used on the fast path when LIB is already old enough). Storage is only
/// queried when the walk needs to go past LIB.
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
    info!(target: LOG_TARGET, ?epoch, ?snapshot_slot, "took/broadcasted SDP snapshot");
    Ok(snapshot)
}

async fn take_sdp_snapshot<Storage>(
    epoch: Epoch,
    lib_id: HeaderId,
    genesis_declarations: &Declarations,
    config: &lb_ledger::Config,
    storage: &Storage,
) -> Result<(Declarations, Slot), DynError>
where
    Storage: storage::Storage + Sync,
{
    // TODO: Because we're not storing genesis in storage,
    // I added an workaround that fetches declarations from memory
    // if LIB (e.g. genesis) is the snapshot block.
    // This workaround must be removed after storing genesis to storage: https://github.com/logos-blockchain/logos-blockchain/issues/2747
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
    while let Some((id, slot)) = chain.next().await {
        if config.epoch(slot) <= current_epoch.saturating_sub(SNAPSHOT_FINALIZATION_DELAY) {
            return Some((id, slot));
        }
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

                if let Err(e) = broadcast_blend_providers(relay, providers).await {
                    error!(target: LOG_TARGET, ?epoch, err = ?e, "Failed to broadcast a new blend SDP snapshot");
                }
            }
        }
    }
}

async fn broadcast_blend_providers(
    relay: &BroadcastRelay,
    providers: ActiveProviders,
) -> Result<(), DynError> {
    relay
        .send(BlockBroadcastMsg::BroadcastBlendProviders(providers))
        .await
        .map_err(|(error, _)| Box::new(error) as DynError)
}
