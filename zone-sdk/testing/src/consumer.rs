//! The reference consumer: the least a zone has to do on top of the SDK's
//! contract, and what every property holds the SDK to.
//!
//! It publishes its planned payloads once the sequencer is ready and
//! re-publishes its own orphaned inscriptions, tracking message lineage so
//! an intent lands once however often the chain reorganizes under it. It
//! only uses the public sequencer API, so it can move into the SDK as the
//! example consumer.
//!
//! A zone makes choices beyond that minimum: it can let an orphan die, post
//! a burst of payloads when its turn comes, or pick up another sequencer's
//! orphans. [`Choices`] lets a test make those choices between steps, so a
//! property can draw them at random; the SDK has to survive every one.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use lb_core::mantle::ops::channel::{MsgId, inscribe::Inscription};
use lb_key_management_system_service::keys::{Ed25519Key, UnverifiedEd25519PublicKey};
use lb_zone_sdk::sequencer::{
    ChannelUpdate, ChannelUpdateTx, Event, InscriptionInfo, ZoneSequencer,
};
use tracing::warn;

use crate::{
    node::SimNode,
    policy::{Inscriptions as _, Policy, finalized_inscriptions},
};

/// Which of our intents each message serves, and which members of an intent
/// are live: on the canonical view, or finalized.
#[derive(Default)]
struct LineageTracker {
    intent_root: HashMap<MsgId, MsgId>,
    pending: HashMap<MsgId, HashSet<MsgId>>,
    finalized_roots: HashSet<MsgId>,
}

impl LineageTracker {
    fn record_publish(&mut self, this_msg: MsgId) {
        self.intent_root.insert(this_msg, this_msg);
        self.pending.entry(this_msg).or_default().insert(this_msg);
    }

    fn record_republish(&mut self, orphan: MsgId, republished: MsgId) {
        let root = *self.intent_root.entry(orphan).or_insert(orphan);
        self.intent_root.insert(republished, root);
        self.pending.entry(root).or_default().insert(republished);
    }

    /// Rebuild the live members from the canonical view.
    fn observe(&mut self, channel_update: &ChannelUpdate) {
        let Some(chain) = channel_update.canonical_chain() else {
            return;
        };
        self.pending.clear();
        for info in chain.inscriptions() {
            if let Some(&root) = self.intent_root.get(&info.this_msg) {
                self.pending.entry(root).or_default().insert(info.this_msg);
            }
        }
    }

    /// Whether `this_msg` serves one of our intents.
    fn is_ours(&self, this_msg: &MsgId) -> bool {
        self.intent_root.contains_key(this_msg)
    }

    fn observe_finalized(&mut self, finalized: impl Iterator<Item = MsgId>) {
        for this_msg in finalized {
            if let Some(&root) = self.intent_root.get(&this_msg) {
                self.finalized_roots.insert(root);
            }
        }
    }

    fn intent_live(&self, this_msg: &MsgId) -> bool {
        let root = self.intent_root.get(this_msg).copied().unwrap_or(*this_msg);
        self.finalized_roots.contains(&root)
            || self
                .pending
                .get(&root)
                .is_some_and(|members| !members.is_empty())
    }
}

/// What a consumer does beyond the contract's minimum, set by the test
/// that holds the handle and read on the next event.
#[derive(Default)]
pub struct Choices {
    abandon_next: bool,
    burst: Vec<Inscription>,
    adopt: bool,
    /// Own orphans this consumer let die, in order.
    pub abandoned: Vec<Inscription>,
    /// Burst payloads it published, in order.
    pub published: Vec<Inscription>,
}

impl Choices {
    /// Let the next own orphan die instead of republishing it.
    pub const fn abandon_next(&mut self) {
        self.abandon_next = true;
    }

    /// Publish `payloads` at once on the next event in turn.
    pub fn burst(&mut self, payloads: Vec<Inscription>) {
        self.burst.extend(payloads);
    }

    /// Republish other sequencers' orphans too, from now on or no longer.
    pub const fn adopt(&mut self, on: bool) {
        self.adopt = on;
    }
}

pub type SharedChoices = Arc<Mutex<Choices>>;

/// Who republishes which payload among a run's consumers.
///
/// A consumer republishes its own orphan only while nobody else holds it,
/// and adopts another's only while nobody does, so an adopted orphan lands
/// once.
pub type Claims = Arc<Mutex<HashMap<Inscription, UnverifiedEd25519PublicKey>>>;

pub struct ReferenceConsumer {
    own: UnverifiedEd25519PublicKey,
    planned: Vec<Inscription>,
    published_initial: bool,
    lineage: LineageTracker,
    choices: SharedChoices,
    claims: Option<Claims>,
    /// Other sequencers' orphans seen and not yet landed again: offered for
    /// adoption on every event, since their owner may abandon them later.
    others: HashMap<Inscription, InscriptionInfo>,
    /// Own orphans whose republish failed, for instance while the funding
    /// wallet has nothing free; tried again on the next block.
    retry: Vec<InscriptionInfo>,
}

impl ReferenceConsumer {
    /// A consumer signing as `key` that publishes `planned` once ready.
    #[must_use]
    pub fn new(key: &Ed25519Key, planned: Vec<Inscription>) -> Self {
        Self {
            own: key.public_key().into_unverified(),
            planned,
            published_initial: false,
            lineage: LineageTracker::default(),
            choices: SharedChoices::default(),
            claims: None,
            others: HashMap::new(),
            retry: Vec::new(),
        }
    }

    #[must_use]
    pub fn boxed(key: &Ed25519Key, planned: Vec<Inscription>) -> Box<dyn Policy> {
        Box::new(Self::new(key, planned))
    }

    /// Take choices from `choices` and share `claims` with the other
    /// consumers of the run.
    #[must_use]
    pub fn with_choices(mut self, choices: SharedChoices, claims: Claims) -> Self {
        self.choices = choices;
        self.claims = Some(claims);
        self
    }

    /// Whether this consumer may republish `payload`: free, or already its
    /// own. Claims it when so.
    fn claim(&self, payload: &Inscription) -> bool {
        let Some(claims) = &self.claims else {
            return true;
        };
        let mut claims = claims.lock().expect("claims");
        *claims.entry(payload.clone()).or_insert(self.own) == self.own
    }

    fn release(&self, payload: &Inscription) {
        if let Some(claims) = &self.claims {
            claims.lock().expect("claims").remove(payload);
        }
    }

    async fn publish_burst(&mut self, sequencer: &mut ZoneSequencer<SimNode>) {
        let in_turn = sequencer
            .subscribe_channel_view()
            .borrow()
            .our_turn_to_write;
        let burst = {
            let mut choices = self.choices.lock().expect("choices");
            if choices.burst.is_empty() || !in_turn {
                return;
            }
            std::mem::take(&mut choices.burst)
        };
        tracing::debug!(count = burst.len(), "burst in turn");
        for payload in burst {
            let _ = self.claim(&payload);
            match sequencer.handle().publish(payload.clone()).await {
                Ok((result, _checkpoint)) => {
                    self.lineage
                        .record_publish(result.tx.inscription().this_msg);
                    self.choices
                        .lock()
                        .expect("choices")
                        .published
                        .push(payload);
                }
                Err(error) => warn!(%error, "Failed to publish burst zone payload"),
            }
        }
    }

    /// Republish `info`, chaining the new message on the orphan's intent.
    async fn republish(&mut self, sequencer: &mut ZoneSequencer<SimNode>, info: &InscriptionInfo) {
        match sequencer.handle().publish(info.payload.clone()).await {
            Ok((result, _checkpoint)) => {
                let republished = result.tx.inscription().this_msg;
                tracing::debug!(payload = ?info.payload, orphan = ?info.this_msg, ?republished, "republished own orphan");
                self.lineage.record_republish(info.this_msg, republished);
            }
            Err(error) => {
                warn!(%error, payload = ?info.payload, "Failed to re-publish orphaned zone payload");
                self.retry.push(info.clone());
            }
        }
    }
}

#[async_trait]
impl Policy for ReferenceConsumer {
    fn published(&mut self, this_msg: MsgId) {
        self.lineage.record_publish(this_msg);
    }

    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
        match event {
            Event::Ready if !self.published_initial => {
                self.published_initial = true;
                for payload in self.planned.clone() {
                    let _ = self.claim(&payload);
                    match sequencer.handle().publish(payload).await {
                        Ok((result, _checkpoint)) => {
                            self.lineage
                                .record_publish(result.tx.inscription().this_msg);
                        }
                        Err(error) => warn!(%error, "Failed to publish planned zone payload"),
                    }
                }
            }
            Event::BlocksProcessed {
                channel_update,
                finalized,
                ..
            } => {
                self.lineage
                    .observe_finalized(finalized_inscriptions(finalized).map(|i| i.this_msg));
                self.lineage.observe(channel_update);
                for info in std::mem::take(&mut self.retry) {
                    if !self.lineage.intent_live(&info.this_msg) {
                        self.republish(sequencer, &info).await;
                    }
                }
                let finalized: HashSet<&Inscription> = finalized_inscriptions(finalized)
                    .map(|i| &i.payload)
                    .collect();
                let on_chain: HashSet<&Inscription> = channel_update
                    .canonical_chain()
                    .into_iter()
                    .flatten()
                    .inscriptions()
                    .map(|i| &i.payload)
                    .collect();
                for entry in channel_update.orphaned() {
                    let ChannelUpdateTx::Inscription(info) = entry else {
                        continue;
                    };
                    if self.lineage.intent_live(&info.this_msg) {
                        tracing::debug!(payload = ?info.payload, orphan = ?info.this_msg, "orphan's intent is live; not republished");
                        continue;
                    }
                    // Ours is what we published, not what carries our key:
                    // sequencers may share a key and still each republish
                    // only their own.
                    if !self.lineage.is_ours(&info.this_msg) {
                        if info.signer != Some(self.own) {
                            self.others.insert(info.payload.clone(), info.clone());
                        }
                        continue;
                    }
                    if !self.claim(&info.payload) {
                        tracing::debug!(payload = ?info.payload, "own orphan held by another consumer; not republished");
                        continue;
                    }
                    let abandon = {
                        let mut choices = self.choices.lock().expect("choices");
                        std::mem::take(&mut choices.abandon_next)
                    };
                    if abandon {
                        tracing::debug!(payload = ?info.payload, "abandoned own orphan");
                        self.release(&info.payload);
                        self.choices
                            .lock()
                            .expect("choices")
                            .abandoned
                            .push(info.payload.clone());
                        continue;
                    }
                    self.republish(sequencer, info).await;
                }
                self.others.retain(|payload, _| {
                    !finalized.contains(payload) && !on_chain.contains(payload)
                });
                if self.choices.lock().expect("choices").adopt {
                    let candidates: Vec<InscriptionInfo> = self.others.values().cloned().collect();
                    for info in candidates {
                        if !self.claim(&info.payload) {
                            continue;
                        }
                        tracing::debug!(payload = ?info.payload, "adopted another's orphan");
                        self.others.remove(&info.payload);
                        self.republish(sequencer, &info).await;
                    }
                }
            }
            Event::Ready | Event::TurnNotification { .. } => {}
        }
        self.publish_burst(sequencer).await;
    }
}
