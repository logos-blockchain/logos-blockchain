use core::fmt::{Debug, Display};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_blend_service::epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait};
use lb_chain_leader_service::{LeaderMsg, WinningPolEpochSlots};
use lb_core::proofs::leader_proof::LeaderPrivate;
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
            |WinningPolEpochSlots {
                 epoch,
                 state,
                 slots,
             }| {
                PolEpochInfo {
                    epoch,
                    state,
                    // Just drive each per-slot future and drop the non-winners.
                    winning_pol_info_stream: Box::pin(
                        slots
                            .filter_map(|winning_slot| winning_slot)
                            .map(|leader_private| leadership_quota_inputs(&leader_private)),
                    ),
                }
            },
        )))
    }
}

/// The `PoQ` leader inputs a winning [`LeaderPrivate`] gives.
#[must_use]
pub fn leadership_quota_inputs(leader_private: &LeaderPrivate) -> ProofOfLeadershipQuotaInputs {
    let PolWitnessInputsData {
        wallet:
            PolWalletInputsData {
                note_value,
                note_nonce,
                set_selectors,
                commitment_aged_path,
                commitment_aged_selectors,
                nullifier_root_aged,
                set_aged_path,
                secret_key,
                ..
            },
        chain: PolChainInputsData { slot_number, .. },
    } = leader_private.input();

    ProofOfLeadershipQuotaInputs {
        slot: *slot_number,
        note_value: *note_value,
        note_nonce: *note_nonce,
        commitment_aged_path_and_selectors: core::array::from_fn(|i| {
            (commitment_aged_path[i], commitment_aged_selectors[i])
        }),
        nullifier_root_aged: *nullifier_root_aged,
        set_aged_path_and_selectors: core::array::from_fn(|i| (set_aged_path[i], set_selectors[i])),
        secret_key: *secret_key,
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Fr};

    use super::*;

    #[test]
    fn leadership_quota_inputs_take_the_aged_half_of_the_pol_witness() {
        let (_, private) = lb_core::proofs::leader_proof::test_utils::note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::ZERO,
        );
        let PolWitnessInputsData { wallet, chain } = private.input().clone();
        let inputs = leadership_quota_inputs(&private);
        assert_eq!(
            (
                inputs.slot,
                inputs.note_value,
                inputs.note_nonce,
                inputs.secret_key
            ),
            (chain.slot_number, 1000, Fr::from(11u64), Fr::from(7u64))
        );
        assert_eq!(inputs.nullifier_root_aged, wallet.nullifier_root_aged);
        assert_eq!(
            inputs.commitment_aged_path_and_selectors[0],
            (
                wallet.commitment_aged_path[0],
                wallet.commitment_aged_selectors[0]
            )
        );
        assert_eq!(
            inputs.set_aged_path_and_selectors[0],
            (wallet.set_aged_path[0], wallet.set_selectors[0])
        );
    }
}
