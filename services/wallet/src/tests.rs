use core::num::NonZeroU32;
use std::time::Duration;

use lb_binary_codec::canonical::BinaryEncode as _;
use lb_chain_service::{ConsensusMsg, Query};
use lb_core::{
    block::{BlockTransactions, UncleHeaders},
    era::AcceptedForkDigests,
    events::{BlockEvent, Event},
    mantle::{
        GenesisParameters, GenesisTime,
        gas::Gas,
        note_set::{CommitmentMmr, NoteSetUpdate},
        ops::{
            channel::{
                MsgId,
                inscribe::{Inscription, InscriptionOp, MiniBlocks},
            },
            sdp::{SDPActiveOp, SDPDeclareOp},
            transfer::TransferOp,
        },
    },
    proofs::leader_proof::Groth16LeaderProof,
    sdp::ServiceParameters,
};
use lb_cryptarchia_engine::{EpochConfig, Slot};
use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof, Field as _};
use lb_key_management_system_service::{
    KMSService, keys::UnverifiedEd25519PublicKey, message::KMSMessage,
};
use lb_ledger::{
    config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{ServiceRewardsParameters, rewards},
};
use lb_storage_service::api::requests::StorageMsg;
use lb_time::era::Era;
use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};
use overwatch::services::{
    relay::{InboundRelay, OutboundRelay, Relay},
    state::{NoOperator, NoState, ServiceState as _},
};
use tokio::{
    sync::{mpsc, watch},
    time::{sleep, timeout},
};

use super::*;
use crate::states::tests::era_schedule;

/// The costs of the deployments' current gas profile.
pub const GAS_PROFILE: GasProfile = GasProfile {
    transfer: Gas::new(10),
    program_register: Gas::new(15114),
    channel_config: Gas::new(24),
    channel_inscribe: Gas::new(26),
    channel_deposit: Gas::new(26),
    channel_withdraw: Gas::new(33),
    sdp_declare: Gas::new(1693),
    sdp_withdraw: Gas::new(0),
    sdp_active: Gas::new(15117),
    claim_pow_reward: Gas::new(1506),
    kilobyte: Gas::new(30),
    note_set: Gas::new(8961),
    proof: Gas::new(4177),
    root: Gas::new(2332),
    buffered: Gas::new(60),
    nullifier: Gas::new(1758),
    commitment: Gas::new(58),
    signature: Gas::new(557),
    user_step: Gas::new(28),
    program_deposit: Gas::new(144),
    program_step: Gas::new(13528),
    opening: Gas::new(470),
};

#[test]
fn op_proof_pairs_each_operation_with_its_proof_type() {
    let zk = ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[1u8; 128]));
    let signature = Ed25519Signature::from_bytes(&[2u8; 64]);
    let transfer = TransferOp::sample();
    let declare = SDPDeclareOp::sample();
    let active = SDPActiveOp::sample();
    assert_eq!(
        op_proof(&OpRef::Transfer(&transfer), Some(zk), None).unwrap(),
        OpProof::ZkTransfer(zk)
    );
    assert_eq!(
        op_proof(&OpRef::SDPDeclare(&declare), Some(zk), Some(signature)).unwrap(),
        OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::new(zk, signature))
    );
    assert_eq!(
        op_proof(&OpRef::SDPActive(&active), None, Some(signature)).unwrap(),
        OpProof::Ed25519Sig(signature)
    );
    assert!(op_proof(&OpRef::Transfer(&transfer), None, None).is_err());
}

/// Long enough for a task that isn't held back to have got this far.
const SETTLE_TIME: Duration = Duration::from_millis(100);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

type Tx = SignedOps<Preverified, StandardMode>;
type Kms = KMSService<KmsBackend, RuntimeServiceId>;
type Service = WalletService<Kms, Chain, Tx, (), RuntimeServiceId>;
type Signer = TxSigner<Kms, RuntimeServiceId>;

struct Chain;

impl ServiceData for Chain {
    type Settings = ();
    type State = NoState<()>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ConsensusMsg<Tx>;
}

#[derive(Clone, Copy, Debug)]
struct RuntimeServiceId;

impl std::fmt::Display for RuntimeServiceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl AsServiceId<Kms> for RuntimeServiceId {
    const SERVICE_ID: Self = Self;
}

impl AsServiceId<Chain> for RuntimeServiceId {
    const SERVICE_ID: Self = Self;
}

/// The genesis parameters of a test chain: the supply cap and the genesis gas
/// prices of the deployments.
fn genesis_parameters() -> GenesisParameters {
    GenesisParameters {
        chain_id: "test".to_owned().try_into().unwrap(),
        start_time: GenesisTime::new(0),
        epoch_nonce: Fr::ZERO,
        max_supply: 10_000_000_000_000_000_000,
        pow_reward_pool: 0,
        rewards_reserve: 1_000_000_000_000_000_000,
        storage_gas_price: NonZero::new(1_000_000_000).unwrap(),
        execution_gas_price: NonZero::new(1_000_000_000).unwrap(),
    }
}

fn ledger_config() -> lb_ledger::Config {
    let mut service_params = HashMap::new();
    service_params.insert(
        lb_core::sdp::ServiceType::BlendNetwork,
        ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
        },
    );
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
        epoch_period_nonce_buffer: 3.try_into().unwrap(),
        epoch_period_nonce_stabilization: 4.try_into().unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        5.try_into().unwrap(),
        ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
        1.0.try_into().unwrap(),
        12,
        4,
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    lb_ledger::Config {
        epoch_config,
        consensus_config,
        sdp_config: lb_ledger::mantle::sdp::Config {
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN).unwrap(),
                    cover_replication_factor: 0,
                    num_blend_layers: 3.try_into().unwrap(),
                    minimum_network_size: 1.try_into().unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: lb_core::sdp::MinStake { threshold: 1 },
        },
        max_transactions: 1024,
        max_transactions_size: 2 * 1024 * 1024,
        max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
        root_window: 1024,
        channel_withdraw_delay: 86_400.into(),
        execution_market: lb_ledger::config::ExecutionMarketConfig {
            max_gas_per_block: lb_ledger::config::MaxGasPerBlock::new(3_137_798).unwrap(),
            fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
        },
        storage_market: lb_ledger::config::StorageMarketConfig {
            max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
        },
        block_rewards: lb_ledger::config::BlockRewardsConfig::new(
            lb_ledger::config::BlockRewardsParameters {
                max_emission_rate: ProperFraction::new(1, 100.try_into().unwrap()).unwrap(),
                min_emission_rate: NonNegativeRatio::new(0, 1.try_into().unwrap()),
                stake_target: ProperFraction::new(3, 10.try_into().unwrap()).unwrap(),
                pooled_fees_target: NonNegativeRatio::new(1, 1.try_into().unwrap()),
                stake_deviation_weight: NonNegativeRatio::new(1, 4.try_into().unwrap()),
                pooled_fees_weight: NonNegativeRatio::new(1, 1.try_into().unwrap()),
                pooled_fees_window: 120.try_into().unwrap(),
                blend_share: ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            },
            10_000_000_000_000_000_000,
            1.try_into().unwrap(),
            ProperFraction::new(1, 30.try_into().unwrap()).unwrap(),
        )
        .unwrap(),
        gas_profile: GAS_PROFILE,
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<19>(),
                damping_den_offset: 0,
                damping_num: 1.try_into().unwrap(),
                max_step: 1.try_into().unwrap(),
                target_transactions_per_block: 1.try_into().unwrap(),
            },
            reward: RewardPoWConfig {
                minimum_difficulty: ModulusShift::new::<26>(),
                ema_smoothing_factor: 9,
                ema_smoothing_precision: 10.try_into().unwrap(),
                target_claims_per_block: 100.try_into().unwrap(),
                rate_num: 0,
                rate_den: NonZero::<u64>::MIN,
                pow_share: 0,
                share_den: NonZero::<u64>::MIN,
                slot_window: 100.try_into().unwrap(),
            },
        },
        accepted_fork_digests: AcceptedForkDigests::new(vec![fork_digest()]),
    }
}

/// The ledger of a chain with no notes.
fn ledger() -> LedgerState {
    LedgerState::from_notes([], &ledger_config(), &genesis_parameters())
}

fn fork_digest() -> ForkDigest {
    ForkDigest::new([0x11; 32])
}

fn tip() -> HeaderId {
    HeaderId::from([0; 32])
}

fn updater() -> StateUpdater<Option<RecoveryState>> {
    StateUpdater::new(Arc::new(watch::channel(None).0))
}

fn wallet_state(updater: &StateUpdater<Option<RecoveryState>>) -> ServiceState<'_> {
    let settings = WalletServiceSettings {
        known_keys: HashMap::new(),
        voucher_master_key_id: "voucher-master".into(),
        recovery_data: RecoveryData::default(),
        pending_note_expiry_blocks: 10,
        fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
        era_schedule: era_schedule(),
    };
    let recovery = RecoveryState::from_settings(&settings).unwrap();
    ServiceState::from_genesis(recovery, &settings, tip(), &[], &[], [], updater).unwrap()
}

/// A signer, and the KMS relay that nothing serves unless the test does.
fn signer() -> (Signer, InboundRelay<KMSMessage<KmsBackend>>) {
    let Relay {
        inbound_relay,
        outbound_relay,
        ..
    } = Relay::new(16);
    (
        Signer::new(KmsServiceApi::new(outbound_relay)),
        inbound_relay,
    )
}

fn closed_relay<Message>() -> OutboundRelay<Message> {
    let Relay { outbound_relay, .. } = Relay::new(1);
    outbound_relay
}

/// A chain that answers what the ledger is at any block with [`ledger`], and
/// nothing else.
fn chain() -> CryptarchiaServiceApi<Chain> {
    let Relay {
        mut inbound_relay,
        outbound_relay,
        ..
    } = Relay::new(16);
    tokio::spawn(async move {
        while let Some(msg) = inbound_relay.recv().await {
            if let ConsensusMsg::Query(Query::GetLedgerState { reply_channel, .. }) = msg {
                assert!(reply_channel.send(Some(ledger())).is_ok());
            }
        }
    });
    CryptarchiaServiceApi::new(outbound_relay)
}

/// Handles `msg`, which leaves any signing it starts in `signings`.
async fn handle(
    msg: WalletMsg,
    state: &mut ServiceState<'_>,
    chain: &CryptarchiaServiceApi<Chain>,
    signer: &Signer,
    signings: &FuturesUnordered<PendingSigning>,
) {
    let voucher_master_key_id = "voucher-master".to_owned();
    let storage = StorageApi::new(closed_relay());
    let era_schedule = era_schedule();
    let era_settings = era_schedule.get(Era::genesis()).unwrap().parameters;
    let handled = Box::pin(Service::handle_wallet_message(
        msg,
        state,
        &voucher_master_key_id,
        &storage,
        chain,
        signer,
        signings,
        &era_schedule,
        era_settings,
    ));
    timeout(REPLY_TIMEOUT, handled)
        .await
        .expect("The handler should not wait for a signature.");
}

/// An inscription, which its signer signs through the KMS.
fn inscription() -> Op {
    let signer = Ed25519Key::from_bytes(&[1; 32]).public_key();
    Op::ChannelInscribe(InscriptionOp {
        channel_id: ChannelId::from([1; 32]),
        inscription: Inscription::new_unchecked(Vec::new()),
        parent: MsgId::root(),
        signer: UnverifiedEd25519PublicKey::from_bytes(signer.as_bytes()).unwrap(),
        mini_blocks: MiniBlocks::default(),
    })
}

fn sign_message() -> (
    WalletMsg,
    oneshot::Receiver<Result<TipResponse<Tx>, WalletServiceError>>,
) {
    let (resp_tx, resp_rx) = oneshot::channel();
    let msg = WalletMsg::SignTx {
        tip: Some(tip()),
        tx_builder: MantleTxBuilder::new().push_op(inscription()).unwrap(),
        resp_tx,
    };
    (msg, resp_rx)
}

fn signing_job(tx_builder: MantleTxBuilder) -> TxSigningRequest {
    TxSigningRequest {
        ops: tx_builder.build(fork_digest()).unwrap(),
        channel_multi_sig_proofs: HashMap::new(),
        witnesses: Vec::new(),
        ledger: ledger(),
    }
}

/// A transfer with no witness to prove it with, which signing can only refuse.
fn unsignable_job() -> TxSigningRequest {
    signing_job(
        MantleTxBuilder::new()
            .push_op(Op::Transfer(TransferOp::sample()))
            .unwrap(),
    )
}

/// The note funding a signing's transaction.
fn funded_notes() -> Vec<NoteNullifier> {
    vec![NoteNullifier(Fr::ONE)]
}

#[tokio::test]
async fn a_signing_in_flight_leaves_the_wallet_free() {
    let updater = updater();
    let mut state = wallet_state(&updater);
    let chain = chain();
    let (signer, _unserved_kms) = signer();
    let signings = FuturesUnordered::new();

    let (msg, _signed) = sign_message();
    handle(msg, &mut state, &chain, &signer, &signings).await;

    let (resp_tx, known_addresses) = oneshot::channel();
    handle(
        WalletMsg::GetKnownAddresses { resp_tx },
        &mut state,
        &chain,
        &signer,
        &signings,
    )
    .await;
    assert!(known_addresses.await.unwrap().is_ok());
}

#[tokio::test]
async fn signings_past_the_limit_wait_for_a_permit() {
    let updater = updater();
    let mut state = wallet_state(&updater);
    let chain = chain();
    let (signer, mut kms) = signer();
    let mut signings = FuturesUnordered::new();

    for _ in 0..=MAX_CONCURRENT_SIGNINGS {
        let (msg, _signed) = sign_message();
        handle(msg, &mut state, &chain, &signer, &signings).await;
    }
    // The service loop's part.
    let _polling = tokio::spawn(async move { while signings.next().await.is_some() {} });

    let mut requests = Vec::new();
    for _ in 0..MAX_CONCURRENT_SIGNINGS {
        requests.push(timeout(REPLY_TIMEOUT, kms.recv()).await.unwrap());
    }
    sleep(SETTLE_TIME).await;
    assert!(timeout(SETTLE_TIME, kms.recv()).await.is_err());

    // Failing one frees its permit for the signing that was waiting.
    drop(requests.pop());
    assert!(timeout(REPLY_TIMEOUT, kms.recv()).await.unwrap().is_some());
}

#[tokio::test]
async fn a_failed_signing_gives_its_reservations_back() {
    let (signer, _unserved_kms) = signer();
    let (resp_tx, response) = oneshot::channel();

    let unused = Service::signing(unsignable_job(), tip(), funded_notes(), &signer, resp_tx).await;

    assert!(response.await.unwrap().is_err());
    assert_eq!(unused, funded_notes());
}

#[tokio::test]
async fn an_undelivered_transaction_gives_its_reservations_back() {
    let (signer, _unserved_kms) = signer();
    let (resp_tx, response) = oneshot::channel();
    drop(response);

    let unused = Service::signing(
        signing_job(MantleTxBuilder::new()),
        tip(),
        funded_notes(),
        &signer,
        resp_tx,
    )
    .await;

    assert_eq!(unused, funded_notes());
}

#[tokio::test]
async fn a_delivered_transaction_keeps_its_reservations() {
    let (signer, _unserved_kms) = signer();
    let (resp_tx, response) = oneshot::channel();

    let unused = Service::signing(
        signing_job(MantleTxBuilder::new()),
        tip(),
        funded_notes(),
        &signer,
        resp_tx,
    )
    .await;

    assert!(response.await.unwrap().is_ok());
    assert_eq!(unused, vec![]);
}

#[tokio::test]
async fn unavailable_events_leave_a_notified_block_for_backfill() {
    let known_note = KnownNote {
        note: Note::new(25, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64))),
        note_set: NoteSetIndex::Ledger,
        nullifier: NoteNullifier(Fr::from(3u64)),
    };
    let settings = wallet_settings(known_note.note.public_key);
    let genesis = HeaderId::from([0; 32]);
    let (sender, _receiver) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(sender));
    let mut state = wallet_state_with_note(&settings, genesis, known_note, &updater);

    let block = create_block(genesis, 1);
    let block_id = block.header().id();
    let child = create_block(block_id, 2);
    let child_id = child.header().id();
    let commitment = known_note.note.commitment();
    let events = Events::from(Event::Block(BlockEvent::NoteSetUpdated(NoteSetUpdate {
        note_set: NoteSetIndex::Ledger,
        nullifiers: vec![],
        commitments: vec![commitment],
    })));
    let storage = storage_api([block, child], events, None);
    let cryptarchia = cryptarchia_api(genesis, block_id, child_id);
    let kms = KmsServiceApi::new(closed_relay());

    Service::handle_new_block(
        block_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await;
    assert!(!state.wallet().has_processed_block(block_id));

    // The next notification backfills the missing parent, including its events.
    Service::handle_new_block(
        child_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await;

    assert!(state.wallet().has_processed_block(block_id));
    assert!(state.wallet().has_processed_block(child_id));
    let wallet_state = state.wallet().wallet_state_at(child_id).unwrap();
    assert_single_note_state(&wallet_state, known_note);
}

#[tokio::test]
async fn undecodable_events_stop_backfill_until_they_can_be_loaded() {
    let known_note = KnownNote {
        note: Note::new(25, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64))),
        note_set: NoteSetIndex::Ledger,
        nullifier: NoteNullifier(Fr::from(3u64)),
    };
    let settings = wallet_settings(known_note.note.public_key);
    let genesis = HeaderId::from([0; 32]);
    let (sender, _receiver) = watch::channel(None);
    let updater = StateUpdater::new(Arc::new(sender));
    let mut state = wallet_state_with_note(&settings, genesis, known_note, &updater);

    let block = create_block(genesis, 1);
    let block_id = block.header().id();
    let child = create_block(block_id, 2);
    let child_id = child.header().id();
    let commitment = known_note.note.commitment();
    let events = Events::from(Event::Block(BlockEvent::NoteSetUpdated(NoteSetUpdate {
        note_set: NoteSetIndex::Ledger,
        nullifiers: vec![],
        commitments: vec![commitment],
    })));
    let storage = storage_api(
        [block, child],
        events,
        Some(Bytes::from_static(b"invalid events")),
    );
    let cryptarchia = cryptarchia_api(genesis, block_id, child_id);
    let kms = KmsServiceApi::new(closed_relay());

    let result = Service::backfill_missing_blocks(
        child_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await;
    assert!(matches!(
        result,
        Err(WalletServiceError::BlockEventsUnavailable(id)) if id == block_id
    ));
    assert!(!state.wallet().has_processed_block(block_id));
    assert!(!state.wallet().has_processed_block(child_id));

    Service::backfill_missing_blocks(
        child_id,
        &mut state,
        &storage,
        &cryptarchia,
        &kms,
        &settings.era_schedule,
    )
    .await
    .unwrap();

    assert!(state.wallet().has_processed_block(block_id));
    assert!(state.wallet().has_processed_block(child_id));
    let wallet_state = state.wallet().wallet_state_at(child_id).unwrap();
    assert_single_note_state(&wallet_state, known_note);
}

fn wallet_settings(owner: ZkPublicKey) -> WalletServiceSettings {
    WalletServiceSettings {
        known_keys: HashMap::from([("owner".into(), owner)]),
        voucher_master_key_id: "voucher-master".into(),
        recovery_data: RecoveryData::default(),
        pending_note_expiry_blocks: 10,
        fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
        era_schedule: era_schedule(),
    }
}

fn wallet_state_with_note<'u>(
    settings: &WalletServiceSettings,
    genesis: HeaderId,
    known_note: KnownNote,
    updater: &'u StateUpdater<Option<RecoveryState>>,
) -> ServiceState<'u> {
    ServiceState::from_genesis(
        RecoveryState::from_settings(settings).unwrap(),
        settings,
        genesis,
        &[],
        &[],
        [known_note],
        updater,
    )
    .unwrap()
}

fn assert_single_note_state(state: &WalletState, known_note: KnownNote) {
    let commitment = known_note.note.commitment();
    assert_eq!(state.notes.size(), 1);
    let owned_note = state.notes.get(&commitment).unwrap();
    assert_eq!(owned_note.note, known_note.note);
    assert_eq!(owned_note.nullifier, known_note.nullifier);
    assert_eq!(
        state.balance(known_note.note.public_key).unwrap().balance,
        known_note.note.value,
    );
    let expected = CommitmentMmr::new().push(commitment).unwrap();
    assert_eq!(
        state
            .commitments
            .get(&NoteSetIndex::Ledger)
            .unwrap()
            .frontier_root(),
        expected.frontier_root(),
    );
}

fn create_block(parent: HeaderId, slot: u64) -> Block<Tx> {
    Block::create(
        parent,
        Slot::new(slot),
        UncleHeaders::empty(),
        Groth16LeaderProof::genesis(),
        BlockTransactions::empty(),
        u32::MAX,
    )
    .unwrap()
}

fn storage_api(
    blocks: [Block<Tx>; 2],
    block_events: Events,
    first_event_response: Option<Bytes>,
) -> StorageApi<Tx> {
    let [block, child] = blocks;
    let block_id = block.header().id();
    let child_id = child.header().id();
    let blocks = HashMap::from([
        (block_id, Bytes::from(block.encode_to_vec())),
        (child_id, Bytes::from(child.encode_to_vec())),
    ]);
    let events = HashMap::from([
        (block_id, Bytes::try_from(block_events).unwrap()),
        (child_id, Bytes::try_from(Events::new()).unwrap()),
    ]);
    let (msg_sender, mut msg_receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut fail_first_event_read = true;
        while let Some(msg) = msg_receiver.recv().await {
            match msg {
                StorageMsg::GetBlock {
                    header_id,
                    response_tx,
                } => response_tx.send(blocks.get(&header_id).cloned()).unwrap(),
                StorageMsg::GetBlockEvents {
                    header_id,
                    response_tx,
                } => {
                    let response = if header_id == block_id && fail_first_event_read {
                        fail_first_event_read = false;
                        first_event_response.clone()
                    } else {
                        events.get(&header_id).cloned()
                    };
                    response_tx.send(response).unwrap();
                }
                _ => panic!("Unexpected storage request"),
            }
        }
    });
    StorageApi::new(OutboundRelay::new(msg_sender))
}

fn cryptarchia_api(
    genesis: HeaderId,
    block: HeaderId,
    child: HeaderId,
) -> CryptarchiaServiceApi<Chain> {
    let (msg_sender, mut msg_receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        while let Some(msg) = msg_receiver.recv().await {
            let ConsensusMsg::Query(Query::GetHeaders {
                from_descendant,
                to_ancestor,
                reply_channel,
            }) = msg
            else {
                panic!("Unexpected chain query");
            };
            assert_eq!(from_descendant, Some(child));
            assert_eq!(to_ancestor, Some(genesis));
            let headers = futures::stream::iter([Ok(child), Ok(block), Ok(genesis)]);
            assert!(reply_channel.send(Box::pin(headers)).is_ok());
        }
    });
    CryptarchiaServiceApi::new(OutboundRelay::new(msg_sender))
}
