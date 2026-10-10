use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::Entry};

use lb_common_http_client::ApiBlock;
use lb_core::{
    events::{Event, Events, HeaderEvent},
    mantle::{
        NoteId, SignedOps, TxHash, Utxo, ledger::verification_mode::StandardMode, ops::OpRef,
        traits::Hashable as _, transactions::states::Unverified,
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::common::wallet::TrackedWallets;
use crate::common::wallet::{TrackedWalletKeys, WalletId, WalletUtxos};

#[derive(Clone)]
/// Wallet UTXO accounting derived by replaying scanned chain blocks.
pub struct ScannerAccounting {
    tracked_wallets: Vec<TrackedWalletKeys>,
    public_key_to_wallet: HashMap<ZkPublicKey, WalletId>,
    wallet_utxos: BTreeMap<WalletId, BTreeMap<NoteId, Utxo>>,
    service_note_ids: HashSet<NoteId>,
    observed_transaction_hashes: BTreeSet<TxHash>,
}

/// Scanner seed state needed to resume wallet accounting without losing
/// service-locked UTXOs across a persisted snapshot.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScannerAccountingSnapshot {
    /// All unspent tracked-wallet UTXOs, including service-locked ones.
    pub wallet_utxos: WalletUtxos,
    /// Notes held by at least one live service declaration at this checkpoint.
    #[serde(default)]
    pub locked_service_note_ids: HashSet<NoteId>,
}

impl ScannerAccountingSnapshot {
    /// Keep only UTXOs belonging to the requested wallets.
    #[must_use]
    pub fn filtered_for_wallets(&self, wallet_ids: &HashSet<WalletId>) -> Self {
        let wallet_utxos = self
            .wallet_utxos
            .iter()
            .filter(|(wallet_id, _)| wallet_ids.contains(*wallet_id))
            .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos.clone()))
            .collect::<WalletUtxos>();
        let known_note_ids = wallet_utxos
            .values()
            .flatten()
            .map(Utxo::id)
            .collect::<HashSet<_>>();

        Self {
            wallet_utxos,
            locked_service_note_ids: self
                .locked_service_note_ids
                .intersection(&known_note_ids)
                .copied()
                .collect(),
        }
    }

    /// Combine the disjoint wallet slices stored by nodes in one scanner
    /// group.
    pub fn extend(&mut self, other: Self) {
        for (wallet_id, utxos) in other.wallet_utxos {
            self.wallet_utxos
                .entry(wallet_id)
                .or_default()
                .extend(utxos);
        }
        self.locked_service_note_ids
            .extend(other.locked_service_note_ids);
    }
}

impl ScannerAccounting {
    /// Build accounting for tracked wallets seeded from genesis UTXOs.
    pub fn new(
        tracked_wallets: Vec<TrackedWalletKeys>,
        genesis_utxos: &[Utxo],
    ) -> Result<Self, crate::common::wallet::TrackedWalletKeysError> {
        let mut accounting = Self::empty(tracked_wallets)?;

        for utxo in genesis_utxos {
            accounting.add_owned_output(*utxo);
        }

        Ok(accounting)
    }

    /// Build accounting for tracked wallets seeded from restored wallet UTXOs.
    pub fn from_wallet_utxos(
        tracked_wallets: Vec<TrackedWalletKeys>,
        wallet_utxos: WalletUtxos,
    ) -> Result<Self, crate::common::wallet::TrackedWalletKeysError> {
        let mut accounting = Self::empty(tracked_wallets)?;

        for utxo in wallet_utxos.into_values().flatten() {
            accounting.add_owned_output(utxo);
        }

        Ok(accounting)
    }

    /// Restore scanner accounting, including service-lock state, from a
    /// persisted scanner snapshot.
    pub fn from_snapshot(
        tracked_wallets: Vec<TrackedWalletKeys>,
        snapshot: ScannerAccountingSnapshot,
    ) -> Result<Self, crate::common::wallet::TrackedWalletKeysError> {
        let mut accounting = Self::from_wallet_utxos(tracked_wallets, snapshot.wallet_utxos)?;
        let known_note_ids = accounting
            .wallet_utxos
            .values()
            .flat_map(BTreeMap::keys)
            .copied()
            .collect::<HashSet<_>>();
        accounting.service_note_ids = snapshot
            .locked_service_note_ids
            .intersection(&known_note_ids)
            .copied()
            .collect();
        Ok(accounting)
    }

    /// Capture all wallet UTXOs and their current service-lock state for a
    /// scanner checkpoint or persisted snapshot.
    #[must_use]
    pub fn snapshot(&self) -> ScannerAccountingSnapshot {
        ScannerAccountingSnapshot {
            wallet_utxos: self
                .wallet_utxos
                .iter()
                .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos.values().copied().collect()))
                .collect(),
            locked_service_note_ids: self.service_note_ids.clone(),
        }
    }

    fn empty(
        tracked_wallets: Vec<TrackedWalletKeys>,
    ) -> Result<Self, crate::common::wallet::TrackedWalletKeysError> {
        let mut public_key_to_wallet: HashMap<ZkPublicKey, WalletId> = HashMap::new();
        let mut wallet_utxos = BTreeMap::new();

        for tracked_wallet in &tracked_wallets {
            wallet_utxos
                .entry(tracked_wallet.wallet_id().clone())
                .or_default();
        }

        for tracked_wallet in &tracked_wallets {
            let wallet_id = tracked_wallet.wallet_id().clone();
            for pk in tracked_wallet.wallet_pks() {
                match public_key_to_wallet.entry(pk) {
                    Entry::Occupied(entry) if entry.get() != &wallet_id => {
                        return Err(
                            crate::common::wallet::TrackedWalletKeysError::DuplicatePublicKey {
                                public_key: pk,
                                first_wallet: entry.get().clone(),
                                second_wallet: wallet_id,
                            },
                        );
                    }
                    Entry::Occupied(_) => {}
                    Entry::Vacant(entry) => {
                        entry.insert(wallet_id.clone());
                    }
                }
            }
        }

        Ok(Self {
            tracked_wallets,
            public_key_to_wallet,
            wallet_utxos,
            service_note_ids: HashSet::new(),
            observed_transaction_hashes: BTreeSet::new(),
        })
    }

    /// Apply one block's transactions to tracked wallet state.
    pub fn apply_block(&mut self, block: &ApiBlock) {
        self.apply_block_with_events(block, &Events::new());
    }

    /// Apply one block's transactions and header events to tracked wallet
    /// state.
    pub fn apply_block_with_events(&mut self, block: &ApiBlock, events: &Events) {
        self.observe_block_transactions(block);

        // Header effects are applied before block transactions by the ledger.
        for event in events.iter() {
            if let Event::Header(HeaderEvent::SdpNoteUnlocked { note_id, .. }) = event {
                self.unlock_note(*note_id);
            }
        }

        for tx in &block.transactions {
            self.apply_transaction(tx);
        }
    }

    /// Record transaction hashes from a block without changing wallet UTXOs.
    pub fn observe_block_transactions(&mut self, block: &ApiBlock) {
        self.observed_transaction_hashes
            .extend(block.transactions.iter().map(SignedOps::hash));
    }

    #[must_use]
    /// Return currently unspent, unlocked UTXOs grouped by wallet id.
    pub fn wallet_utxos(&self) -> WalletUtxos {
        self.wallet_utxos
            .iter()
            .map(|(wallet_id, utxos_by_note)| {
                (
                    wallet_id.clone(),
                    utxos_by_note
                        .iter()
                        .filter_map(|(note_id, utxo)| {
                            (!self.service_note_ids.contains(note_id)).then_some(*utxo)
                        })
                        .collect(),
                )
            })
            .collect()
    }

    #[cfg(test)]
    /// Publish the current scanner UTXO view into tracked wallets for tests.
    pub fn publish_into(&self, wallets: &mut TrackedWallets) {
        wallets.replace_current_wallets_utxos(self.wallet_utxos());
    }

    #[must_use]
    /// Return all transaction hashes observed by this accounting instance.
    pub const fn observed_transaction_hashes(&self) -> &BTreeSet<TxHash> {
        &self.observed_transaction_hashes
    }

    #[must_use]
    /// Return the number of wallets tracked by this accounting instance.
    pub const fn tracked_wallet_count(&self) -> usize {
        self.tracked_wallets.len()
    }

    fn apply_transaction(&mut self, tx: &SignedOps<Unverified, StandardMode>) {
        for op in tx.op_refs() {
            match op {
                OpRef::Transfer(transfer) => {
                    for note_id in transfer.inputs.iter().copied() {
                        self.remove_spent_note(note_id);
                    }
                    for utxo in transfer.utxos() {
                        self.add_owned_output(utxo);
                    }
                }
                OpRef::ChannelDeposit(deposit) => {
                    // The deposit consumes its inputs and re-creates them as
                    // channel notes under a new NoteId. The re-created notes
                    // are channel-owned, which the wallet doesn't track, so
                    // only the spend is observed.
                    for note_id in deposit.inputs.iter().copied() {
                        self.remove_spent_note(note_id);
                    }
                }
                OpRef::SDPDeclare(declaration) => {
                    self.lock_note(declaration.service_note_id);
                }
                // Withdrawal itself does not release an SDP note; finalization
                // is observed through the block's `SdpNoteUnlocked` event.
                // Channel operations only move notes in and out of channel
                // ownership, which the wallet doesn't track.
                // TODO: observe released notes once channel notes are tracked.
                OpRef::SDPWithdraw(_)
                | OpRef::ChannelWithdraw(_)
                | OpRef::ChannelTransfer(_)
                | OpRef::ChannelConfig(_)
                | OpRef::ChannelInscribe(_)
                | OpRef::SDPActive(_)
                | OpRef::LeaderClaim(_)
                | OpRef::ClaimPowReward(_) => {}
            }
        }
    }

    fn remove_spent_note(&mut self, note_id: NoteId) {
        self.service_note_ids.remove(&note_id);
        for utxos_by_note in self.wallet_utxos.values_mut() {
            utxos_by_note.remove(&note_id);
        }
    }

    fn add_owned_output(&mut self, utxo: Utxo) {
        let Some(wallet_id) = self.public_key_to_wallet.get(&utxo.note.pk) else {
            return;
        };
        self.wallet_utxos
            .entry(wallet_id.clone())
            .or_default()
            .insert(utxo.id(), utxo);
    }

    fn lock_note(&mut self, note_id: NoteId) {
        if self
            .wallet_utxos
            .values()
            .any(|utxos_by_note| utxos_by_note.contains_key(&note_id))
        {
            self.service_note_ids.insert(note_id);
        }
    }

    fn unlock_note(&mut self, note_id: NoteId) {
        self.service_note_ids.remove(&note_id);
    }
}

#[cfg(test)]
mod tests {
    use lb_common_http_client::{ApiBlock, ApiHeader, Slot};
    use lb_core::{
        events::{Event, Events, HeaderEvent},
        header::{ContentId, HeaderId},
        mantle::{
            Note, SignedOps, Utxo,
            ledger::{BoundedInputs, Inputs, Outputs, verification_mode::StandardMode},
            ops::{
                Op,
                channel::{ChannelId, deposit::DepositOp},
                transfer::TransferOp,
            },
            traits::Hashable as _,
            transactions::{Ops, states::Unverified},
        },
        proofs::leader_proof::Groth16LeaderProof,
        sdp::{DeclarationMessage, Locator, Nonce, ProviderId, ServiceType, WithdrawMessage},
    };
    use lb_key_management_system_service::keys::Ed25519Key;

    use super::ScannerAccounting;
    use crate::common::wallet::{
        TrackedWalletKeys, TrackedWallets, WalletOutputState, WalletReservedInputs,
    };

    fn pk(value: u8) -> lb_key_management_system_service::keys::ZkPublicKey {
        lb_key_management_system_service::keys::ZkPublicKey::new(value.into())
    }

    fn utxo(
        value: u64,
        output_index: usize,
        pk: lb_key_management_system_service::keys::ZkPublicKey,
    ) -> Utxo {
        Utxo::new([output_index as u8; 32], output_index, Note::new(value, pk))
    }

    fn block(seed: u8, txs: Vec<SignedOps<Unverified, StandardMode>>) -> ApiBlock {
        ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([seed; 32]),
                parent_block: HeaderId::from([seed.saturating_sub(1); 32]),
                slot: Slot::from(u64::from(seed)),
                body_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            uncle_headers: Vec::new(),
            transactions: txs,
        }
    }

    /// A transaction creating `outputs`. A channel withdraw no longer creates
    /// notes, so a transfer is what the accounting observes owned outputs from.
    fn transfer_tx(outputs: [Note; 2]) -> SignedOps<Unverified, StandardMode> {
        let ops = Ops::from([Op::Transfer(TransferOp::new(
            Inputs::empty(),
            Outputs::new(outputs),
        ))]);
        SignedOps::from_ops_with_sample_proofs(ops)
    }

    fn sdp_declaration(
        service_note_id: lb_core::mantle::NoteId,
        service_type: ServiceType,
    ) -> DeclarationMessage {
        let provider_key = Ed25519Key::from_bytes(&[42; 32]).public_key();
        let locator: Locator = "/ip4/127.0.0.1/tcp/9100"
            .parse()
            .expect("locator should be valid");

        DeclarationMessage {
            service_type,
            locators: locator.into(),
            provider_id: ProviderId::from(provider_key),
            zk_id: pk(9),
            service_note_id,
        }
    }

    #[test]
    fn accounting_records_tx_hashes() {
        let tx = transfer_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![tx.clone()]));

        assert!(
            accounting
                .observed_transaction_hashes()
                .contains(&tx.hash())
        );
    }

    #[test]
    fn accounting_adds_wallet_owned_outputs() {
        let tx = transfer_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![tx]));

        let utxos = accounting.wallet_utxos();
        assert_eq!(utxos["alice"].len(), 1);
        assert_eq!(utxos["alice"][0].note.value, 10);
    }

    #[test]
    fn accounting_keeps_outputs_across_multiple_batches() {
        let first_tx = transfer_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
        let second_tx = transfer_tx([Note::new(30, pk(1)), Note::new(40, pk(2))]);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[])
                .expect("accounting should build");

        accounting.apply_block(&block(1, vec![first_tx]));
        accounting.apply_block(&block(2, vec![second_tx]));

        let mut values = accounting.wallet_utxos()["alice"]
            .iter()
            .map(|utxo| utxo.note.value)
            .collect::<Vec<_>>();
        values.sort_unstable();
        assert_eq!(values, vec![10, 30]);
    }

    #[test]
    fn accounting_removes_spent_utxos() {
        let owned = utxo(10, 0, pk(1));
        let ops = Ops::from([Op::ChannelDeposit(DepositOp {
            channel_id: ChannelId::from([0; 32]),
            inputs: BoundedInputs::from(owned.id()).into(),
            metadata: b"deposit".into(),
        })]);
        let spend = SignedOps::from_ops_with_sample_proofs(ops);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[owned])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![spend]));

        assert_eq!(accounting.wallet_utxos()["alice"], []);
    }

    #[test]
    fn accounting_ignores_unknown_outputs() {
        let tx = transfer_tx([Note::new(10, pk(9)), Note::new(20, pk(8))]);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![tx]));

        assert_eq!(accounting.wallet_utxos()["alice"], []);
    }

    #[test]
    fn withdrawal_restores_locked_utxo_only_after_final_unlock_event() {
        let locked = utxo(10, 0, pk(1));
        let declaration = sdp_declaration(locked.id(), ServiceType::BlendNetwork);
        let declare_ops = Ops::from([Op::SDPDeclare(declaration.clone())]);
        let declare_tx = SignedOps::from_ops_with_sample_proofs(declare_ops);
        let withdraw_ops = Ops::from([Op::SDPWithdraw(WithdrawMessage {
            declaration_id: declaration.id(),
            nonce: Nonce::new(0.into(), 0),
        })]);
        let withdraw_tx = SignedOps::from_ops_with_sample_proofs(withdraw_ops);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[locked])
                .expect("accounting should build");

        accounting.apply_block(&block(1, vec![declare_tx]));
        assert_eq!(accounting.wallet_utxos()["alice"], []);

        accounting.apply_block(&block(2, vec![withdraw_tx]));
        assert!(
            accounting.wallet_utxos()["alice"].is_empty(),
            "including withdrawal must not release collateral immediately"
        );

        let unlock_events = Events::from(Event::Header(HeaderEvent::SdpNoteUnlocked {
            note_id: locked.id(),
            service_type: ServiceType::BlendNetwork,
            declaration_id: declaration.id(),
        }));
        accounting.apply_block_with_events(&block(3, vec![]), &unlock_events);
        assert_eq!(accounting.wallet_utxos()["alice"][0].note.value, 10);
    }

    /// A persisted scanner checkpoint must retain both the collateral UTXO
    /// and its lock marker so a post-restore finalization event can unlock it.
    #[test]
    fn restored_snapshot_keeps_locked_collateral_until_final_unlock() {
        let locked = utxo(10, 0, pk(1));
        let declaration = sdp_declaration(locked.id(), ServiceType::BlendNetwork);
        let declare_tx = SignedOps::from_ops_with_sample_proofs(Ops::from([Op::SDPDeclare(
            declaration.clone(),
        )]));
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[locked])
                .expect("accounting should build");

        accounting.apply_block(&block(1, vec![declare_tx]));
        assert_eq!(accounting.wallet_utxos()["alice"], []);

        let serialized = serde_json::to_vec(&accounting.snapshot())
            .expect("scanner accounting snapshot should serialize");
        let snapshot = serde_json::from_slice(&serialized)
            .expect("scanner accounting snapshot should deserialize");
        let mut restored = ScannerAccounting::from_snapshot(
            vec![TrackedWalletKeys::new("alice", [pk(1)])],
            snapshot,
        )
        .expect("restored accounting should build");

        let withdraw_tx =
            SignedOps::from_ops_with_sample_proofs(Ops::from([Op::SDPWithdraw(WithdrawMessage {
                declaration_id: declaration.id(),
                nonce: Nonce::new(0.into(), 1),
            })]));
        restored.apply_block(&block(2, vec![withdraw_tx]));
        assert_eq!(restored.wallet_utxos()["alice"], []);

        let unlock_events = Events::from(Event::Header(HeaderEvent::SdpNoteUnlocked {
            note_id: locked.id(),
            service_type: ServiceType::BlendNetwork,
            declaration_id: declaration.id(),
        }));
        restored.apply_block_with_events(&block(3, vec![]), &unlock_events);
        restored.apply_block_with_events(&block(4, vec![]), &unlock_events);
        assert_eq!(restored.wallet_utxos()["alice"], vec![locked]);
    }

    /// A note shared across services remains unavailable when only one
    /// declaration is removed; duplicate final-release events are idempotent.
    #[test]
    fn shared_service_note_unlocks_only_after_last_service_release() {
        let locked = utxo(10, 0, pk(1));
        let declaration_a = sdp_declaration(locked.id(), ServiceType::BlendNetwork);
        let declaration_b = sdp_declaration(locked.id(), ServiceType::Test);
        let declare_a = SignedOps::from_ops_with_sample_proofs(Ops::from([Op::SDPDeclare(
            declaration_a.clone(),
        )]));
        let declare_b = SignedOps::from_ops_with_sample_proofs(Ops::from([Op::SDPDeclare(
            declaration_b.clone(),
        )]));
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[locked])
                .expect("accounting should build");

        accounting.apply_block(&block(1, vec![declare_a, declare_b]));
        let withdrawals = SignedOps::from_ops_with_sample_proofs(Ops::from([
            Op::SDPWithdraw(WithdrawMessage {
                declaration_id: declaration_a.id(),
                nonce: Nonce::new(0.into(), 1),
            }),
            Op::SDPWithdraw(WithdrawMessage {
                declaration_id: declaration_b.id(),
                nonce: Nonce::new(0.into(), 1),
            }),
        ]));
        accounting.apply_block(&block(2, vec![withdrawals]));
        assert_eq!(accounting.wallet_utxos()["alice"], []);

        // Ledger emits no final unlock event when A is removed because B still
        // owns this note.
        accounting.apply_block_with_events(&block(3, vec![]), &Events::new());
        assert_eq!(accounting.wallet_utxos()["alice"], []);

        let final_release = Events::from(Event::Header(HeaderEvent::SdpNoteUnlocked {
            note_id: locked.id(),
            service_type: ServiceType::Test,
            declaration_id: declaration_b.id(),
        }));
        accounting.apply_block_with_events(&block(4, vec![]), &final_release);
        accounting.apply_block_with_events(&block(5, vec![]), &final_release);
        assert_eq!(accounting.wallet_utxos()["alice"], vec![locked]);
    }

    #[test]
    fn publishing_updates_tracked_wallets() {
        let tx = transfer_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![tx]));
        let mut wallets = TrackedWallets::default();
        accounting.publish_into(&mut wallets);

        let state = wallets.current_wallet_states([TrackedWalletKeys::new("alice", [pk(1)])]);
        assert_eq!(
            state["alice"]
                .balance(WalletOutputState::OnChain)
                .output_count,
            1
        );
    }

    #[test]
    fn reservations_still_affect_available_balance() {
        let owned = utxo(10, 0, pk(1));
        let accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[owned])
                .expect("accounting should build");
        let mut wallets = TrackedWallets::default();
        accounting.publish_into(&mut wallets);
        wallets.record_wallet_reservation(
            "alice",
            lb_core::mantle::TxHash([1; 32]),
            WalletReservedInputs::new(vec![owned], Vec::new()),
            0,
        );

        let state = wallets.current_wallet_states([TrackedWalletKeys::new("alice", [pk(1)])]);
        assert_eq!(
            state["alice"]
                .balance(WalletOutputState::Available)
                .output_count,
            0
        );
        assert_eq!(
            state["alice"]
                .balance(WalletOutputState::Reserved)
                .output_count,
            1
        );
    }
}
