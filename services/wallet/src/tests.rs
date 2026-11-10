use lb_chain_service::ConsensusMsg;
use lb_groth16::{Field as _, Fr};
use lb_key_management_system_service::{KMSService, message::KMSMessage};
use lb_ledger::ConsensusExcludedKeys;
use lb_time::era::Era;
use overwatch::services::{
    relay::{InboundRelay, OutboundRelay, Relay},
    state::{NoOperator, NoState, ServiceState as _, StateUpdater},
};
use tokio::{
    sync::watch,
    time::{sleep, timeout},
};

use super::*;
use crate::states::tests::{era_schedule, ledger_config};

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

fn ledger() -> LedgerState {
    LedgerState::from_utxos([], &ledger_config(), 0, &ConsensusExcludedKeys::empty())
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
        era_schedule: era_schedule(),
    };
    let recovery = RecoveryState::from_settings(&settings).unwrap();
    ServiceState::new(recovery, &settings, tip(), &ledger(), updater)
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

/// Handles `msg`, which leaves any signing it starts in `signings`. With no
/// chain to answer, the wallet stays at its LIB and carries on with the
/// message.
async fn handle(
    msg: WalletMsg,
    state: &mut ServiceState<'_>,
    signer: &Signer,
    signings: &FuturesUnordered<PendingSigning>,
) {
    let voucher_master_key_id = "voucher-master".to_owned();
    let storage = StorageApi::new(closed_relay());
    let chain = CryptarchiaServiceApi::new(closed_relay());
    let era_schedule = era_schedule();
    let era_settings = era_schedule.get(Era::genesis()).unwrap().parameters;
    let handled = Box::pin(Service::handle_wallet_message(
        msg,
        state,
        &voucher_master_key_id,
        &storage,
        &chain,
        signer,
        signings,
        &era_schedule,
        era_settings,
    ));
    timeout(REPLY_TIMEOUT, handled)
        .await
        .expect("The handler should not wait for a signature.");
}

fn sign_with_zk_message() -> (
    WalletMsg,
    oneshot::Receiver<Result<ZkSignature, WalletServiceError>>,
) {
    let (resp_tx, resp_rx) = oneshot::channel();
    let msg = WalletMsg::SignTxWithZk {
        tx_hash: TxHash::default(),
        pks: ZkPublicKeys::try_from_iter([ZkPublicKey::new(Fr::ONE)]).unwrap(),
        resp_tx,
    };
    (msg, resp_rx)
}

fn signing_job(tx_builder: MantleTxBuilder) -> TxSigningRequest {
    TxSigningRequest {
        mantle_tx: tx_builder.build(fork_digest()).unwrap(),
        channel_multi_sig_proofs: HashMap::new(),
        leader_claims: HashMap::new(),
        ledger: ledger(),
    }
}

/// A leader claim with no witness, which signing can only refuse.
fn unsignable_job() -> TxSigningRequest {
    let claim = Op::LeaderClaim(LeaderClaimOp {
        rewards_root: RewardsRoot::default(),
        voucher_nullifier: VoucherNullifier::default(),
        pk: ZkPublicKey::new(Fr::ONE),
    });
    signing_job(MantleTxBuilder::new().push_op(claim).unwrap())
}

/// What a signing holds for the voucher and the note reserved for it.
fn reservations() -> HeldReservations {
    let updater = updater();
    let mut state = wallet_state(&updater);
    let (claim, note) = (VoucherNullifier::default(), NoteId::from(Fr::ONE));
    state.reserve_claim(claim, NonZero::new(1).unwrap());
    state.reserve_pending_notes([note]);
    state.held_reservations(Some(claim), [note])
}

#[tokio::test]
async fn a_zk_signature_in_flight_leaves_the_wallet_free() {
    let updater = updater();
    let mut state = wallet_state(&updater);
    let (signer, _unserved_kms) = signer();
    let signings = FuturesUnordered::new();

    let (msg, _signature) = sign_with_zk_message();
    handle(msg, &mut state, &signer, &signings).await;

    let (resp_tx, known_addresses) = oneshot::channel();
    handle(
        WalletMsg::GetKnownAddresses { resp_tx },
        &mut state,
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
    let (signer, mut kms) = signer();
    let mut signings = FuturesUnordered::new();

    for _ in 0..=MAX_CONCURRENT_SIGNINGS {
        let (msg, _signature) = sign_with_zk_message();
        handle(msg, &mut state, &signer, &signings).await;
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

    let unused = Service::signing(unsignable_job(), tip(), reservations(), &signer, resp_tx).await;

    assert!(response.await.unwrap().is_err());
    assert!(unused.is_some());
}

#[tokio::test]
async fn an_undelivered_transaction_gives_its_reservations_back() {
    let (signer, _unserved_kms) = signer();
    let (resp_tx, response) = oneshot::channel();
    drop(response);

    let unused = Service::signing(
        signing_job(MantleTxBuilder::new()),
        tip(),
        reservations(),
        &signer,
        resp_tx,
    )
    .await;

    assert!(unused.is_some());
}

#[tokio::test]
async fn a_delivered_transaction_keeps_its_reservations() {
    let (signer, _unserved_kms) = signer();
    let (resp_tx, response) = oneshot::channel();

    let unused = Service::signing(
        signing_job(MantleTxBuilder::new()),
        tip(),
        reservations(),
        &signer,
        resp_tx,
    )
    .await;

    assert!(response.await.unwrap().is_ok());
    assert!(unused.is_none());
}
