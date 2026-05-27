//! Chain-derived membership.
//!
//! The membership for each epoch is read from the SDP snapshot frozen into that
//! epoch's [`EpochState`](lb_ledger::EpochState), queried from the chain on
//! slot ticks. This puts membership on the **same slot-tick clock** as the
//! leader inputs (the [`EpochHandler`] `PoL` path), so both halves share the
//! chain's per-epoch view and cannot drift — replacing the pushed
//! `ActiveProviders` broadcast.

use core::{hash::Hash, pin::Pin};
use std::fmt::{Debug, Display};

use futures::{Stream, StreamExt as _, stream::unfold};
use lb_chain_service::{
    Epoch,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use lb_time_service::{SlotTick, TimeService, TimeServiceMessage, backends::TimeBackend};
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot;

use crate::{
    LOG_TARGET,
    membership::{MembershipInfo, node_id, service::membership_info_from_epoch_state},
};

#[derive(Clone, Debug)]
pub struct BlendEpochState<NodeId> {
    pub epoch: Epoch,
    pub nonce: Fr,
    pub aged: Fr,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
    pub membership_info: MembershipInfo<NodeId>,
}

/// A chain-derived membership stream.
///
/// Unlike [`MembershipStream`](super::MembershipStream) this is not `Sync`,
/// since producing each item awaits a chain query; consumers only require
/// `Send + Unpin`.
pub type BlendEpochStateStream<NodeId> =
    Pin<Box<dyn Stream<Item = BlendEpochState<NodeId>> + Send + 'static>>;

/// Subscribe to a chain-derived stream of
/// [`BlendEpochState`](super::BlendEpochState).
///
/// On every slot tick the chain is queried for the current epoch's
/// `EpochState`; on each new epoch the membership frozen into its SDP snapshot
/// is yielded. The same `EpochHandler`/`get_epoch_state` mechanism is used by
/// the leader-input path, so the two are on one clock.
pub async fn subscribe<ChainService, NodeId, TimeRuntimeBackend, RuntimeServiceId>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    signing_public_key: Ed25519PublicKey,
    zk_public_key: Option<ZkPublicKey>,
) -> BlendEpochStateStream<NodeId>
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

    // TODO: Refactor into a function or own type that replaces `EpochHandler`.
    Box::pin(unfold(
        (slot_ticks, None::<Epoch>, chain_service),
        move |(mut slot_ticks, mut last_epoch, chain_service)| async move {
            loop {
                let SlotTick { epoch, slot } = slot_ticks.next().await?;
                if Some(epoch) == last_epoch {
                    continue;
                }
                match chain_service.get_epoch_state(slot).await {
                    Ok(Ok(epoch_state)) => {
                        last_epoch = Some(epoch);
                        let membership_info = membership_info_from_epoch_state::<NodeId>(
                            &epoch_state,
                            &signing_public_key,
                            zk_public_key,
                        );
                        let item = BlendEpochState {
                            epoch,
                            nonce: epoch_state.nonce,
                            aged: epoch_state.utxo_merkle_root(),
                            lottery_0: epoch_state.lottery_0,
                            lottery_1: epoch_state.lottery_1,
                            membership_info,
                        };
                        return Some((item, (slot_ticks, last_epoch, chain_service)));
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(target: LOG_TARGET, "Chain service returned error for epoch state at slot {slot:?}: {e:?}; will retry on next slot of epoch {epoch:?}");
                    }
                    Err(e) => {
                        tracing::warn!(target: LOG_TARGET, "Failed to query epoch state at slot {slot:?}: {e:?}; will retry on next slot of epoch {epoch:?}");
                    }
                }
            }
        },
    ))
}
