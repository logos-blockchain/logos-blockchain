//! A policy reacts to one sequencer's events from inside its drive loop; the
//! reference one is [`crate::consumer::ReferenceConsumer`].

use async_trait::async_trait;
use lb_core::mantle::ops::channel::{MsgId, inscribe::Inscription};
use lb_zone_sdk::sequencer::{
    ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, InscriptionInfo, ZoneSequencer,
};

use crate::node::SimNode;

/// A policy reacting to one sequencer's events from inside its drive loop.
#[async_trait]
pub trait Policy: Send {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event);

    /// The test published `this_msg` through the sequencer's handle.
    fn published(&mut self, _this_msg: MsgId) {}
}

#[must_use]
pub fn make_inscription(msg: &str) -> Inscription {
    Inscription::try_from(msg.as_bytes().to_vec()).expect("payload fits an inscription")
}

#[must_use]
pub fn payload_str(payload: &Inscription) -> String {
    String::from_utf8_lossy(payload.as_slice()).into_owned()
}

/// Inscriptions in the just-finalized txs.
pub fn finalized_inscriptions(finalized: &[FinalizedTx]) -> impl Iterator<Item = &InscriptionInfo> {
    finalized
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(info) => Some(info),
            FinalizedOp::Deposit(_)
            | FinalizedOp::Withdraw(_)
            | FinalizedOp::Config(_)
            | FinalizedOp::ChannelTransfer(_) => None,
        })
}

/// The inscriptions carried by channel-update entries, in order.
pub trait Inscriptions<'a> {
    fn inscriptions(self) -> impl Iterator<Item = &'a InscriptionInfo>;
}

impl<'a, I: IntoIterator<Item = &'a ChannelUpdateTx>> Inscriptions<'a> for I {
    fn inscriptions(self) -> impl Iterator<Item = &'a InscriptionInfo> {
        self.into_iter().filter_map(ChannelUpdateTx::inscription)
    }
}

/// The entries an update contributes to a consumer's non-finalized view: a
/// conflict's whole `canonical_chain()`, an extension's `adopted`.
pub fn contributed(update: &ChannelUpdate) -> impl Iterator<Item = &ChannelUpdateTx> {
    let prefix = match update {
        ChannelUpdate::Conflict { common_prefix, .. } => common_prefix.as_slice(),
        ChannelUpdate::Extension { .. } => &[],
    };
    prefix.iter().chain(update.adopted())
}

/// Does nothing; the test drives the sequencer directly.
pub struct PassivePolicy;

#[async_trait]
impl Policy for PassivePolicy {
    async fn on_event(&mut self, _sequencer: &mut ZoneSequencer<SimNode>, _event: &Event) {}
}
