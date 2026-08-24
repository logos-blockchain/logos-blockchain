//! Configurable [`adapter::Node`] mock and shared builders for unit tests.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::StreamExt as _;
use lb_common_http_client::{
    ApiBlock, ApiHeader, BlockInfo, ChainServiceInfo, CryptarchiaInfo, Events, PhaseTag,
    ProcessedBlockEvent, Slot, State, TimeInfo,
};
use lb_core::{
    events::{DepositNote, Event as ChainEvent, TxEvent, TxEventPayload},
    header::{ContentId, HeaderId},
    mantle::{
        Op, RawMantleTx, SignedMantleTx, Value,
        channel::ChannelState,
        gas::GasCost,
        ops::{
            OpId as _, OpProof,
            channel::{
                ChannelId, MsgId,
                config::Keys,
                deposit::DepositOp,
                inscribe::{Inscription, InscriptionOp},
            },
        },
        traits::Hashable as _,
        transactions::{Ops, OpsProofs, states::Unverified},
    },
    proofs::leader_proof::Groth16LeaderProof,
};
use lb_groth16::Fr;
use lb_http_api_common::bodies::wallet::fund::{WalletFundRequestBody, WalletFundResponseBody};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};
use tokio::sync::{mpsc, watch};

use crate::{
    ZoneMessage,
    adapter::{self, BoxStream},
    sequencer::FundingConfig,
};

/// One scripted `block_stream` connection: serve `events`, then `then`.
#[derive(Clone)]
pub struct StreamScript {
    pub events: Vec<ProcessedBlockEvent>,
    pub then: StreamEnd,
}

/// How a scripted stream behaves after its events.
#[derive(Clone, Copy)]
pub enum StreamEnd {
    /// Stay open without further events.
    Hang,
    /// End the stream — a dropped connection.
    End,
}

/// Wrap stream scripts for [`MockNode::scripts`].
pub fn scripts(scripts: Vec<StreamScript>) -> Arc<Mutex<VecDeque<StreamScript>>> {
    Arc::new(Mutex::new(scripts.into()))
}

/// Configurable mock node. Construct with struct-update syntax over
/// [`MockNode::default`], overriding only what the scenario needs.
#[derive(Clone)]
pub struct MockNode {
    /// Served by `channel_state()`.
    pub channel_state: Option<ChannelState>,
    /// Optional gate for pausing `channel_state()` calls in cancellation tests.
    pub channel_state_gate: Option<watch::Receiver<bool>>,
    /// Optional notification sent whenever `channel_state()` is called.
    pub channel_state_calls: Option<mpsc::UnboundedSender<()>>,
    /// LIB and tip ids reported by `consensus_info()`.
    pub lib: HeaderId,
    pub tip: HeaderId,
    /// LIB slot (and current slot) reported by `consensus_info()`.
    pub lib_slot: Slot,
    /// Successive `block_stream()` connections consume these; the last
    /// script is reused once the queue would run dry.
    pub scripts: Arc<Mutex<VecDeque<StreamScript>>>,
    /// Served by `block()`, keyed by header id; unknown ids yield `None`.
    pub blocks: Vec<ApiBlock>,
    /// Served by `immutable_blocks()`, filtered by the queried slot range.
    pub immutable: Vec<ApiBlock>,
    /// Optional gate for pausing `immutable_blocks()` calls in cancellation
    /// tests.
    pub immutable_blocks_gate: Option<watch::Receiver<bool>>,
    /// Optional notification sent whenever `immutable_blocks()` is called.
    pub immutable_blocks_calls: Option<mpsc::UnboundedSender<()>>,
    /// Served by `zone_messages_in_blocks()`, filtered by the queried slot
    /// range.
    pub zone_messages: Vec<(ZoneMessage, Slot)>,
    /// When set, `block_stream` errors while `false` and open streams end on
    /// the next `true -> false` transition, driving the reconnect path.
    pub up: Option<watch::Receiver<bool>>,
    /// Receives every `post_transaction` tx.
    pub posted: Option<mpsc::Sender<SignedMantleTx<Unverified>>>,
    /// Served by `block_events()`, keyed by block id; absent ids yield `None`.
    pub events: HashMap<HeaderId, Events>,
    /// Receives the priority-fee percentages from funding requests.
    pub funding_priority_fees: Option<mpsc::Sender<u64>>,
}

impl Default for MockNode {
    fn default() -> Self {
        Self {
            channel_state: Some(single_key_channel_state()),
            channel_state_gate: None,
            channel_state_calls: None,
            lib: header_id(0),
            tip: header_id(0),
            lib_slot: Slot::genesis(),
            scripts: scripts(vec![StreamScript {
                events: vec![live_event(&api_block(1, 0, 1, Vec::new()))],
                then: StreamEnd::Hang,
            }]),
            blocks: Vec::new(),
            immutable: Vec::new(),
            immutable_blocks_gate: None,
            immutable_blocks_calls: None,
            zone_messages: Vec::new(),
            up: None,
            posted: None,
            events: HashMap::new(),
            funding_priority_fees: None,
        }
    }
}

impl MockNode {
    /// Default node plus a receiver for its posted transactions.
    pub fn with_posted_channel() -> (Self, mpsc::Receiver<SignedMantleTx<Unverified>>) {
        let (tx, rx) = mpsc::channel(10);
        (
            Self {
                posted: Some(tx),
                ..Self::default()
            },
            rx,
        )
    }

    fn next_script(&self) -> StreamScript {
        let mut queue = self.scripts.lock().expect("mock scripts lock");
        if queue.len() > 1 {
            queue.pop_front().expect("len checked")
        } else {
            queue.front().cloned().unwrap_or(StreamScript {
                events: Vec::new(),
                then: StreamEnd::Hang,
            })
        }
    }
}

#[async_trait]
impl adapter::Node for MockNode {
    async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
        Ok(ChainServiceInfo {
            cryptarchia_info: CryptarchiaInfo {
                lib: self.lib,
                lib_slot: self.lib_slot,
                tip: self.tip,
                slot: self.lib_slot,
                height: 0,
                state: State::Online,
            },
            phase: PhaseTag::Following,
        })
    }

    async fn time_info(&self) -> Result<TimeInfo, lb_common_http_client::Error> {
        Ok(TimeInfo {
            slot_duration_ms: 1_000,
            genesis_time_unix_ms: 0,
            current_slot: 0,
            current_epoch: 0,
        })
    }

    async fn channel_state(
        &self,
        _channel_id: ChannelId,
    ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
        if let Some(calls) = &self.channel_state_calls {
            let _ = calls.send(());
        }

        if let Some(gate) = &self.channel_state_gate {
            let mut gate = gate.clone();
            while !*gate.borrow_and_update() {
                gate.changed().await.map_err(|_| {
                    lb_common_http_client::Error::Client(
                        "channel-state test gate closed".to_owned(),
                    )
                })?;
            }
        }

        Ok(self.channel_state.clone())
    }

    async fn block_stream(
        &self,
    ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
        if let Some(up_rx) = &self.up
            && !*up_rx.borrow()
        {
            return Err(lb_common_http_client::Error::Client("node down".to_owned()));
        }
        let script = self.next_script();
        let events = futures::stream::iter(script.events);
        if let Some(up_rx) = &self.up {
            // Stay open until the node goes down, then end so the sequencer
            // re-enters `ensure_connected` (where `block_stream` errors).
            let up_rx = up_rx.clone();
            let until_down = futures::stream::once(async move {
                let mut up_rx = up_rx;
                while *up_rx.borrow_and_update() {
                    if up_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .filter_map(async |()| None::<ProcessedBlockEvent>);
            return Ok(Box::pin(events.chain(until_down)));
        }
        Ok(match script.then {
            StreamEnd::Hang => Box::pin(events.chain(futures::stream::pending())),
            StreamEnd::End => Box::pin(events),
        })
    }

    async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
        Ok(Box::pin(futures::stream::pending()))
    }

    async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
        Ok(self.blocks.iter().find(|b| b.header.id == id).cloned())
    }

    async fn block_events(
        &self,
        id: HeaderId,
    ) -> Result<Option<Events>, lb_common_http_client::Error> {
        Ok(self.events.get(&id).cloned())
    }

    async fn immutable_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
    ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
        if let Some(calls) = &self.immutable_blocks_calls {
            let _ = calls.send(());
        }

        if let Some(gate) = &self.immutable_blocks_gate {
            let mut gate = gate.clone();
            while !*gate.borrow_and_update() {
                gate.changed().await.map_err(|_| {
                    lb_common_http_client::Error::Client(
                        "immutable-blocks test gate closed".to_owned(),
                    )
                })?;
            }
        }

        Ok(self
            .immutable
            .iter()
            .filter(|b| b.header.slot >= slot_from && b.header.slot <= slot_to)
            .cloned()
            .collect())
    }

    async fn zone_messages_in_block(
        &self,
        _id: HeaderId,
        _channel_id: ChannelId,
    ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        _channel_id: ChannelId,
    ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
        let messages: Vec<_> = self
            .zone_messages
            .iter()
            .filter(|(_, slot)| *slot >= slot_from && *slot <= slot_to)
            .cloned()
            .collect();
        Ok(Box::pin(futures::stream::iter(messages)))
    }

    async fn post_transaction(
        &self,
        tx: SignedMantleTx<Unverified>,
    ) -> Result<(), lb_common_http_client::Error> {
        if let Some(posted) = &self.posted {
            posted.send(tx).await.expect("posted receiver alive");
        }
        Ok(())
    }

    async fn fund_tx(
        &self,
        request: WalletFundRequestBody,
    ) -> Result<WalletFundResponseBody, lb_common_http_client::Error> {
        if let Some(priority_fees) = &self.funding_priority_fees {
            priority_fees
                .send(request.priority_fee_percent)
                .await
                .expect("funding percentage receiver alive");
        }
        // Fee-less passthrough: build the request's ops unchanged, as the
        // node would at zero gas price.
        Ok(WalletFundResponseBody {
            tip: header_id(0),
            funded_tx: request.tx_builder.build().map_err(|e| {
                lb_common_http_client::Error::Server(format!("mock funding failed: {e:?}"))
            })?,
            transfer_proof: None,
        })
    }
}

/// Funding config backed by a fixture key; [`MockNode::fund_tx`] ignores it
/// and returns the ops unchanged.
#[must_use]
pub fn funding_config() -> FundingConfig {
    FundingConfig {
        funding_pk: Fr::from(1u64).into(),
        max_tx_fee: GasCost::new(u64::MAX),
        priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
    }
}

/// Channel state with a single accredited key (the zero Ed25519 key) and all
/// thresholds at 1.
pub fn single_key_channel_state() -> ChannelState {
    ChannelState {
        accredited_keys: Keys::from(Ed25519Key::from_bytes(&[0; 32]).public_key()).into(),
        configuration_threshold: 1,
        tip_message: MsgId::root(),
        config_tip_hash: MsgId::root(),
        tip_slot: Slot::default(),
        tip_sequencer: 0,
        tip_sequencer_starting_slot: Slot::default(),
        posting_timeframe: 0u32.into(),
        posting_timeout: 0u32.into(),
        transfer_threshold: 1,
    }
}

pub fn header_id(n: u8) -> HeaderId {
    let mut bytes = [0u8; 32];
    bytes[0] = n;
    HeaderId::from(bytes)
}

pub fn api_block(
    id: u8,
    parent: u8,
    slot: u64,
    transactions: Vec<SignedMantleTx<Unverified>>,
) -> ApiBlock {
    ApiBlock {
        header: ApiHeader {
            id: header_id(id),
            parent_block: header_id(parent),
            slot: slot.into(),
            body_root: ContentId::from([0; 32]),
            proof_of_leadership: Groth16LeaderProof::genesis(),
        },
        uncle_headers: Vec::new(),
        transactions,
    }
}

/// A live stream event delivering `block` with the tip at that same block
/// and LIB pinned at genesis.
pub fn live_event(block: &ApiBlock) -> ProcessedBlockEvent {
    ProcessedBlockEvent {
        block: block.clone(),
        tip: block.header.id,
        tip_slot: block.header.slot,
        lib: header_id(0),
        lib_slot: Slot::genesis(),
    }
}

/// Build a `SignedMantleTx` carrying the given ops, with placeholder proofs.
/// Suitable for tests that only care about op extraction, not verification.
pub fn unverified_tx_with_ops(ops: Vec<Op>) -> SignedMantleTx<Unverified> {
    let n = ops.len();
    let mantle_tx = RawMantleTx(Ops::try_from(ops).expect("ops fit"));
    SignedMantleTx::new(
        mantle_tx,
        OpsProofs::new_unchecked(vec![OpProof::Ed25519Sig(Ed25519Signature::zero()); n]),
    )
}

/// An inscription op signed by the zero Ed25519 key.
pub fn inscribe_op(channel_id: ChannelId, parent: MsgId, payload: &[u8]) -> InscriptionOp {
    InscriptionOp {
        channel_id,
        inscription: Inscription::new_unchecked(payload.to_vec()),
        parent,
        signer: Ed25519Key::from_bytes(&[0u8; 32]).public_key(),
    }
}

/// A `Deposit` block event matching `op` inside `tx`, recreating `notes`.
pub fn deposit_event(
    tx: &SignedMantleTx<Unverified>,
    op: &DepositOp,
    amount: Value,
    notes: Vec<DepositNote>,
) -> Events {
    Events::from(ChainEvent::Tx(TxEvent::new(
        tx.mantle_tx().hash(),
        op.op_id(),
        TxEventPayload::Deposit {
            channel_id: op.channel_id,
            amount,
            metadata: op.metadata.clone(),
            notes: notes.try_into().expect("bounded note count"),
        },
    )))
}
