//! Utility functions for the banning subsystem, providing a bridge/interface
//! between synchronous callers and the asynchronous banning service.

use std::time::Duration;

use libp2p::PeerId;
use overwatch::services::relay::OutboundRelay;
use tokio::{
    runtime::RuntimeFlavor,
    sync::{broadcast, oneshot},
    time::timeout,
};

use crate::{
    BanStatus, BanningRequest,
    types::{BanningEvent, Violation},
};

const TIMEOUT_WAITING_FOR_SERVICE: Duration = Duration::from_secs(2);

// /// Checks if a peer is currently banned by querying the banning subsystem
// via the provided /// `banning_relay`. If `peer_id` is `None`, returns
// `false`. #[allow(clippy::must_use_candidate)]
// pub fn banning_is_banned(
//     banning_relay: &Option<OutboundRelay<BanningRequest>>,
//     peer_id: Option<&PeerId>,
// ) -> bool {
//     if let Some(banning_relay) = banning_relay
//         && let Some(peer_id) = peer_id
//     {
//         return block_on_now(async {
//             let (tx, rx) = oneshot::channel();
//             // Treat failures as "not banned" (service is going down or relay
//             // closed)
//             if banning_relay
//                 .send(BanningRequest::GetBanState {
//                     peer_id: *peer_id,
//                     reply: tx,
//                 })
//                 .await
//                 .is_err()
//             {
//                 return false;
//             }
//             let ban_status = timeout(TIMEOUT_WAITING_FOR_SERVICE, rx)
//                 .await
//                 .ok() // timeout -> None
//                 .and_then(Result::ok) // channel closed -> None
//                 .unwrap_or(BanStatus::NotBanned);
//             matches!(ban_status, BanStatus::Banned { .. })
//         })
//         .unwrap_or_else(Err);
//     }
//     false
// }

/// Report a banning violation to get a peer banned using the banning subsystem
/// via the provided `banning_relay`. If `peer_id` is `None`, returns `false`.
pub fn banning_ban_peer(
    banning_relay: &Option<OutboundRelay<BanningRequest>>,
    violation: Violation,
) -> Result<bool, overwatch::DynError> {
    if let Some(banning_relay) = banning_relay {
        return block_on_now_from_sync(async {
            let (tx, rx) = oneshot::channel();
            if let Err(err) = banning_relay
                .send(BanningRequest::BanPeer {
                    violation,
                    reply: Some(tx),
                })
                .await
            {
                return Err(format!("[Banning] 'ban_peer' oneshot error: {err:?}").into());
            }
            let ban_status = timeout(TIMEOUT_WAITING_FOR_SERVICE, rx)
                .await
                .ok() // timeout -> None
                .and_then(Result::ok) // channel closed -> None
                .unwrap_or(BanStatus::NotBanned);
            Ok(matches!(ban_status, BanStatus::Banned { .. }))
        })
        .unwrap_or_else(Err);
    }
    Ok(false)
}

/// Unban a peer using the banning subsystem via the provided `banning_relay`.
/// If `peer_id` is `None`, returns `false`.
pub fn banning_unban_peer(
    banning_relay: &Option<OutboundRelay<BanningRequest>>,
    peer_id: &PeerId,
) -> Result<bool, overwatch::DynError> {
    if let Some(banning_relay) = banning_relay {
        return block_on_now_from_sync(async {
            let (tx, rx) = oneshot::channel();
            if let Err(err) = banning_relay
                .send(BanningRequest::UnbanPeer {
                    peer_id: *peer_id,
                    reply: tx,
                })
                .await
            {
                return Err(format!("[Banning] 'unban_peer' oneshot error: {err:?}").into());
            }
            let unbanned = timeout(TIMEOUT_WAITING_FOR_SERVICE, rx)
                .await
                .ok() // timeout -> None
                .and_then(Result::ok) // channel closed -> None
                .unwrap_or_default();
            Ok(unbanned)
        })
        .unwrap_or_else(Err);
    }
    Ok(false)
}

/// Subscribe to banning events using the banning subsystem via the provided
/// `banning_relay`.
pub fn banning_subscribe(
    banning_relay: &Option<OutboundRelay<BanningRequest>>,
) -> Result<Option<broadcast::Receiver<BanningEvent>>, overwatch::DynError> {
    if let Some(banning_relay) = banning_relay {
        return block_on_now_from_sync(async {
            let (tx, rx) = oneshot::channel();
            match banning_relay
                .send(BanningRequest::Subscribe { reply: tx })
                .await
            {
                Ok(()) => match timeout(TIMEOUT_WAITING_FOR_SERVICE, rx).await {
                    Ok(Ok(val)) => Ok(Some(val)),
                    Ok(Err(err)) => {
                        Err(format!("[Banning] 'subscribe' oneshot error: {err}").into())
                    }
                    Err(_) => Err("[Banning] Timeout 'subscribe'".into()),
                },
                Err(err) => Err(format!("[Banning] relay send error 'subscribe': {err:?}").into()),
            }
        })
        .unwrap_or_else(Err);
    }
    Ok(None)
}

/// List all active bans using the banning subsystem via the provided
/// `banning_relay`.
pub fn banning_list_active_bans(
    banning_relay: &Option<OutboundRelay<BanningRequest>>,
) -> Result<Vec<(PeerId, BanStatus)>, overwatch::DynError> {
    if let Some(banning_relay) = banning_relay {
        return block_on_now_from_sync(async {
            let (tx, rx) = oneshot::channel();
            match banning_relay
                .send(BanningRequest::ListActiveBans { reply: tx })
                .await
            {
                Ok(()) => match timeout(TIMEOUT_WAITING_FOR_SERVICE, rx).await {
                    Ok(Ok(ban_list)) => Ok(ban_list),
                    Ok(Err(err)) => {
                        Err(format!("[Banning] 'list_active_bans' oneshot error: {err}").into())
                    }
                    Err(_) => Err("[Banning] Timeout 'list_active_bans'".into()),
                },
                Err(err) => {
                    Err(format!("[Banning] Relay send error 'list_active_bans': {err:?}").into())
                }
            }
        })
        .unwrap_or_else(Err);
    }
    Ok(vec![])
}

/// A synchronous helper that runs a future to completion from synchronous code,
/// ensuring we don't deadlock or panic.
///
/// This helper function is intended for bridging sync APIs to async services.
/// Behaviour:
/// - When inside a Tokio multi-thread runtime, it uses `block_in_place(||
///   handle.block_on(fut))` to re-enter the async context but not deadlock the
///   runtime.
/// - If the current Tokio runtime is single-threaded, it creates a temporary
///   multi-thread runtime to run the future.
/// - If no Tokio runtime is available, it creates a temporary single-thread
///   runtime to run the future.
///
/// Safety notes:
/// - Creating a temporary runtime has non‑trivial cost and may affect timing in
///   tests.
/// - Callers should avoid calling this from latency‑sensitive async paths when
///   inside a multi‑thread runtime.
pub fn block_on_now_from_sync<T>(fut: impl Future<Output = T>) -> Result<T, overwatch::DynError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == RuntimeFlavor::MultiThread {
            // Current runtime is multi-threaded, we can block in place
            Ok(tokio::task::block_in_place(|| handle.block_on(fut)))
        } else {
            // Current runtime is not multi-threaded, create a temporary multi-threaded
            // runtime
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .map_err(|e| {
                    format!("[Banning] failed to create temp multi-thread runtime: {e}")
                })?;
            Ok(rt.block_on(fut))
        }
    } else {
        // We do not have a runtime available, create a new one
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| format!("[Banning] failed to create temp current-thread runtime: {e}"))?;
        Ok(rt.block_on(fut))
    }
}
