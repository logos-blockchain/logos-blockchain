use async_trait::async_trait;
use futures::{Stream, future::ready, stream::once};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_groth16::AdditiveGroup as _;
use overwatch::overwatch::OverwatchHandle;

use crate::epoch_info::{PolEpochInfo, PolInfoProvider};

pub struct OncePolStreamProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProvider<RuntimeServiceId> for OncePolStreamProvider {
    type Stream = Box<dyn Stream<Item = PolEpochInfo> + Send + Unpin>;

    async fn subscribe(
        _overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream> {
        Some(Box::new(once(ready(PolEpochInfo {
            epoch: Epoch::new(0),
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
