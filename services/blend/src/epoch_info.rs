use core::fmt::{self, Debug, Formatter};

use async_trait::async_trait;
use futures::Stream;
use lb_blend::scheduling::message_blend::provers::WinningPolInfoStream;
use lb_cryptarchia_engine::Epoch;
use overwatch::overwatch::OverwatchHandle;

/// Secret `PoL` info associated to an epoch, as returned by the `PoL` info
/// provider.
///
/// Carries the epoch's *stream* of winning-slot secret inputs (one item per
/// winning slot), so the leadership proof generator can use a distinct slot for
/// each data message and thereby produce distinct key nullifiers. Exactly one
/// `PolEpochInfo` is yielded per epoch; the stream inside is consumed lazily.
pub struct PolEpochInfo {
    pub epoch: Epoch,
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
