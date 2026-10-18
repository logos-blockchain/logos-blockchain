//! Other sequencers of the channel as the network shows them.
//!
//! A writer is an accredited key nobody runs an SDK for. Its inscriptions,
//! note transfers, withdraws and config changes are built by hand and put
//! straight into blocks, like transactions another node's sequencer wrote.

use lb_common_http_client::Slot;
use lb_core::mantle::{
    Note, SignedOps,
    channel::ChannelState,
    ledger::verification_mode::StandardMode,
    ops::channel::{ChannelId, VerifiedChannelKeys},
    transactions::{GasPrices, states::Unverified},
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};

use crate::{
    ledger::SimLedger,
    policy::make_inscription,
    txs::{self, ConfigRules, Signer, TxError, WithdrawPlan},
};

/// What a writer puts into its next block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterOp {
    /// One inscription on the channel tip.
    Inscribe,
    /// Two chained inscriptions in one transaction.
    InscribeTwo,
    /// Re-key one channel note to the writer's recipient key.
    Transfer,
    /// Withdraw 1 out of a channel note to the writer's recipient key.
    Withdraw,
    /// Rotate the accredited keys and change the posting rules.
    Config,
}

/// The op cycle a writer runs through: mostly inscriptions, with note and
/// config changes in between so forks can carry different notes and rules.
const OP_CYCLE: [WriterOp; 8] = [
    WriterOp::Inscribe,
    WriterOp::Inscribe,
    WriterOp::Transfer,
    WriterOp::Inscribe,
    WriterOp::InscribeTwo,
    WriterOp::Withdraw,
    WriterOp::Inscribe,
    WriterOp::Config,
];

/// Posting rules a writer's configs cycle through; never both zero, which
/// would freeze the turn on the first key forever.
const RULES: [(u32, u32); 4] = [(3, 0), (5, 4), (3, 6), (4, 0)];

pub struct Writer {
    pub name: String,
    pub key: Ed25519Key,
    /// Receives the notes this writer transfers and withdraws.
    pub recipient: ZkPublicKey,
    /// Transactions this writer put into blocks, in order.
    pub posted: Vec<SignedOps<Unverified, StandardMode>>,
    built: usize,
}

impl Writer {
    #[must_use]
    pub fn new(name: &str, key: Ed25519Key, recipient: ZkPublicKey) -> Self {
        Self {
            name: name.to_owned(),
            key,
            recipient,
            posted: Vec::new(),
            built: 0,
        }
    }

    /// This writer's index in the channel's accredited list.
    #[must_use]
    pub fn index_in(&self, state: &ChannelState) -> Option<u16> {
        let pk = self.key.public_key().into_unverified();
        let index = state.accredited_keys.iter().position(|key| *key == pk)?;
        u16::try_from(index).ok()
    }

    /// Whether the round robin hands this writer the block at `slot`.
    #[must_use]
    pub fn in_turn(&self, state: &ChannelState, slot: Slot) -> bool {
        self.index_in(state)
            .is_some_and(|index| state.round_robin(slot).0 == index)
    }

    /// The op this writer builds next.
    #[must_use]
    pub const fn next_op(&self) -> WriterOp {
        OP_CYCLE[self.built % OP_CYCLE.len()]
    }

    /// A posted transaction that `ledger` (the block's parent) has not
    /// included and would still accept at `slot`: what a sequencer re-posts
    /// after a reorg.
    #[must_use]
    pub fn reland(
        &self,
        ledger: &SimLedger,
        prices: &GasPrices,
        slot: Slot,
        included: impl Fn(&SignedOps<Unverified, StandardMode>) -> bool,
    ) -> Option<SignedOps<Unverified, StandardMode>> {
        self.posted
            .iter()
            .find(|tx| {
                if included(tx) {
                    return false;
                }
                let mut trial = ledger.clone();
                trial.set_gas_prices(prices.clone());
                trial.apply_tx(tx, slot).is_ok()
            })
            .cloned()
    }

    /// Build the next op against `ledger`, the state of the block's parent;
    /// `keys` is the accredited list a config rotates. Falls back to an
    /// inscription when the op has nothing to work on.
    pub async fn build(
        &mut self,
        node: &crate::SimNode,
        ledger: &SimLedger,
        channel_id: ChannelId,
        funding_pk: ZkPublicKey,
        keys: &VerifiedChannelKeys,
    ) -> Result<SignedOps<Unverified, StandardMode>, TxError> {
        let state = ledger
            .channel(&channel_id)
            .ok_or_else(|| TxError::Assembly("channel does not exist".to_owned()))?;
        let index = self
            .index_in(state)
            .ok_or_else(|| TxError::Assembly(format!("{} is not accredited", self.name)))?;
        let signer = Signer {
            key: &self.key,
            index,
        };
        let n = self.built;
        let notes = ledger.channel_notes(&channel_id);
        let op = match self.next_op() {
            WriterOp::Transfer | WriterOp::Withdraw if notes.is_empty() => WriterOp::Inscribe,
            WriterOp::Withdraw if state.transfer_threshold != 1 => WriterOp::Inscribe,
            WriterOp::Config if state.configuration_threshold != 1 => WriterOp::Inscribe,
            op => op,
        };
        let tx = match op {
            WriterOp::Inscribe | WriterOp::InscribeTwo => {
                let mut payloads = vec![make_inscription(&format!("{} {n}", self.name))];
                if op == WriterOp::InscribeTwo {
                    payloads.push(make_inscription(&format!("{} {n} (2)", self.name)));
                }
                txs::build_funded_custom_tx(
                    node,
                    channel_id,
                    &self.key,
                    funding_pk,
                    &payloads,
                    state.tip_message,
                )
                .await?
                .0
            }
            WriterOp::Transfer => {
                let (id, note) = notes[n % notes.len()];
                txs::build_channel_transfer_tx(
                    node,
                    channel_id,
                    signer,
                    funding_pk,
                    vec![id],
                    vec![Note::new(note.value, self.recipient)],
                )
                .await?
            }
            WriterOp::Withdraw => {
                let (id, note) = notes[n % notes.len()];
                txs::build_channel_withdraw_tx(
                    node,
                    channel_id,
                    signer,
                    funding_pk,
                    &WithdrawPlan {
                        inputs: vec![(id, note.value)],
                        amount: 1,
                        recipient: self.recipient,
                        change_pk: note.pk,
                    },
                )
                .await?
            }
            WriterOp::Config => {
                let mut rotated: Vec<_> = keys.iter().copied().collect();
                let by = 1 + n % rotated.len().max(1);
                rotated.rotate_left(by);
                let (posting_timeframe, posting_timeout) = RULES[n % RULES.len()];
                txs::build_channel_config_tx(
                    node,
                    channel_id,
                    signer,
                    funding_pk,
                    state.config_tip_hash,
                    ConfigRules {
                        keys: VerifiedChannelKeys::try_from(rotated)
                            .map_err(|e| TxError::Assembly(format!("config keys: {e:?}")))?,
                        posting_timeframe,
                        posting_timeout,
                        configuration_threshold: 1,
                        transfer_threshold: 1,
                    },
                )
                .await?
            }
        };
        self.built += 1;
        self.posted.push(tx.clone());
        Ok(tx)
    }

    /// Hashes of this writer's transactions.
    pub fn posted_hashes(&self) -> impl Iterator<Item = lb_zone_sdk::node_types::TxHash> + '_ {
        self.posted
            .iter()
            .map(lb_core::mantle::traits::Hashable::hash)
    }
}
