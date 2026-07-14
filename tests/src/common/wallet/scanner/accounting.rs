use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use lb_common_http_client::ApiBlock;
use lb_core::mantle::{NoteId, SignedMantleTx, Transaction as _, TxHash, Utxo, ops::Op};
use lb_key_management_system_service::keys::ZkPublicKey;

#[cfg(test)]
use crate::common::wallet::TrackedWallets;
use crate::common::wallet::{TrackedWalletKeys, WalletId, WalletUtxos};

#[derive(Clone)]
/// Wallet UTXO accounting derived by replaying scanned chain blocks.
pub struct ScannerAccounting {
    tracked_wallets: Vec<TrackedWalletKeys>,
    public_key_to_wallet: HashMap<ZkPublicKey, WalletId>,
    wallet_utxos: BTreeMap<WalletId, BTreeMap<NoteId, Utxo>>,
    locked_note_ids: HashSet<NoteId>,
    observed_transaction_hashes: BTreeSet<TxHash>,
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

    fn empty(
        tracked_wallets: Vec<TrackedWalletKeys>,
    ) -> Result<Self, crate::common::wallet::TrackedWalletKeysError> {
        let mut public_key_to_wallet = HashMap::new();
        let mut wallet_utxos = BTreeMap::new();

        for tracked_wallet in &tracked_wallets {
            wallet_utxos
                .entry(tracked_wallet.wallet_id().clone())
                .or_default();
        }

        for tracked_wallet in &tracked_wallets {
            let wallet_id = tracked_wallet.wallet_id().clone();
            for pk in tracked_wallet.wallet_pks() {
                if let Some(existing_wallet) = public_key_to_wallet.insert(pk, wallet_id.clone()) {
                    return Err(
                        crate::common::wallet::TrackedWalletKeysError::DuplicatePublicKey {
                            public_key: pk,
                            first_wallet: existing_wallet,
                            second_wallet: wallet_id,
                        },
                    );
                }
            }
        }

        Ok(Self {
            tracked_wallets,
            public_key_to_wallet,
            wallet_utxos,
            locked_note_ids: HashSet::new(),
            observed_transaction_hashes: BTreeSet::new(),
        })
    }

    /// Apply one block's transactions to tracked wallet state.
    pub fn apply_block(&mut self, block: &ApiBlock) {
        self.observe_block_transactions(block);

        for tx in &block.transactions {
            self.apply_transaction(tx);
        }
    }

    /// Record transaction hashes from a block without changing wallet UTXOs.
    pub fn observe_block_transactions(&mut self, block: &ApiBlock) {
        self.observed_transaction_hashes
            .extend(block.transactions.iter().map(SignedMantleTx::hash));
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
                            (!self.locked_note_ids.contains(note_id)).then_some(*utxo)
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

    fn apply_transaction(&mut self, tx: &SignedMantleTx) {
        for op in tx.mantle_tx.ops() {
            match op {
                Op::Transfer(transfer) => {
                    for note_id in transfer.inputs.iter().copied() {
                        self.remove_spent_note(note_id);
                    }
                    for utxo in transfer.outputs.utxos(transfer) {
                        self.add_owned_output(utxo);
                    }
                }
                Op::ChannelDeposit(deposit) => {
                    for note_id in deposit.inputs.iter().copied() {
                        self.remove_spent_note(note_id);
                    }
                }
                Op::ChannelWithdraw(withdraw) => {
                    for utxo in withdraw.outputs.utxos(withdraw) {
                        self.add_owned_output(utxo);
                    }
                }
                Op::SDPDeclare(declaration) => {
                    self.lock_note(declaration.locked_note_id);
                }
                Op::SDPWithdraw(withdrawal) => {
                    self.unlock_note(withdrawal.locked_note_id);
                }
                Op::ChannelConfig(_)
                | Op::ChannelInscribe(_)
                | Op::SDPActive(_)
                | Op::LeaderClaim(_) => {}
            }
        }
    }

    fn remove_spent_note(&mut self, note_id: NoteId) {
        self.locked_note_ids.remove(&note_id);
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
            self.locked_note_ids.insert(note_id);
        }
    }

    fn unlock_note(&mut self, note_id: NoteId) {
        self.locked_note_ids.remove(&note_id);
    }
}

#[cfg(test)]
mod tests {
    use lb_common_http_client::{ApiBlock, ApiHeader, Slot};
    use lb_core::{
        header::{ContentId, HeaderId},
        mantle::{
            MantleTx, Note, SignedMantleTx, Transaction as _, Utxo,
            ledger::{Inputs, Outputs},
            ops::{
                Op,
                channel::{ChannelId, deposit::DepositOp, withdraw::ChannelWithdrawOp},
            },
        },
        proofs::leader_proof::Groth16LeaderProof,
        sdp::{DeclarationMessage, Locator, ProviderId, ServiceType, WithdrawMessage},
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

    fn block(seed: u8, txs: Vec<SignedMantleTx>) -> ApiBlock {
        ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([seed; 32]),
                parent_block: HeaderId::from([seed.saturating_sub(1); 32]),
                slot: Slot::from(u64::from(seed)),
                block_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            transactions: txs,
        }
    }

    fn withdraw_tx(outputs: [Note; 2]) -> SignedMantleTx {
        SignedMantleTx::new_unverified(
            MantleTx(
                [Op::ChannelWithdraw(ChannelWithdrawOp {
                    channel_id: ChannelId::from([0; 32]),
                    outputs: Outputs::new(outputs),
                    withdraw_nonce: 0,
                })]
                .into(),
            ),
            Vec::new(),
        )
    }

    fn sdp_declaration(locked_note_id: lb_core::mantle::NoteId) -> DeclarationMessage {
        let provider_key = Ed25519Key::from_bytes(&[42; 32]).public_key();
        let locator: Locator = "/ip4/127.0.0.1/tcp/9100"
            .parse()
            .expect("locator should be valid");

        DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: locator.into(),
            provider_id: ProviderId::from(provider_key),
            zk_id: pk(9),
            locked_note_id,
        }
    }

    #[test]
    fn accounting_records_tx_hashes() {
        let tx = withdraw_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
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
        let tx = withdraw_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
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
        let first_tx = withdraw_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
        let second_tx = withdraw_tx([Note::new(30, pk(1)), Note::new(40, pk(2))]);
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
        let spend = SignedMantleTx::new_unverified(
            MantleTx(
                [Op::ChannelDeposit(DepositOp {
                    channel_id: ChannelId::from([0; 32]),
                    inputs: Inputs::from([owned.id()]),
                    metadata: b"deposit".into(),
                })]
                .into(),
            ),
            Vec::new(),
        );
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[owned])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![spend]));

        assert!(accounting.wallet_utxos()["alice"].is_empty());
    }

    #[test]
    fn accounting_ignores_unknown_outputs() {
        let tx = withdraw_tx([Note::new(10, pk(9)), Note::new(20, pk(8))]);
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[])
                .expect("accounting should build");
        accounting.apply_block(&block(1, vec![tx]));

        assert!(accounting.wallet_utxos()["alice"].is_empty());
    }

    #[test]
    fn sdp_declare_hides_and_withdraw_restores_locked_utxo() {
        let locked = utxo(10, 0, pk(1));
        let declaration = sdp_declaration(locked.id());
        let declare_tx = SignedMantleTx::new_unverified(
            MantleTx([Op::SDPDeclare(declaration.clone())].into()),
            Vec::new(),
        );
        let withdraw_tx = SignedMantleTx::new_unverified(
            MantleTx(
                [Op::SDPWithdraw(WithdrawMessage {
                    declaration_id: declaration.id(),
                    locked_note_id: locked.id(),
                    nonce: 0,
                })]
                .into(),
            ),
            Vec::new(),
        );
        let mut accounting =
            ScannerAccounting::new(vec![TrackedWalletKeys::new("alice", [pk(1)])], &[locked])
                .expect("accounting should build");

        accounting.apply_block(&block(1, vec![declare_tx]));
        assert!(accounting.wallet_utxos()["alice"].is_empty());

        accounting.apply_block(&block(2, vec![withdraw_tx]));
        assert_eq!(accounting.wallet_utxos()["alice"][0].note.value, 10);
    }

    #[test]
    fn publishing_updates_tracked_wallets() {
        let tx = withdraw_tx([Note::new(10, pk(1)), Note::new(20, pk(2))]);
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
