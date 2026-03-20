use core::{
    fmt::{Debug, Display},
    future::ready,
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_blend_service::epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait};
use lb_chain_leader_service::LeaderMsg;
use lb_pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use lb_services_utils::wait_until_services_are_ready;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot::channel;
use tokio_stream::wrappers::WatchStream;

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

    /// Subscribes to a stream of potential winning `PoL` epoch slots, and
    /// filters out `None` values (initial state) and already processed epochs.
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
        let pol_winning_slot_receiver = receiver.await.ok()?;
        // Return a `WatchStream` that filters out `None`s (i.e., at the very beginning
        // of chain leader start), and any leader info that belongs to an already
        // processed epoch.
        Some(Box::new(
            WatchStream::new(pol_winning_slot_receiver)
                .filter_map(ready)
                .scan(
                    None,
                    |processed_epoch, (leader_private, leader_public, epoch)| {
                        let should_yield_new_epoch =
                            processed_epoch.is_none_or(|processed_epoch| processed_epoch < epoch);
                        if !should_yield_new_epoch {
                            return ready(Some(None));
                        }

                        *processed_epoch = Some(epoch);
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

                        ready(Some(Some(PolEpochInfo {
                            epoch,
                            poq_public_inputs: leader_public,
                            poq_private_inputs: ProofOfLeadershipQuotaInputs {
                                aged_path_and_selectors,
                                note_value: *note_value,
                                output_number: *output_number,
                                secret_key: *secret_key,
                                slot: *slot_number,
                                transaction_hash: *transaction_hash,
                            },
                        })))
                    },
                )
                .filter_map(ready),
        ))
    }
}
