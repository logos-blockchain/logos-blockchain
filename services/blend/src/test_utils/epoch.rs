use async_trait::async_trait;
use futures::{Stream, future::ready, stream::once};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_chain_service::Slot;
use lb_core::crypto::ZkHash;
use lb_groth16::{Field as _, Fr};
use lb_ledger::EpochState;
use overwatch::overwatch::OverwatchHandle;

use crate::epoch_info::{ChainApi, PolEpochInfo, PolInfoProvider};

pub fn default_epoch_state() -> EpochState {
    use lb_ledger::UtxoTree;

    EpochState {
        epoch: 1.into(),
        nonce: ZkHash::ZERO,
        total_stake: 1_000,
        utxos: UtxoTree::new(),
        lottery_0: Fr::ZERO,
        lottery_1: Fr::ZERO,
    }
}

#[derive(Clone)]
pub struct TestChainService;

#[async_trait]
impl<RuntimeServiceId> ChainApi<RuntimeServiceId> for TestChainService {
    async fn get_epoch_state_for_slot(&self, _slot: Slot) -> EpochState {
        default_epoch_state()
    }
}

pub struct OncePolStreamProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProvider<RuntimeServiceId> for OncePolStreamProvider {
    type Stream = Box<dyn Stream<Item = PolEpochInfo> + Send + Unpin>;

    async fn subscribe(
        _overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream> {
        Some(Box::new(once(ready(PolEpochInfo {
            nonce: ZkHash::ZERO,
            poq_private_inputs: ProofOfLeadershipQuotaInputs {
                slot: 1,
                note_value: 1,
                transaction_hash: ZkHash::ZERO,
                output_number: 1,
                aged_path_and_selectors: [(ZkHash::ZERO, false); _],
                secret_key: ZkHash::ZERO,
            },
        }))))
    }
}
