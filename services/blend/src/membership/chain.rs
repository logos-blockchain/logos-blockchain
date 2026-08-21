//! Chain-derived per-epoch state.
//!
//! On every slot tick the chain is queried for the current epoch's
//! [`EpochState`](lb_ledger::EpochState); on each new epoch the membership and
//! leader inputs frozen into its SDP snapshot are yielded together as a
//! [`BlendEpochState`]. Both halves come from the same chain query, so they
//! cannot drift.

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

/// A chain-derived per-epoch state stream.
///
/// Not `Sync`, since producing each item awaits a chain query; consumers only
/// require `Send + Unpin`.
pub type BlendEpochStateStream<NodeId> =
    Pin<Box<dyn Stream<Item = BlendEpochState<NodeId>> + Send + 'static>>;

fn log_membership_transition<NodeId>(
    epoch: Epoch,
    slot: lb_cryptarchia_engine::Slot,
    membership: &lb_blend::scheduling::membership::Membership<NodeId>,
    minimum_network_size: usize,
) where
    NodeId: Debug + Eq + Hash,
{
    let mode = if membership.size() < minimum_network_size {
        "broadcast"
    } else if membership.contains_local() {
        "core"
    } else {
        "edge"
    };
    let local_node_id = membership
        .local_index()
        .and_then(|index| membership.get_node_at(index))
        .map(|node| format!("{:?}", node.id));
    let membership_identities: Vec<_> = (0..membership.size())
        .filter_map(|index| membership.get_node_at(index))
        .map(|node| format!("{:?}", node.id))
        .collect();
    tracing::info!(
        target: LOG_TARGET,
        diagnostic = "blend_tsi_outage",
        event = "blend_epoch_transition",
        epoch = u32::from(epoch),
        slot = u64::from(slot),
        local_node_id = ?local_node_id,
        local_is_member = membership.contains_local(),
        mode,
        membership_count = membership.size(),
        membership_identities = ?membership_identities,
        "Rebuilt Blend epoch membership"
    );
}

/// Subscribe to a chain-derived stream of [`BlendEpochState`].
///
/// One item is yielded per epoch — the first slot of the epoch whose chain
/// query succeeds. Slot ticks within an already-yielded epoch are ignored;
/// failed chain queries do not advance the tracked epoch, so the next slot of
/// the same epoch is retried.
#[expect(
    clippy::too_many_lines,
    reason = "Epoch latch diagnostics remain adjacent to the handoff"
)]
pub async fn subscribe<ChainService, NodeId, TimeRuntimeBackend, RuntimeServiceId>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    signing_public_key: Ed25519PublicKey,
    zk_public_key: Option<ZkPublicKey>,
    minimum_network_size: usize,
) -> BlendEpochStateStream<NodeId>
where
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    NodeId: node_id::TryFrom + Clone + Debug + Hash + Eq + Send + Sync + 'static,
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
        (
            slot_ticks,
            None::<Epoch>,
            chain_service,
            signing_public_key,
            zk_public_key,
            minimum_network_size,
        ),
        async move |(
            mut ticks,
            mut last_epoch,
            chain_api,
            signing_pk,
            zk_pk,
            minimum_network_size,
        )| {
            loop {
                let SlotTick { epoch, slot } = ticks.next().await?;
                if Some(epoch) == last_epoch {
                    continue;
                }
                match chain_api.get_epoch_state_with_source(slot).await {
                    Ok(Ok(query_result)) => {
                        let lb_chain_service::EpochStateQueryResult {
                            epoch_state,
                            source_tip_id,
                            source_tip_slot,
                            source_tip_height,
                            source_lib_id,
                            source_lib_slot,
                            ..
                        } = query_result;
                        let membership_info = membership_info_from_epoch_state::<NodeId>(
                            &epoch_state,
                            &signing_pk,
                            zk_pk,
                        );
                        let membership_provider_ids: Vec<_> =
                            (0..membership_info.membership.size())
                                .filter_map(|index| membership_info.membership.get_node_at(index))
                                .map(|node| format!("{:?}", node.id))
                                .collect();
                        tracing::info!(
                            target: LOG_TARGET,
                            diagnostic = "blend_tsi_outage",
                            event = "blend_epoch_state_latched",
                            clock_epoch = u32::from(epoch),
                            clock_slot = u64::from(slot),
                            epoch_state_epoch = u32::from(epoch_state.epoch),
                            nonce = ?epoch_state.nonce,
                            aged_utxo_root = ?epoch_state.utxo_merkle_root(),
                            lottery_0 = ?epoch_state.lottery_0,
                            lottery_1 = ?epoch_state.lottery_1,
                            blend_pow_difficulty = "unavailable_in_epoch_state",
                            membership_count = membership_info.membership.size(),
                            membership_provider_ids = ?membership_provider_ids,
                            source_tip_id = %source_tip_id,
                            source_tip_slot = u64::from(source_tip_slot),
                            source_tip_height,
                            source_lib_id = %source_lib_id,
                            source_lib_slot = u64::from(source_lib_slot),
                            "Latched Blend epoch state from chain query"
                        );
                        last_epoch = Some(epoch);
                        log_membership_transition(
                            epoch,
                            slot,
                            &membership_info.membership,
                            minimum_network_size,
                        );
                        let item = BlendEpochState {
                            epoch,
                            nonce: epoch_state.nonce,
                            aged: epoch_state.utxo_merkle_root(),
                            lottery_0: epoch_state.lottery_0,
                            lottery_1: epoch_state.lottery_1,
                            membership_info,
                        };
                        return Some((
                            item,
                            (
                                ticks,
                                last_epoch,
                                chain_api,
                                signing_pk,
                                zk_pk,
                                minimum_network_size,
                            ),
                        ));
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
