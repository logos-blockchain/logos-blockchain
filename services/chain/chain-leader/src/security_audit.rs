use std::{
    collections::HashSet,
    fmt::{Debug, Display},
    time::{Duration, Instant},
};

#[cfg(not(feature = "testing-disable-proposal-publish"))]
use lb_core::block::{Proposal, References};
use lb_core::{
    block::{Block, Error as BlockError},
    header::HeaderId,
    mantle::{
        ops::leader_claim::VoucherCm,
        traits::{Hashable, StorageSize},
        transactions::hash::TxHash,
    },
    proofs::leader_proof::LeaderProof as _,
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_log_targets::diagnostic::BLEND_REACHABILITY;
use lb_wallet_service::api::{WalletApi, WalletApiError, WalletServiceData};
use overwatch::services::AsServiceId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{LOG_TARGET, blend::BlendAdapter};

/// Runtime controls for security-audit-only leader behavior.
///
/// Audit behavior is disabled when this setting is omitted or set to zero.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityAuditSettings {
    /// Additional valid same-parent, same-slot blocks to produce after a
    /// genuine leadership win.
    #[serde(default)]
    pub sibling_blocks_per_leadership: usize,
}

impl SecurityAuditSettings {
    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        self.sibling_blocks_per_leadership == 0
    }
}

pub struct LeadershipTiming {
    slot_started: Instant,
    proposal_started: Instant,
    ordinary_finished: Instant,
    parent: HeaderId,
    slot: Slot,
}

impl LeadershipTiming {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            slot_started: now,
            proposal_started: now,
            ordinary_finished: now,
            parent: HeaderId::from([0; 32]),
            slot: Slot::genesis(),
        }
    }

    pub fn proposal_started(mut self, parent: HeaderId, slot: Slot) -> Self {
        self.proposal_started = Instant::now();
        self.parent = parent;
        self.slot = slot;
        self
    }

    fn submission_finished(mut self) -> Self {
        self.ordinary_finished = Instant::now();
        self
    }

    pub fn proposal_failed(self, settings: SecurityAuditSettings) {
        self.submission_finished()
            .report(settings, None, &SiblingStats::default());
    }

    fn report(
        &self,
        settings: SecurityAuditSettings,
        ordinary_submission: Option<bool>,
        stats: &SiblingStats,
    ) {
        tracing::info!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "security_audit_leadership_timing",
            parent_block_id = %self.parent,
            slot = u64::from(self.slot),
            configured_siblings = settings.sibling_blocks_per_leadership,
            constructed_siblings = stats.constructed,
            failed_siblings = stats.failed,
            relay_enqueued_siblings = stats.relay_enqueued,
            skipped_siblings = if ordinary_submission == Some(true) { 0 } else { settings.sibling_blocks_per_leadership },
            ordinary_self_applied = ordinary_submission.is_some(),
            ordinary_relay_enqueued = ordinary_submission == Some(true),
            ordinary_proposal_ms = self.ordinary_finished.duration_since(self.proposal_started).as_secs_f64() * 1000.0,
            slot_to_ordinary_submission_ms = self.ordinary_finished.duration_since(self.slot_started).as_secs_f64() * 1000.0,
            sibling_construction_ms = stats.construction.as_secs_f64() * 1000.0,
            sibling_blend_enqueue_ms = stats.blend_enqueue.as_secs_f64() * 1000.0,
            additional_processing_ms = stats.additional_processing.as_secs_f64() * 1000.0,
            total_leadership_processing_ms = self.slot_started.elapsed().as_secs_f64() * 1000.0,
            "Leadership processing completed; relay enqueue does not imply peer reception"
        );
    }
}

#[derive(Debug, Default)]
pub struct SiblingStats {
    constructed: usize,
    failed: usize,
    relay_enqueued: usize,
    construction: Duration,
    blend_enqueue: Duration,
    additional_processing: Duration,
}

#[derive(Debug, Error)]
enum SiblingError {
    #[error("Wallet API error: {0}")]
    Wallet(Box<WalletApiError>),
    #[error("Wallet returned a duplicate voucher commitment")]
    DuplicateVoucher,
    #[error("Ordinary block transaction body size exceeds the supported limit")]
    TransactionSizeOverflow,
    #[error("Sibling construction failed: {0}")]
    Block(#[from] BlockError),
}

/// Preserve the completed block only when siblings need it. These copies belong
/// to ordinary proposal encoding; no sibling is constructed before submission.
#[cfg(not(feature = "testing-disable-proposal-publish"))]
pub fn ordinary_proposal<Tx>(
    block: Block<Tx>,
    settings: SecurityAuditSettings,
) -> (Proposal, Option<Block<Tx>>)
where
    Tx: Hashable<Hash = TxHash>,
{
    if settings.is_disabled() {
        return (block.to_proposal(), None);
    }
    let proposal = Proposal {
        header: block.header().clone(),
        uncle_headers: block.uncle_headers().clone(),
        references: References {
            mempool_transactions: block.transactions().map_ref(|tx| tx.hash().prefix()),
        },
        signature: *block.signature(),
    };
    (proposal, Some(block))
}

/// Called only after ordinary local application and Blend submission complete.
/// `None` means application failed; `Some(false)` means no relay enqueue.
/// Failures here never change the ordinary proposal or the local chain.
#[expect(
    clippy::cognitive_complexity,
    reason = "Sibling diagnostics and sequential publication preserve audit ordering"
)]
pub async fn produce_siblings<Tx, Wallet, BlendService, RuntimeServiceId>(
    ordinary: Option<&Block<Tx>>,
    signing_key: &Ed25519Key,
    wallet: &WalletApi<Wallet, RuntimeServiceId>,
    blend: &BlendAdapter<BlendService>,
    settings: SecurityAuditSettings,
    timing: LeadershipTiming,
    ordinary_submission: Option<bool>,
) -> SiblingStats
where
    Tx: Clone + Hashable<Hash = TxHash> + StorageSize + Send + Sync,
    Wallet: WalletServiceData,
    RuntimeServiceId: Debug + Display + Sync + AsServiceId<Wallet>,
    BlendService: lb_blend_service::api::BlendServiceData<NodeId: Send>,
{
    let timing = timing.submission_finished();
    let mut stats = SiblingStats::default();
    if let Some(ordinary) = ordinary
        && ordinary_submission == Some(true)
        && !settings.is_disabled()
    {
        tracing::info!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "security_audit_ordinary_submitted",
            ordinary_block_id = %ordinary.header().id(),
            parent_block_id = %ordinary.header().parent(),
            slot = u64::from(ordinary.header().slot()),
            ordinary_self_applied = true,
            ordinary_relay_enqueued = true,
            configured_siblings = settings.sibling_blocks_per_leadership,
            "Ordinary proposal completed local application and Blend enqueue before sibling construction"
        );
        let additional_started = Instant::now();
        let mut vouchers = HashSet::from([*ordinary.header().leader_proof().voucher_cm()]);
        for sibling_index in 1..=settings.sibling_blocks_per_leadership {
            let construction_started = Instant::now();
            let sibling = construct_sibling(ordinary, signing_key, wallet, &mut vouchers).await;
            stats.construction += construction_started.elapsed();
            match sibling {
                Ok(sibling) => {
                    stats.constructed += 1;
                    let sibling_block_id = sibling.header().id();
                    let parent_block_id = sibling.header().parent();
                    let slot = u64::from(sibling.header().slot());
                    let enqueue_started = Instant::now();
                    let relay_enqueued = blend.publish_proposal(sibling.to_proposal()).await;
                    stats.blend_enqueue += enqueue_started.elapsed();
                    if relay_enqueued {
                        stats.relay_enqueued += 1;
                    } else {
                        stats.failed += 1;
                    }
                    tracing::info!(
                        target: LOG_TARGET,
                        diagnostic = BLEND_REACHABILITY,
                        event = "security_audit_sibling_submitted",
                        ordinary_block_id = %ordinary.header().id(),
                        %sibling_block_id,
                        %parent_block_id,
                        slot,
                        sibling_index,
                        relay_enqueued,
                        "Sibling proposal enqueue attempted at local Blend relay"
                    );
                }
                Err(error) => {
                    stats.failed += 1;
                    tracing::error!(
                        target: LOG_TARGET,
                        diagnostic = BLEND_REACHABILITY,
                        event = "security_audit_sibling_construction_failed",
                        ordinary_block_id = %ordinary.header().id(),
                        sibling_index,
                        %error,
                        "Failed to construct additional sibling"
                    );
                }
            }
        }
        stats.additional_processing = additional_started.elapsed();
    }
    timing.report(settings, ordinary_submission, &stats);
    stats
}

async fn construct_sibling<Tx, Wallet, RuntimeServiceId>(
    ordinary: &Block<Tx>,
    signing_key: &Ed25519Key,
    wallet: &WalletApi<Wallet, RuntimeServiceId>,
    vouchers: &mut HashSet<VoucherCm>,
) -> Result<Block<Tx>, SiblingError>
where
    Tx: Clone + Hashable<Hash = TxHash> + StorageSize + Send + Sync,
    Wallet: WalletServiceData,
    RuntimeServiceId: Debug + Display + Sync + AsServiceId<Wallet>,
{
    let voucher = wallet
        .generate_new_voucher()
        .await
        .map_err(|error| SiblingError::Wallet(Box::new(error)))?;
    if !vouchers.insert(voucher) {
        return Err(SiblingError::DuplicateVoucher);
    }
    // The ordinary block has already passed validation under this era's limit;
    // its exact transaction-body size is therefore a sufficient bound for the
    // sibling, whose body is identical.
    let max_transactions_size =
        ordinary
            .transactions()
            .iter()
            .try_fold(0u32, |total, transaction| {
                let size = u32::try_from(transaction.storage_size())
                    .map_err(|_| SiblingError::TransactionSizeOverflow)?;
                total
                    .checked_add(size)
                    .ok_or(SiblingError::TransactionSizeOverflow)
            })?;
    let header = ordinary.header();
    Ok(Block::create(
        header.parent(),
        header.slot(),
        ordinary.uncle_headers().clone(),
        header.leader_proof().clone().with_audit_voucher_cm(voucher),
        ordinary.transactions().clone(),
        max_transactions_size,
        signing_key,
    )?)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use lb_binary_codec::canonical::BinaryDecode as _;
    use lb_blend_service::{
        api::ApiError,
        message::{DataPayload, ProxyServiceMessage, ServiceMessage},
    };
    use lb_core::{
        block::{BlockLimits, BlockTransactions, Proposal as BlockProposal, UncleHeaders},
        era::ForkDigest,
        mantle::{
            Note, Op, OpProof, SignedOps, Utxo,
            ledger::{Inputs, Outputs, verification_mode::StandardMode},
            ops::transfer::TransferOp,
            transactions::{OpProofs, Ops, states::Unverified},
        },
        proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic, check_winning},
    };
    use lb_groth16::Fr;
    use lb_key_management_system_service::{backend::preload::KeyId, keys::ZkKey};
    use lb_ledger::{ConsensusExcludedKeys, EpochState, Ledger, LedgerState, UtxoTree};
    use lb_wallet_service::{UtxoWithKeyId, WalletMsg, WalletServiceSettings};
    use overwatch::{
        DynError,
        services::{
            ServiceData,
            relay::OutboundRelay,
            state::{NoOperator, NoState},
        },
    };
    use rand::rngs::OsRng;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        kms::KmsAdapter,
        leadership::{PrivateInputsError, build_proof_for, single_era, test_config},
    };

    type TestTx = SignedOps<lb_core::mantle::transactions::states::Preverified, StandardMode>;
    type BlendMessage = ProxyServiceMessage<ServiceMessage<()>>;

    #[derive(Debug)]
    struct TestRuntime;

    impl Display for TestRuntime {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("TestRuntime")
        }
    }

    impl AsServiceId<TestWallet> for TestRuntime {
        const SERVICE_ID: Self = Self;
    }

    struct TestWallet;

    impl ServiceData for TestWallet {
        type Settings = WalletServiceSettings;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = WalletMsg;
    }

    impl WalletServiceData for TestWallet {
        type Kms = ();
        type Cryptarchia = ();
        type Tx = ();
    }

    struct TestBlend;

    impl ServiceData for TestBlend {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = BlendMessage;
    }

    impl lb_blend_service::ServiceComponents for TestBlend {
        type NodeId = ();
    }

    #[derive(Default)]
    struct TestKms {
        witnesses: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl KmsAdapter<TestRuntime> for TestKms {
        type KeyId = KeyId;

        async fn check_winning_with_key(
            &self,
            _: KeyId,
            utxo: &Utxo,
            public: &LeaderPublic,
        ) -> Result<bool, DynError> {
            Ok(check_winning(
                *utxo,
                *public,
                &ZkKey::zero().to_public_key(),
                Fr::from(0u64),
            ))
        }

        async fn build_private_inputs_for_winning_utxo_and_slot(
            &self,
            _: KeyId,
            utxo: &Utxo,
            epoch: &EpochState,
            public: LeaderPublic,
            latest: &UtxoTree,
        ) -> Result<(LeaderPrivate, Ed25519Key), PrivateInputsError> {
            self.witnesses.fetch_add(1, Ordering::SeqCst);
            let signing_key = Ed25519Key::generate(&mut OsRng);
            let witness = LeaderPrivate::new(
                public,
                *utxo,
                &epoch.utxo_merkle_path(utxo).unwrap(),
                &latest.path(&utxo.id()).unwrap(),
                Fr::from(0u64),
                &signing_key.public_key(),
            );
            Ok((witness, signing_key))
        }
    }

    struct Fixture {
        ordinary: Block<TestTx>,
        signing_key: Ed25519Key,
        ledger: Ledger<HeaderId>,
        wallet: WalletApi<TestWallet, TestRuntime>,
        vouchers: Arc<AtomicUsize>,
        ordinary_ready: Arc<AtomicBool>,
        kms: TestKms,
        max_transactions_size: u32,
    }

    impl Fixture {
        #[expect(
            clippy::too_many_lines,
            reason = "The fixture sets up a valid leader, transaction, ledger, and wallet."
        )]
        async fn new(voucher_values: &[u64]) -> Self {
            let vouchers = Arc::new(AtomicUsize::new(0));
            let ordinary_ready = Arc::new(AtomicBool::new(false));
            let mut voucher_values: VecDeque<_> = voucher_values
                .iter()
                .map(|value| VoucherCm::from(Fr::from(*value)))
                .collect();
            let (sender, mut receiver) = mpsc::channel(10);
            let calls = Arc::clone(&vouchers);
            let ready = Arc::clone(&ordinary_ready);
            tokio::spawn(async move {
                while let Some(WalletMsg::GenerateNewVoucherSecret { resp_tx }) =
                    receiver.recv().await
                {
                    let index = calls.fetch_add(1, Ordering::SeqCst);
                    if index > 0 {
                        assert!(
                            ready.load(Ordering::SeqCst),
                            "ordinary block must be applied and enqueued before sibling voucher generation"
                        );
                    }
                    let voucher = voucher_values
                        .pop_front()
                        .expect("unexpected voucher request");
                    drop(resp_tx.send(Ok(voucher)));
                }
            });
            let wallet = WalletApi::new(OutboundRelay::new(sender));
            let config = test_config();
            let max_transactions_size = config.max_transactions_size;
            let key = ZkKey::zero();
            let leader_transfer = TransferOp::new(
                Inputs::empty(),
                Outputs::new([Note::new(1000, key.to_public_key())]),
            );
            let leader_utxo = leader_transfer
                .outputs
                .utxo_by_index(0, &leader_transfer)
                .unwrap();
            // Reuse the signed transfer fixture pattern from tx_selection tests.
            let funding_utxo = Utxo::new([17; 32], 0, Note::new(1000, key.to_public_key()));
            let ops = Ops::new(
                ForkDigest::new([0x11; 32]),
                [Op::Transfer(TransferOp::new(
                    Inputs::try_new(vec![funding_utxo.id()]).unwrap(),
                    Outputs::new([]),
                ))],
            );
            let signature =
                ZkKey::multi_sign(std::slice::from_ref(&key), &ops.hash().to_fr()).unwrap();
            let tx = SignedOps::<Unverified, StandardMode>::from_parts(
                ops,
                OpProofs::try_from_iter([OpProof::ZkSig(signature)]).unwrap(),
            )
            .unwrap()
            .preverify()
            .unwrap();
            let parent_state = LedgerState::from_utxos(
                [leader_utxo, funding_utxo],
                &config,
                0,
                &ConsensusExcludedKeys::empty(),
            );
            let eligible = [UtxoWithKeyId {
                utxo: leader_utxo,
                key_id: KeyId::from("0"),
            }];
            let kms = TestKms::default();
            for slot in (1..10_000).map(Slot::from) {
                if let Some((proof, signing_key)) = build_proof_for(
                    &eligible,
                    parent_state.latest_utxos(),
                    parent_state.epoch_state(),
                    slot,
                    &wallet,
                    &kms,
                )
                .await
                .unwrap()
                {
                    let parent = HeaderId::from([0; 32]);
                    let ordinary = Block::create(
                        parent,
                        slot,
                        UncleHeaders::empty(),
                        proof,
                        BlockTransactions::try_from_iter([tx], config.max_transactions).unwrap(),
                        config.max_transactions_size,
                        &signing_key,
                    )
                    .unwrap();
                    assert_eq!(kms.witnesses.load(Ordering::SeqCst), 1);
                    assert_eq!(vouchers.load(Ordering::SeqCst), 1);
                    return Self {
                        ordinary,
                        signing_key,
                        ledger: Ledger::new(parent, parent_state, Arc::new(single_era(config))),
                        wallet,
                        vouchers,
                        ordinary_ready,
                        kms,
                        max_transactions_size,
                    };
                }
            }
            panic!("fixture should win a legitimate slot");
        }

        fn validate(&mut self, candidate: &Block<TestTx>, commit: bool) {
            let header = candidate.header();
            let parent_state = self.ledger.state(&self.ordinary.header().parent()).unwrap();
            let epoch = parent_state.epoch_state();
            let public = LeaderPublic::new(
                epoch.utxos.root(),
                parent_state.latest_utxos().root(),
                epoch.nonce,
                header.slot().into(),
                epoch.lottery_0,
                epoch.lottery_1,
            );
            assert!(header.leader_proof().verify(&public));
            let update = self
                .ledger
                .prepare_update::<_, Groth16LeaderProof>(
                    header.id(),
                    header.parent(),
                    header.slot(),
                    header.leader_proof(),
                    &candidate.uncle_headers().slots(),
                    candidate.transactions_iter().cloned(),
                )
                .unwrap()
                .verify_batch_proofs()
                .unwrap();
            if commit {
                self.ledger.commit_update(update);
            }
        }

        async fn apply_and_enqueue_ordinary(&mut self, blend: &BlendAdapter<TestBlend>) {
            self.validate(&self.ordinary.clone(), true);
            assert!(
                blend
                    .publish_proposal(self.ordinary.clone().to_proposal())
                    .await
            );
            self.ordinary_ready.store(true, Ordering::SeqCst);
        }
    }

    fn blend_channel() -> (BlendAdapter<TestBlend>, mpsc::Receiver<BlendMessage>) {
        let (relay_sender, mut relay_receiver) = mpsc::channel(10);
        let (observed_sender, observed_receiver) = mpsc::channel(10);
        tokio::spawn(async move {
            while let Some(ProxyServiceMessage::Blend { payload, reply }) =
                relay_receiver.recv().await
            {
                let observed = observed_sender
                    .send(ProxyServiceMessage::Inner(ServiceMessage::Blend(payload)))
                    .await;
                let result = observed.map_err(|_| {
                    ApiError::CommsFailure("test Blend observer was dropped".to_owned())
                });
                drop(reply.send(result));
            }
        });
        (
            BlendAdapter::new(OutboundRelay::new(relay_sender)),
            observed_receiver,
        )
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "Keep the candidate validity and ordering assertions together."
    )]
    async fn check_siblings(count: usize) {
        let values: Vec<_> = (1..=u64::try_from(count).unwrap() + 1).collect();
        let mut fixture = Fixture::new(&values).await;
        let timing = LeadershipTiming::start().proposal_started(
            fixture.ordinary.header().parent(),
            fixture.ordinary.header().slot(),
        );
        let (blend, mut receiver) = blend_channel();
        fixture.apply_and_enqueue_ordinary(&blend).await;
        let stats = produce_siblings(
            Some(&fixture.ordinary),
            &fixture.signing_key,
            &fixture.wallet,
            &blend,
            SecurityAuditSettings {
                sibling_blocks_per_leadership: count,
            },
            timing,
            Some(true),
        )
        .await;
        assert_eq!(stats.constructed, count);
        assert_eq!(stats.relay_enqueued, count);
        assert_eq!(stats.failed, 0);
        assert_eq!(fixture.vouchers.load(Ordering::SeqCst), count + 1);
        assert_eq!(fixture.kms.witnesses.load(Ordering::SeqCst), 1);
        println!("additional siblings = {count}, costs = {stats:?}");
        if count == 0 {
            assert_eq!(stats.construction, Duration::ZERO);
            assert_eq!(stats.blend_enqueue, Duration::ZERO);
            assert_eq!(stats.additional_processing, Duration::ZERO);
        }

        let mut vouchers = HashSet::new();
        let mut ids = HashSet::new();
        let mut signatures = HashSet::new();
        let ordinary_id = fixture.ordinary.header().id();
        for index in 0..=count {
            let ProxyServiceMessage::Inner(ServiceMessage::Blend(DataPayload::BlockProposal(
                bytes,
            ))) = receiver.try_recv().unwrap()
            else {
                panic!("expected a proposal")
            };
            let (rest, proposal) = BlockProposal::decode(
                &bytes,
                &BlockLimits {
                    max_uncles: u8::MAX,
                    max_transactions: u16::MAX,
                    max_transactions_size: fixture.max_transactions_size,
                },
            )
            .unwrap();
            assert_eq!(rest, b"");
            let block = Block::reconstruct(
                proposal.header,
                proposal.uncle_headers,
                fixture.ordinary.transactions().clone(),
                proposal.signature,
                fixture.max_transactions_size,
            )
            .unwrap();
            let header = block.header();
            let original = fixture.ordinary.header();
            assert_eq!(header.parent(), original.parent());
            assert_eq!(header.slot(), original.slot());
            assert_eq!(block.uncle_headers(), fixture.ordinary.uncle_headers());
            assert_eq!(block.transactions(), fixture.ordinary.transactions());
            assert!(!block.transactions().is_empty());
            assert_eq!(header.body_root(), original.body_root());
            assert_eq!(
                header.leader_proof().proof().to_bytes(),
                original.leader_proof().proof().to_bytes()
            );
            assert_eq!(
                header.leader_proof().entropy(),
                original.leader_proof().entropy()
            );
            assert_eq!(
                header.leader_proof().leader_key(),
                original.leader_proof().leader_key()
            );
            assert!(vouchers.insert(*header.leader_proof().voucher_cm()));
            assert!(ids.insert(header.id()));
            assert!(signatures.insert(block.signature().to_bytes()));
            fixture.validate(&block, false);
            if index == 0 {
                assert_eq!(header.id(), ordinary_id, "ordinary proposal must be first");
            } else {
                assert!(
                    fixture.ledger.state(&header.id()).is_none(),
                    "siblings are never self-applied"
                );
            }
        }
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn zero_siblings_preserves_ordinary_candidate() {
        check_siblings(0).await;
    }

    #[tokio::test]
    async fn one_sibling_reuses_valid_proof_and_body() {
        check_siblings(1).await;
    }

    #[tokio::test]
    async fn three_siblings_reuse_valid_proof_and_body() {
        check_siblings(3).await;
    }

    #[cfg(not(feature = "testing-disable-proposal-publish"))]
    #[tokio::test]
    async fn ordinary_submission_retains_template_only_when_needed() {
        let fixture = Fixture::new(&[1]).await;
        for count in [0, 3] {
            let (proposal, template) = ordinary_proposal(
                fixture.ordinary.clone(),
                SecurityAuditSettings {
                    sibling_blocks_per_leadership: count,
                },
            );
            assert_eq!(proposal, fixture.ordinary.clone().to_proposal());
            assert_eq!(template.is_some(), count > 0);
        }
        assert_eq!(fixture.vouchers.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unsuccessful_ordinary_proposal_skips_sibling_work() {
        let fixture = Fixture::new(&[1]).await;
        let (blend, mut receiver) = blend_channel();
        for ordinary_submission in [None, Some(false)] {
            let stats = produce_siblings(
                Some(&fixture.ordinary),
                &fixture.signing_key,
                &fixture.wallet,
                &blend,
                SecurityAuditSettings {
                    sibling_blocks_per_leadership: 3,
                },
                LeadershipTiming::start().proposal_started(
                    fixture.ordinary.header().parent(),
                    fixture.ordinary.header().slot(),
                ),
                ordinary_submission,
            )
            .await;
            assert_eq!(stats.constructed, 0);
            assert_eq!(stats.relay_enqueued, 0);
        }
        assert_eq!(fixture.vouchers.load(Ordering::SeqCst), 1);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn duplicate_voucher_does_not_undo_ordinary_or_prevent_next_sibling() {
        let mut fixture = Fixture::new(&[1, 1, 2]).await;
        let ordinary_id = fixture.ordinary.header().id();
        let (blend, receiver) = blend_channel();
        fixture.apply_and_enqueue_ordinary(&blend).await;
        let stats = produce_siblings(
            Some(&fixture.ordinary),
            &fixture.signing_key,
            &fixture.wallet,
            &blend,
            SecurityAuditSettings {
                sibling_blocks_per_leadership: 2,
            },
            LeadershipTiming::start().proposal_started(
                fixture.ordinary.header().parent(),
                fixture.ordinary.header().slot(),
            ),
            Some(true),
        )
        .await;
        assert_eq!(stats.constructed, 1);
        assert_eq!(stats.relay_enqueued, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(receiver.len(), 2);
        assert!(fixture.ledger.state(&ordinary_id).is_some());
    }

    #[tokio::test]
    async fn closed_blend_relay_counts_failure_without_undoing_ordinary() {
        let mut fixture = Fixture::new(&[1, 2]).await;
        let (blend, receiver) = blend_channel();
        fixture.apply_and_enqueue_ordinary(&blend).await;
        drop(receiver);
        let stats = produce_siblings(
            Some(&fixture.ordinary),
            &fixture.signing_key,
            &fixture.wallet,
            &blend,
            SecurityAuditSettings {
                sibling_blocks_per_leadership: 1,
            },
            LeadershipTiming::start().proposal_started(
                fixture.ordinary.header().parent(),
                fixture.ordinary.header().slot(),
            ),
            Some(true),
        )
        .await;
        assert_eq!(stats.constructed, 1);
        assert_eq!(stats.relay_enqueued, 0);
        assert_eq!(stats.failed, 1);
        assert!(
            fixture
                .ledger
                .state(&fixture.ordinary.header().id())
                .is_some()
        );
    }
}
