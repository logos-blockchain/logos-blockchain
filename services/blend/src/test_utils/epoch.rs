use core::cell::RefCell;

use async_trait::async_trait;
use futures::{
    Stream,
    future::ready,
    stream::{once, repeat},
};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_groth16::AdditiveGroup as _;
use overwatch::overwatch::OverwatchHandle;
use tokio::sync::watch;

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
            winning_pol_info_stream: Box::pin(repeat(ProofOfLeadershipQuotaInputs {
                slot: 1,
                note_value: 1,
                transaction_hash: ZkHash::ZERO,
                output_number: 1,
                aged_path_and_selectors: [(ZkHash::ZERO, false); _],
                secret_key: ZkHash::ZERO,
            })),
        }))))
    }
}

thread_local! {
    static POL_GATE: RefCell<Option<watch::Receiver<bool>>> = const { RefCell::new(None) };
}

/// Holds this epoch's secret `PoL` info back until [`Self::release`] is called,
/// so a test can occupy the window in which a node knows its membership but
/// cannot yet build leadership proofs — the window a block proposal used to be
/// dropped in.
///
/// Level-triggered like [`crate::test_utils::crypto::PowGate`]: the stream is
/// polled repeatedly, so an edge-triggered gate would lose the release.
pub struct PolGate(watch::Sender<bool>);

impl PolGate {
    /// Sets up a closed gate for providers subscribed on this thread.
    #[must_use]
    pub fn setup() -> Self {
        let (sender, receiver) = watch::channel(false);
        POL_GATE.with_borrow_mut(|gate| *gate = Some(receiver));
        Self(sender)
    }

    /// Lets the secret `PoL` info through, now and from now on.
    pub fn release(&self) {
        self.0.send_replace(true);
    }
}

/// Yields the same single [`PolEpochInfo`] as [`OncePolStreamProvider`], but
/// not until a [`PolGate`] opens.
pub struct GatedPolStreamProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProvider<RuntimeServiceId> for GatedPolStreamProvider {
    type Stream = Box<dyn Stream<Item = PolEpochInfo> + Send + Unpin>;

    async fn subscribe(
        _overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream> {
        let mut gate = POL_GATE.with_borrow(Clone::clone)?;
        Some(Box::new(Box::pin(once(async move {
            gate.wait_for(|open| *open)
                .await
                .expect("the gate should outlive the provider");
            PolEpochInfo {
                epoch: Epoch::new(0),
                winning_pol_info_stream: Box::pin(repeat(ProofOfLeadershipQuotaInputs {
                    slot: 1,
                    note_value: 1,
                    transaction_hash: ZkHash::ZERO,
                    output_number: 1,
                    aged_path_and_selectors: [(ZkHash::ZERO, false); _],
                    secret_key: ZkHash::ZERO,
                })),
            }
        }))))
    }
}
