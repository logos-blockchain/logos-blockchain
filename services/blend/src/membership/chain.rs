//! Chain-derived membership.
//!
//! The membership for each epoch is read from the SDP snapshot frozen into that
//! epoch's [`EpochState`](lb_ledger::EpochState), queried from the chain on
//! slot ticks. This puts membership on the **same slot-tick clock** as the
//! leader inputs (the [`EpochHandler`] `PoL` path), so both halves share the
//! chain's per-epoch view and cannot drift — replacing the pushed
//! `ActiveProviders` broadcast.

use core::{hash::Hash, num::NonZeroU64, pin::Pin};
use std::fmt::{Debug, Display};

use futures::{Stream, StreamExt as _};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use lb_time_service::{TimeService, TimeServiceMessage, backends::TimeBackend};
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot;

use crate::{
    epoch_info::{EpochEvent, EpochHandler},
    membership::{MembershipInfo, node_id, service::membership_info_from_epoch_state},
};

/// A chain-derived membership stream.
///
/// Unlike [`MembershipStream`](super::MembershipStream) this is not `Sync`,
/// since producing each item awaits a chain query; consumers only require
/// `Send + Unpin`.
pub type ChainMembershipStream<NodeId> =
    Pin<Box<dyn Stream<Item = MembershipInfo<NodeId>> + Send + 'static>>;

/// Subscribe to a chain-derived stream of
/// [`MembershipInfo`](super::MembershipInfo).
///
/// On every slot tick the chain is queried for the current epoch's
/// `EpochState`; on each new epoch the membership frozen into its SDP snapshot
/// is yielded. The same `EpochHandler`/`get_epoch_state` mechanism is used by
/// the leader-input path, so the two are on one clock.
pub async fn subscribe<ChainService, NodeId, TimeRuntimeBackend, RuntimeServiceId>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    signing_public_key: Ed25519PublicKey,
    zk_public_key: Option<ZkPublicKey>,
    epoch_transition_period_in_slots: NonZeroU64,
) -> ChainMembershipStream<NodeId>
where
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    NodeId: node_id::TryFrom + Clone + Hash + Eq + Send + Sync + 'static,
    TimeRuntimeBackend: TimeBackend + Send,
    RuntimeServiceId: AsServiceId<ChainService>
        + AsServiceId<TimeService<TimeRuntimeBackend, RuntimeServiceId>>
        + Clone
        + Debug
        + Display
        + Sync
        + Send
        + Unpin
        + 'static,
{
    let chain_service = CryptarchiaServiceApi::<ChainService, RuntimeServiceId>::new(
        overwatch_handle
            .relay::<ChainService>()
            .await
            .expect("Relay with chain service should be available."),
    );
    let epoch_handler = EpochHandler::new(chain_service, epoch_transition_period_in_slots);

    let slot_ticks = {
        let time_relay = overwatch_handle
            .relay::<TimeService<_, _>>()
            .await
            .expect("Relay with time service should be available.");
        let (sender, receiver) = oneshot::channel();
        time_relay
            .send(TimeServiceMessage::Subscribe { sender })
            .await
            .expect("Failed to subscribe to slot clock.");
        receiver
            .await
            .expect("Should not fail to receive slot stream from time service.")
    };

    Box::pin(futures::stream::unfold(
        (slot_ticks, epoch_handler, signing_public_key, zk_public_key),
        async move |(mut slot_ticks, mut epoch_handler, signing_public_key, zk_public_key)| {
            loop {
                let tick = slot_ticks.next().await?;
                if let Some(
                    EpochEvent::NewEpoch((epoch_state, _))
                    | EpochEvent::NewEpochAndOldEpochTransitionExpired((epoch_state, _)),
                ) = epoch_handler.tick(tick).await
                {
                    let info = membership_info_from_epoch_state::<NodeId>(
                        &epoch_state,
                        &signing_public_key,
                        zk_public_key,
                    );
                    return Some((
                        info,
                        (slot_ticks, epoch_handler, signing_public_key, zk_public_key),
                    ));
                }
            }
        },
    ))
}
