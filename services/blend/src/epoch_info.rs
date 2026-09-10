use core::fmt::{self, Debug, Formatter};

use async_trait::async_trait;
use futures::Stream;
use lb_blend::scheduling::message_blend::provers::WinningPolInfoStream;
pub use lb_core::sdp::blend::{PolEpochState, PolEpochStateSource};
use lb_cryptarchia_engine::Epoch;
use overwatch::overwatch::OverwatchHandle;

/// Private `PoL` information for an epoch, as returned by the `PoL` info
/// provider.
///
/// `state` identifies the chain-derived epoch state against which the lazy
/// winning-slot stream was constructed. The stream carries the secret inputs
/// for winning slots and is consumed lazily.
pub struct PolEpochInfo {
    pub epoch: Epoch,
    pub state: PolEpochState,
    /// The stream of `PoL` secret inputs for the slots found to be winning in
    /// this epoch.
    pub winning_pol_info_stream: WinningPolInfoStream,
}

impl Debug for PolEpochInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolEpochInfo")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait PolInfoProvider<RuntimeServiceId> {
    type Stream: Stream<Item = PolEpochInfo>;

    async fn subscribe(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream>;
}
