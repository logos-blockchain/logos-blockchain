use core::fmt::{Debug, Display};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_blend_service::epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait};
use lb_chain_leader_service::{LeaderMsg, WinningPolEpochSlots};
use lb_pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use lb_services_utils::wait_until_services_are_ready;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot::channel;

use crate::CryptarchiaLeaderService;

/// The provider of a stream of winning `PoL` epoch slots for the Blend service,
/// without introducing a cyclic dependency from Blend service to chain service.
pub struct PolInfoProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProviderTrait<RuntimeServiceId> for PolInfoProvider
where
    RuntimeServiceId:
        AsServiceId<CryptarchiaLeaderService> + Debug + Display + Send + Sync + 'static,
{
    type Stream = Box<dyn Stream<Item = PolEpochInfo> + Send + Unpin>;

    /// Subscribes to the chain-leader's per-epoch winning-slot handoffs (one
    /// per epoch) and maps each into a [`PolEpochInfo`], converting that
    /// epoch's stream of winning leadership inputs into the Blend
    /// leadership-quota inputs.
    async fn subscribe(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream> {
        wait_until_services_are_ready!(
            overwatch_handle,
            // No timeout since chain-leader service becomes ready
            // only after switching to Online mode.
            None,
            CryptarchiaLeaderService
        )
        .await
        .ok()?;
        let cryptarchia_service_relay = overwatch_handle
            .relay::<CryptarchiaLeaderService>()
            .await
            .ok()?;
        let (sender, receiver) = channel();
        cryptarchia_service_relay
            .send(LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender })
            .await
            .ok()?;
        let winning_pol_epoch_slots_stream = receiver.await.ok()?;
        Some(Box::new(winning_pol_epoch_slots_stream.map(
            |WinningPolEpochSlots { epoch, slots }| {
                PolEpochInfo {
                    epoch,
                    // Just drive each per-slot future and drop the non-winners.
                    winning_pol_info_stream: Box::pin(
                        slots
                            .filter_map(|winning_slot| winning_slot)
                            .map(|leader_private| {
                                let PolWitnessInputsData {
                                    wallet:
                                        PolWalletInputsData {
                                            aged_path,
                                            aged_selectors,
                                            note_value,
                                            output_number,
                                            secret_key,
                                            transaction_hash,
                                            ..
                                        },
                                    chain: PolChainInputsData { slot_number, .. },
                                } = leader_private.input();

                                let aged_path_and_selectors =
                                    core::array::from_fn(|i| (aged_path[i], aged_selectors[i]));

                                ProofOfLeadershipQuotaInputs {
                                    aged_path_and_selectors,
                                    note_value: *note_value,
                                    output_number: *output_number,
                                    secret_key: *secret_key,
                                    slot: *slot_number,
                                    transaction_hash: *transaction_hash,
                                }
                            }),
                    ),
                }
            },
        )))
    }
}
