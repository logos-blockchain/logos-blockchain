//! Canned service behaviour for the conformance suite.
//!
//! Each stub answers the messages a service receives from the HTTP handlers
//! with fixed, representative values. They are not simulations: the point is
//! to drive every handler through its responses so they can be checked
//! against the published schema. Identifiers listed in [`missing`] make the
//! lookups that take them report "not found", so 404 responses are
//! exercised too.

use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use bytes::Bytes;
use futures::StreamExt as _;
use lb_api_service::http::consensus::Cryptarchia;
use lb_binary_codec::{bincode::SerializeOp as _, canonical::BinaryDecodeExt as _};
use lb_blend_service::message::{NetworkInfo, ProxyServiceMessage, ServiceMessage};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockInfo};
use lb_chain_leader_service::LeaderMsg;
use lb_chain_service::{
    ChainServiceInfo, ConsensusMsg, CryptarchiaInfo, PhaseTag, ProcessedBlockEvent, Query,
    StartingState,
};
use lb_core::{
    block::{Block, BlockTransactions, UncleHeaders},
    events::Events,
    header::HeaderId,
    mantle::{
        SignedOps,
        ledger::verification_mode::StandardMode,
        ops::leader_claim::{VoucherCm, VoucherNullifier},
        traits::GenesisTx as _,
        transactions::states::{Preverified, Unverified},
    },
    proofs::leader_proof::Groth16LeaderProof,
    sdp::{Declaration, DeclarationId, DeclarationMessage},
};
use lb_cryptarchia_engine::{Epoch, Slot, State};
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature, ZkPublicKey, ZkSignature};
use lb_ledger::LedgerState;
use lb_network_service::{
    backends::libp2p::{Command, Libp2pInfo, NetworkCommand},
    message::NetworkMsg,
};
use lb_pow_service::{
    AutoClaimStatus, AutoClaimTick, ClaimableRewardsInfo, PoWServiceMessage, PoWStatus,
};
use lb_sdp_service::SdpMessage;
use lb_storage_service::StorageMsg;
use lb_time_service::{TimeServiceInfo, TimeServiceMessage};
use lb_tracing_service::TracingMessage;
use lb_tx_service::{MempoolMetrics, MempoolMsg, backend::Status};
use lb_wallet_service::{
    ClaimableVoucherInfo, ClaimableVouchersInfo, LeaderAgedNoteInfo, LeaderAgedNotesInfo,
    TipResponse, WalletMsg,
};
use libp2p::PeerId;
use overwatch::services::{AsServiceId, ServiceData, relay::AnyMessage};
use tokio::{runtime::Handle, sync::broadcast};

use super::harness::relay;
use crate::{
    BlendService, BlockBroadcastService, CryptarchiaLeaderService, MempoolService, NetworkService,
    PoWService, RuntimeServiceId, StorageService, TimeService, TracingService, WalletService,
    generic_services::SdpService,
};

type Message<Service> = <Service as ServiceData>::Message;
type Tx = SignedOps<Preverified, StandardMode>;

fn is<Service>(service_id: RuntimeServiceId) -> bool
where
    RuntimeServiceId: AsServiceId<Service>,
{
    service_id == <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID
}

pub fn relay_for(runtime: &Handle, service_id: RuntimeServiceId) -> Option<AnyMessage> {
    Some(if is::<Cryptarchia<RuntimeServiceId>>(service_id) {
        relay::<Message<Cryptarchia<RuntimeServiceId>>>(runtime, chain)
    } else if is::<StorageService>(service_id) {
        relay::<Message<StorageService>>(runtime, storage)
    } else if is::<MempoolService>(service_id) {
        relay::<Message<MempoolService>>(runtime, mempool)
    } else if is::<TimeService>(service_id) {
        relay::<Message<TimeService>>(runtime, time)
    } else if is::<BlockBroadcastService>(service_id) {
        relay::<Message<BlockBroadcastService>>(runtime, block_broadcast)
    } else if is::<NetworkService>(service_id) {
        relay::<Message<NetworkService>>(runtime, network)
    } else if is::<BlendService>(service_id) {
        relay::<Message<BlendService>>(runtime, blend)
    } else if is::<SdpService<RuntimeServiceId>>(service_id) {
        relay::<Message<SdpService<RuntimeServiceId>>>(runtime, sdp)
    } else if is::<WalletService>(service_id) {
        relay::<Message<WalletService>>(runtime, wallet)
    } else if is::<PoWService>(service_id) {
        relay::<Message<PoWService>>(runtime, pow)
    } else if is::<CryptarchiaLeaderService>(service_id) {
        relay::<Message<CryptarchiaLeaderService>>(runtime, leader)
    } else if is::<TracingService>(service_id) {
        relay::<Message<TracingService>>(runtime, tracing)
    } else {
        return None;
    })
}

/// Identifiers the stubs treat as unknown.
pub mod missing {
    pub const HEADER_BYTE: u8 = 0xee;
    pub const HEADER_ID: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
}

pub fn header_id(byte: u8) -> HeaderId {
    HeaderId::from([byte; 32])
}

fn is_missing(id: &HeaderId) -> bool {
    *id == header_id(missing::HEADER_BYTE)
}

pub fn zero_hex() -> String {
    "00".repeat(32)
}

/// A valid Ed25519 public key (the base point); all-zero keys are rejected.
pub const ED25519_PUBLIC_KEY: &str =
    "5866666666666666666666666666666666666666666666666666666666666666";

fn peer_id() -> PeerId {
    PeerId::from_public_key(
        &libp2p::identity::Keypair::ed25519_from_bytes([7; 32])
            .expect("valid key")
            .public(),
    )
}

fn reply<T>(sender: tokio::sync::oneshot::Sender<T>, value: T) {
    drop(sender.send(value));
}

pub fn chain_info() -> ChainServiceInfo {
    ChainServiceInfo {
        cryptarchia_info: CryptarchiaInfo {
            lib: header_id(1),
            lib_slot: Slot::from(10),
            tip: header_id(2),
            slot: Slot::from(20),
            height: 15,
            state: State::Online,
        },
        phase: PhaseTag::ProlongedBootstrapPeriod,
    }
}

/// The genesis ledger of the bundled deployment. Its inscription channel,
/// `ChannelId([0; 32])`, exists, so channel lookups have something to find.
static LEDGER: LazyLock<LedgerState> = LazyLock::new(|| {
    let deployment = crate::config::DeploymentSettings::default();
    let rewards = deployment.blend_reward_params();
    let (settings, _, _) = crate::config::cryptarchia::ServiceConfig {
        user: crate::config::cryptarchia::serde::Config::with_required_values(
            crate::config::cryptarchia::serde::RequiredValues {
                funding_pk: ZkPublicKey::zero(),
            },
        ),
        deployment: deployment.cryptarchia,
    }
    .into_cryptarchia_services_settings(
        rewards,
        lb_services_utils::overwatch::RecoveryData::default(),
    );
    let StartingState::Genesis { genesis_block } = settings.starting_state else {
        unreachable!("the bundled deployment starts from genesis");
    };
    let genesis_tx = genesis_block.genesis_tx().clone();
    let epoch_nonce = genesis_tx.cryptarchia_parameter().epoch_nonce;
    LedgerState::from_genesis_tx::<HeaderId>(genesis_tx, &settings.config, epoch_nonce)
        .expect("genesis ledger builds")
        .0
});

/// A block that passes the checks the API applies when decoding stored
/// blocks (the leader signature, not the leadership proof).
static BLOCK: LazyLock<Block<Tx>> = LazyLock::new(|| {
    let leader_key = Ed25519Key::from_bytes(&[1; 32]);
    // Layout: proof (128B) || entropy contribution (32B) || leader key (32B)
    // || voucher commitment (32B).
    let proof_bytes = [
        &[0u8; 160][..],
        leader_key.public_key().as_bytes(),
        &[0u8; 32],
    ]
    .concat();
    Block::create(
        header_id(1),
        Slot::new(5),
        UncleHeaders::empty(),
        Groth16LeaderProof::decode_all(&proof_bytes).expect("leader proof decodes"),
        BlockTransactions::empty(),
        &leader_key,
    )
    .expect("block builds")
});

fn block_bytes() -> Bytes {
    Bytes::try_from(BLOCK.clone()).expect("block encodes")
}

fn tx_bytes() -> Bytes {
    SignedOps::<Unverified, StandardMode>::empty()
        .to_bytes()
        .expect("transaction encodes")
}

fn chain(message: Message<Cryptarchia<RuntimeServiceId>>) {
    let ConsensusMsg::Query(query) = message else {
        panic!("handlers only send chain queries");
    };
    match query {
        Query::Info { reply_channel } => reply(reply_channel, chain_info()),
        Query::GetHeaders { reply_channel, .. } => reply(
            reply_channel,
            futures::stream::iter([Ok(header_id(2)), Ok(header_id(1))]).boxed(),
        ),
        Query::GetLedgerState {
            block_id,
            reply_channel,
        } => reply(
            reply_channel,
            (!is_missing(&block_id)).then(|| LEDGER.clone()),
        ),
        Query::GetSdpDeclarations { reply_channel } | Query::GetSdpSnapshot { reply_channel } => {
            let message = DeclarationMessage::sample();
            reply(
                reply_channel,
                HashMap::from([(message.id(), Declaration::new(Epoch::from(1), &message))]),
            );
        }
        Query::GetBlockEvents { id, reply_channel } => {
            reply(reply_channel, (!is_missing(&id)).then(Events::new));
        }
        Query::NewBlockSubscribe { sender } => {
            let (events, receiver) = broadcast::channel(4);
            drop(events.send(ProcessedBlockEvent {
                block_id: BLOCK.header().id(),
                block_slot: BLOCK.header().slot(),
                tip: header_id(2),
                tip_slot: Slot::from(20),
                lib: header_id(1),
                lib_slot: Slot::from(10),
            }));
            reply(sender, receiver);
        }
        other => panic!("unstubbed chain query: {other:?}"),
    }
}

fn storage(message: Message<StorageService>) {
    match message {
        StorageMsg::GetBlock {
            header_id,
            response_tx,
        } => reply(response_tx, (!is_missing(&header_id)).then(block_bytes)),
        StorageMsg::ScanImmutableBlockIds { response_tx, .. }
        | StorageMsg::ScanImmutableBlockIdsReverse { response_tx, .. } => {
            reply(response_tx, vec![BLOCK.header().id()]);
        }
        StorageMsg::GetTransactions {
            tx_hashes,
            response_tx,
        } => {
            let found: Vec<Bytes> = tx_hashes
                .into_iter()
                .filter(|hash| hash.0 != [missing::HEADER_BYTE; 32])
                .map(|_| tx_bytes())
                .collect();
            reply(response_tx, futures::stream::iter(found).boxed());
        }
        _ => panic!("unstubbed storage message"),
    }
}

fn mempool(message: Message<MempoolService>) {
    match message {
        MempoolMsg::Add { reply_channel, .. } => reply(reply_channel, Ok(())),
        MempoolMsg::View { reply_channel, .. } => {
            reply(reply_channel, futures::stream::iter([Tx::empty()]).boxed());
        }
        MempoolMsg::Metrics { reply_channel } => reply(
            reply_channel,
            MempoolMetrics {
                pending_items: 1,
                last_item_timestamp: 1_700_000_000,
            },
        ),
        MempoolMsg::Status {
            items,
            reply_channel,
        } => reply(
            reply_channel,
            items.iter().map(|_| Status::Pending).collect(),
        ),
        _ => panic!("unstubbed mempool message"),
    }
}

fn time(message: Message<TimeService>) {
    match message {
        TimeServiceMessage::Info { sender } => reply(
            sender,
            Ok(TimeServiceInfo {
                slot_duration_ms: 1_000,
                genesis_time_unix_ms: 1_700_000_000_000,
                current_slot: Slot::from(20),
                current_epoch: Epoch::from(1),
            }),
        ),
        _ => panic!("unstubbed time message"),
    }
}

fn block_broadcast(message: Message<BlockBroadcastService>) {
    match message {
        BlockBroadcastMsg::SubscribeToFinalizedBlocks { result_sender } => {
            let (blocks, receiver) = broadcast::channel(4);
            drop(blocks.send(BlockInfo {
                height: 14,
                header_id: header_id(1),
            }));
            reply(result_sender, receiver);
        }
        BlockBroadcastMsg::BroadcastFinalizedBlock(_) => {
            panic!("handlers do not broadcast blocks")
        }
    }
}

fn network(message: Message<NetworkService>) {
    let NetworkMsg::Process(Command::Network(command)) = message else {
        panic!("unstubbed network message");
    };
    let peer = peer_id();
    match command {
        NetworkCommand::Info { reply: sender } => reply(
            sender,
            Libp2pInfo {
                listen_addresses: vec![
                    "/ip4/127.0.0.1/udp/3000/quic-v1"
                        .parse()
                        .expect("valid multiaddr"),
                ],
                peer_id: peer,
                connected_peers: vec![peer],
                n_peers: 1,
                n_connections: 1,
                n_pending_connections: 0,
                discovered_peers: vec![peer],
                n_discovered_peers: 1,
            },
        ),
        NetworkCommand::Connect(dial) => reply(dial.result_sender, Ok(peer)),
        NetworkCommand::ConnectedPeers { reply: sender } => reply(sender, HashSet::from([peer])),
        _ => panic!("unstubbed network command"),
    }
}

fn blend(message: Message<BlendService>) {
    match message {
        ProxyServiceMessage::Inner(ServiceMessage::GetNetworkInfo { reply: sender }) => {
            reply(
                sender,
                Some(NetworkInfo {
                    node_id: peer_id(),
                    core_info: None,
                }),
            );
        }
        ProxyServiceMessage::Inner(ServiceMessage::GetPendingTransactions { reply: sender }) => {
            reply(sender, vec![tx_bytes().to_vec()]);
        }
        ProxyServiceMessage::Inner(ServiceMessage::Blend(_)) => {}
        ProxyServiceMessage::JoinAsCore { reply: sender, .. } => {
            reply(sender, Ok(DeclarationId([3; 32])));
        }
    }
}

fn sdp(message: Message<SdpService<RuntimeServiceId>>) {
    match message {
        SdpMessage::PostDeclaration { reply_channel, .. } => {
            reply(reply_channel, Ok(DeclarationId([3; 32])));
        }
        SdpMessage::PostActivity { .. } | SdpMessage::PostWithdrawal { .. } => {}
        SdpMessage::SetCurrentDeclarationId { reply_channel, .. } => reply(reply_channel, Ok(())),
    }
}

fn wallet(message: Message<WalletService>) {
    let tip = header_id(2);
    match message {
        WalletMsg::GetBalance { resp_tx, .. } => reply(
            resp_tx,
            Ok(TipResponse {
                tip,
                response: Some(lb_wallet::WalletBalance {
                    balance: 10,
                    notes: HashMap::new(),
                }),
            }),
        ),
        WalletMsg::FundTx {
            tx_builder,
            resp_tx,
            ..
        } => reply(
            resp_tx,
            Ok(TipResponse {
                tip,
                response: tx_builder,
            }),
        ),
        WalletMsg::SignTx { resp_tx, .. } => reply(
            resp_tx,
            Ok(TipResponse {
                tip,
                response: Tx::empty(),
            }),
        ),
        WalletMsg::SignTxWithEd25519 { resp_tx, .. } => {
            reply(resp_tx, Ok(Ed25519Signature::from_bytes(&[0; 64])));
        }
        WalletMsg::SignTxWithZk { resp_tx, .. } => reply(
            resp_tx,
            Ok(ZkSignature::decode_all(&[0; 128]).expect("zk signature decodes")),
        ),
        WalletMsg::GetLeaderAgedNotesInfo { resp_tx, .. } => reply(
            resp_tx,
            Ok(TipResponse {
                tip,
                response: LeaderAgedNotesInfo {
                    notes: vec![LeaderAgedNoteInfo {
                        note_id: lb_core::mantle::NoteId(ZkPublicKey::zero().into_inner()),
                        value: 10,
                        public_key: ZkPublicKey::zero(),
                    }],
                    total_value: 10,
                },
            }),
        ),
        WalletMsg::GetClaimableVouchers { resp_tx, .. } => reply(
            resp_tx,
            Ok(TipResponse {
                tip,
                response: ClaimableVouchersInfo {
                    vouchers: vec![ClaimableVoucherInfo {
                        commitment: VoucherCm::default(),
                        nullifier: VoucherNullifier::default(),
                    }],
                    reward_amount: 5,
                    total_claimable: 5,
                },
            }),
        ),
        _ => panic!("unstubbed wallet message"),
    }
}

fn pow(message: Message<PoWService>) {
    match message {
        PoWServiceMessage::StartMining
        | PoWServiceMessage::StopMining
        | PoWServiceMessage::StartAutoClaim
        | PoWServiceMessage::StopAutoClaim => {}
        PoWServiceMessage::Claim { response, .. } => {
            reply(response, Ok(Some(lb_core::mantle::TxHash::from([4; 32]))));
        }
        PoWServiceMessage::ClaimableRewardsInfo { response } => reply(
            response,
            ClaimableRewardsInfo {
                claimable_tickets: 1,
                slots_until_expiry: vec![Slot::from(3)],
            },
        ),
        PoWServiceMessage::Status { response } => reply(
            response,
            PoWStatus {
                is_mining: true,
                are_rewards_enabled: true,
                auto_claim: AutoClaimStatus {
                    is_armed: false,
                    tick: AutoClaimTick::default(),
                    targets: Vec::new(),
                },
            },
        ),
    }
}

fn leader(message: Message<CryptarchiaLeaderService>) {
    match message {
        LeaderMsg::Claim { sender } => {
            reply(sender, Ok(lb_core::mantle::TxHash::from([4; 32])));
        }
        LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { .. } => {
            panic!("handlers do not subscribe to winning slots")
        }
    }
}

fn tracing(message: Message<TracingService>) {
    match message {
        TracingMessage::ReloadFilter { reply_channel, .. } => reply(reply_channel, Ok(())),
    }
}
