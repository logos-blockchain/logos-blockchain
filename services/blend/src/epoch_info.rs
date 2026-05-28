use core::{
    fmt::{self, Debug, Formatter},
    marker::PhantomData,
    num::NonZeroU64,
    ops::Deref,
};

use async_trait::async_trait;
use futures::Stream;
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::proofs::leader_proof::LeaderPublic;
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_ledger::EpochState;
use lb_log_targets::blend;
use lb_time_service::SlotTick;
use overwatch::overwatch::OverwatchHandle;

/// Secret `PoL` info associated to an epoch, as returned by the `PoL` info
/// provider.
#[derive(Clone)]
pub struct PolEpochInfo {
    pub epoch: Epoch,
    pub poq_public_inputs: LeaderPublic,
    /// The `PoL` secret inputs that are found to be winning at least one slot
    /// in the current epoch.
    pub poq_private_inputs: ProofOfLeadershipQuotaInputs,
}

impl Debug for PolEpochInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolEpochInfo")
            .field("epoch", &self.epoch)
            .field("poq_public_inputs", &self.poq_public_inputs)
            .field("poq_private_inputs", &"<redacted>")
            .finish()
    }
}

#[async_trait]
pub trait PolInfoProvider<RuntimeServiceId> {
    type Stream: Stream<Item = PolEpochInfo>;

    async fn subscribe(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream>;
}
