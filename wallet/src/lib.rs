pub mod error;
mod voucher;

use std::{
    borrow::Borrow,
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Debug,
};

pub use error::WalletError;
use lb_core::{
    block::Block,
    crypto::ZkHasher,
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, GasConstants, NoteId, Utxo, Value,
        ops::{
            Op,
            leader_claim::{VoucherCm, VoucherNullifier},
            transfer::TransferOp,
        },
        tx_builder::MantleTxBuilder,
    },
    proofs::leader_proof::LeaderProof as _,
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_ledger::LedgerState;
use lb_mmr::{MerkleMountainRange, MerklePath};
use serde::{Deserialize, Serialize};
use tracing::info;

pub use crate::voucher::Vouchers;

/// A lightweight block information necessary for wallet
pub struct WalletBlock {
    pub id: HeaderId,
    pub parent: HeaderId,
    pub epoch: Epoch,
    pub voucher_cm: VoucherCm,
    pub spent_notes: Vec<NoteId>,
    pub transfers: Vec<TransferOp>,
    pub locked_notes: HashSet<NoteId>,
    pub unlocked_notes: HashSet<NoteId>,
}

impl WalletBlock {
    #[must_use]
    pub fn from_block<Tx>(block: &Block<Tx>, epoch: Epoch) -> Self
    where
        Tx: AuthenticatedMantleTx,
    {
        // TODO: handle inputs/outputs of ALL operations: https://github.com/logos-blockchain/logos-blockchain/issues/2627
        let mut spent_notes = Vec::new();
        let mut transfers = Vec::new();
        let mut locked_notes = HashSet::new();
        let mut unlocked_notes = HashSet::new();

        for auth_tx in block.transactions() {
            for op in auth_tx.mantle_tx().ops() {
                match op {
                    Op::ChannelDeposit(deposit) => {
                        spent_notes.extend(deposit.inputs.iter().copied());
                    }
                    Op::Transfer(transfer) => {
                        spent_notes.extend(transfer.inputs.iter().copied());
                        transfers.push(transfer.clone());
                    }
                    Op::SDPDeclare(declaration) => {
                        locked_notes.insert(declaration.locked_note_id);
                    }
                    Op::SDPWithdraw(withdrawal) => {
                        unlocked_notes.insert(withdrawal.locked_note_id);
                    }
                    _ => {}
                }
            }
        }

        Self {
            id: block.header().id(),
            parent: block.header().parent(),
            epoch,
            voucher_cm: *block.header().leader_proof().voucher_cm(),
            spent_notes,
            transfers,
            locked_notes,
            unlocked_notes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletState {
    pub utxos: rpds::HashTrieMapSync<NoteId, Utxo>,
    pub pk_index: rpds::HashTrieMapSync<ZkPublicKey, rpds::HashTrieSetSync<NoteId>>,
    pub locked_notes: rpds::HashTrieSetSync<NoteId>,
    pub epoch: Epoch,
    /// MMR of all voucher commitments included in the chain
    pub vouchers: MerkleMountainRange<VoucherCm, ZkHasher>,
    /// All **tracked** voucher merkle paths up to the current block
    pub voucher_paths: VoucherPaths,
    /// A snapshot of **tracked** voucher merkle paths,
    /// updated at the first block of each epoch.
    pub voucher_paths_snapshot: VoucherPaths,
}

pub type VoucherPaths = rpds::HashTrieMapSync<VoucherCm, MerklePath>;

impl WalletState {
    pub fn from_ledger<KeyId>(
        known_keys: &HashMap<ZkPublicKey, KeyId>,
        ledger: &LedgerState,
    ) -> Self {
        let mut utxos = rpds::HashTrieMapSync::new_sync();
        let mut pk_index = rpds::HashTrieMapSync::new_sync();
        let mut locked_notes = rpds::HashTrieSetSync::new_sync();

        let all_locked_notes = ledger.mantle_ledger().sdp_ledger().locked_notes();
        for (_, (utxo, _)) in ledger.latest_utxos().utxos().iter() {
            if known_keys.contains_key(&utxo.note.pk) {
                let note_id = utxo.id();
                utxos = utxos.insert(note_id, *utxo);

                let note_set = pk_index
                    .get(&utxo.note.pk)
                    .cloned()
                    .unwrap_or_else(rpds::HashTrieSetSync::new_sync)
                    .insert(note_id);
                pk_index = pk_index.insert(utxo.note.pk, note_set);

                if all_locked_notes.contains(&note_id) {
                    locked_notes = locked_notes.insert(note_id);
                }
            }
        }

        Self {
            utxos,
            pk_index,
            locked_notes,
            epoch: ledger.epoch_state().epoch,
            vouchers: ledger.mantle_ledger().vouchers().clone(),
            voucher_paths: rpds::HashTrieMapSync::new_sync(),
            voucher_paths_snapshot: rpds::HashTrieMapSync::new_sync(),
        }
    }

    pub fn utxos_owned_by_pks(
        &self,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
    ) -> Vec<Utxo> {
        pks.into_iter()
            .filter_map(|pk| self.pk_index.get(pk.borrow()))
            .flatten()
            .map(|id| self.utxos[id])
            .collect()
    }

    pub fn fund_tx<G: GasConstants>(
        &self,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
    ) -> Result<MantleTxBuilder, WalletError> {
        // Get all UTXOs owned by the provided PKs, excluding the following notes:
        // - Notes that are being consumed/locked by the tx
        // - Notes that are already locked in Ledger
        let consumed_or_locked = tx_builder
            .consumed_or_locked_notes()
            .chain(self.locked_notes.iter().copied())
            .collect::<HashSet<_>>();
        let mut utxos = self
            .utxos_owned_by_pks(pks)
            .into_iter()
            .filter(|utxo| !consumed_or_locked.contains(&utxo.id()))
            .collect::<Vec<_>>();

        // Consume large valued notes first to ensure we converge.
        utxos.sort_by_key(|utxo| -i128::from(utxo.note.value));

        for i in 0..utxos.len() {
            let funded_tx_builder = tx_builder
                .clone()
                .extend_ledger_inputs(utxos[..=i].iter().copied());

            let funding_delta = funded_tx_builder.funding_delta::<G>()?;

            match funding_delta.cmp(&0) {
                Ordering::Less => {
                    // Insufficient funds, need more UTXO's.
                }
                Ordering::Equal => {
                    // We can exactly pay the tx cost, no change note needed.
                    return Ok(funded_tx_builder);
                }
                Ordering::Greater => {
                    // We have enough balance, but we need to introduce a change note.
                    // The change note will slightly increase the storage cost of the tx so there is
                    // a chance that we will not be able to fund the tx with the change note.
                    if let Some(tx_with_change) = funded_tx_builder.return_change::<G>(change_pk)? {
                        // We were able to fund the tx with change note added.
                        return Ok(tx_with_change);
                    }
                    // Otherwise, need more UTXO's.
                }
            }
        }

        Err(WalletError::InsufficientFunds {
            available: utxos.iter().map(|u| u.note.value).sum::<u64>(),
        })
    }

    #[must_use]
    pub fn balance(&self, pk: ZkPublicKey) -> Option<WalletBalance> {
        let mut balance = WalletBalance {
            balance: 0,
            notes: HashMap::new(),
        };

        self.pk_index.get(&pk)?.iter().for_each(|id| {
            let value = self.utxos[id].note.value;
            balance.balance += value;
            balance.notes.insert(*id, value);
        });

        Some(balance)
    }

    #[must_use]
    pub fn apply_block<KeyId, VoucherId>(
        &self,
        known_keys: &HashMap<ZkPublicKey, KeyId>,
        known_vouchers: &Vouchers<VoucherId>,
        block: &WalletBlock,
    ) -> Self {
        let mut utxos = self.utxos.clone();
        let mut pk_index = self.pk_index.clone();
        let mut locked_notes = self.locked_notes.clone();

        for spent_id in &block.spent_notes {
            remove_spent_utxo(spent_id, &mut utxos, &mut pk_index);
        }

        for transfer in &block.transfers {
            // Add new UTXOs (outputs) - only if they belong to our known keys
            for utxo in transfer.outputs.utxos(transfer) {
                if known_keys.contains_key(&utxo.note.pk) {
                    let note_id = utxo.id();
                    utxos = utxos.insert(note_id, utxo);

                    let note_set = pk_index
                        .get(&utxo.note.pk)
                        .cloned()
                        .unwrap_or_else(rpds::HashTrieSetSync::new_sync)
                        .insert(note_id);
                    pk_index = pk_index.insert(utxo.note.pk, note_set);
                }
            }
        }

        for locked_note in &block.locked_notes {
            if utxos.contains_key(locked_note) {
                locked_notes = locked_notes.insert(*locked_note);
            }
        }
        for unlocked_note in &block.unlocked_notes {
            locked_notes = locked_notes.remove(unlocked_note);
        }

        let (vouchers, voucher_paths, voucher_paths_snapshot) =
            self.apply_voucher(known_vouchers, block);

        Self {
            utxos,
            pk_index,
            locked_notes,
            epoch: block.epoch,
            vouchers,
            voucher_paths,
            voucher_paths_snapshot,
        }
    }

    /// Apply the voucher commitment from the block to the wallet state.
    ///
    /// Returns:
    /// - Updated MMR including the new voucher commitment
    /// - Updated tracked voucher paths, including the new voucher if it is
    ///   owned by us
    /// - Snapshot of trakced voucher paths (updated only if epoch is advancing)
    fn apply_voucher<VoucherId>(
        &self,
        known_vouchers: &Vouchers<VoucherId>,
        block: &WalletBlock,
    ) -> (
        MerkleMountainRange<VoucherCm, ZkHasher>,
        VoucherPaths,
        VoucherPaths,
    ) {
        // Snapshot voucher paths if epoch is advancing
        let snapshot = if block.epoch > self.epoch {
            self.voucher_paths.clone()
        } else {
            self.voucher_paths_snapshot.clone()
        };

        // Filter out vouchers that have been claimed/finalized.
        // `known_vouchers` always reflects the latest set of vouchers to be tracked.
        let (cms, mut paths): (Vec<VoucherCm>, Vec<MerklePath>) = self
            .voucher_paths
            .iter()
            .filter(|(cm, _)| known_vouchers.get(cm).is_some())
            .map(|(cm, path)| (*cm, path.clone()))
            .unzip();

        // Push the new voucher to the MMR and update all tracked paths
        let (vouchers, new_path) = self
            .vouchers
            .push_with_paths(block.voucher_cm, &mut paths)
            .expect("vouchers MMR shouldn't be full");

        // Rebuild the tracked voucher paths map with updated paths.
        let mut voucher_paths = rpds::HashTrieMapSync::new_sync();
        for (cm, path) in cms.into_iter().zip(paths) {
            voucher_paths = voucher_paths.insert(cm, path);
        }

        // Track the new voucher's path if it is owned by us
        if known_vouchers.get(&block.voucher_cm).is_some() {
            voucher_paths = voucher_paths.insert(block.voucher_cm, new_path);
        }

        (vouchers, voucher_paths, snapshot)
    }
}

fn remove_spent_utxo(
    spent_id: &NoteId,
    utxos: &mut rpds::HashTrieMapSync<NoteId, Utxo>,
    pk_index: &mut rpds::HashTrieMapSync<ZkPublicKey, rpds::HashTrieSetSync<NoteId>>,
) {
    let Some(utxo) = utxos.get(spent_id) else {
        return;
    };

    let pk = utxo.note.pk;
    *utxos = utxos.remove(spent_id);

    let Some(note_set) = pk_index.get(&pk) else {
        return;
    };

    let updated_set = note_set.remove(spent_id);
    if updated_set.is_empty() {
        *pk_index = pk_index.remove(&pk);
    } else {
        *pk_index = pk_index.insert(pk, updated_set);
    }
}

#[derive(Clone)]
pub struct Wallet<KeyId, VoucherId> {
    known_keys: HashMap<ZkPublicKey, KeyId>,
    known_vouchers: Vouchers<VoucherId>,
    wallet_states: BTreeMap<HeaderId, WalletState>,
}

impl<KeyId, VoucherId> Wallet<KeyId, VoucherId>
where
    VoucherId: Debug,
{
    /// Initialize [`Wallet`] from a given [`LedgerState`] at LIB.
    ///
    /// It initializes empty Merkle paths for all known vouchers,
    /// which will be updated as new blocks are applied.
    pub fn from_lib_ledger_state(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_vouchers: Vouchers<VoucherId>,
        lib: HeaderId,
        ledger: &LedgerState,
    ) -> Self {
        let known_keys = known_keys.into_iter().collect();
        let wallet_state = WalletState::from_ledger(&known_keys, ledger);

        info!(
            ?lib,
            n_known_keys = known_keys.len(),
            n_known_vouchers = known_vouchers.count(),
            n_all_vouchers = wallet_state.vouchers.len(),
            "initializing wallet with LIB ledger state"
        );

        Self {
            known_keys,
            known_vouchers,
            wallet_states: [(lib, wallet_state)].into(),
        }
    }

    /// Initialize [`Wallet`] from a given [`WalletState`] at LIB
    /// (e.g., restored from persisted state).
    ///
    /// Tracking of Merkle paths  for known vouchers starts from the paths
    /// stored in the [`WalletState`].
    pub fn from_lib_wallet_state(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_vouchers: Vouchers<VoucherId>,
        lib: HeaderId,
        wallet_state: WalletState,
    ) -> Self {
        let known_keys = known_keys.into_iter().collect::<HashMap<_, _>>();

        info!(
            ?lib,
            n_known_keys = known_keys.len(),
            n_known_vouchers = known_vouchers.count(),
            n_all_vouchers = wallet_state.vouchers.len(),
            n_voucher_paths = wallet_state.voucher_paths.size(),
            n_snapshotted_voucher_paths = wallet_state.voucher_paths_snapshot.size(),
            "initializing wallet with LIB wallet state"
        );

        Self {
            known_keys,
            known_vouchers,
            wallet_states: [(lib, wallet_state)].into(),
        }
    }

    #[must_use]
    pub const fn known_keys(&self) -> &HashMap<ZkPublicKey, KeyId> {
        &self.known_keys
    }

    pub fn add_known_voucher(&mut self, cm: VoucherCm, nf: VoucherNullifier, id: VoucherId) {
        self.known_vouchers.insert(cm, nf, id);
    }

    #[must_use]
    pub fn get_voucher_by_nullifier(&self, nf: &VoucherNullifier) -> Option<&VoucherId> {
        self.known_vouchers.get_by_nullifier(nf)
    }

    pub fn voucher_commitments_and_nullifiers(
        &self,
    ) -> impl Iterator<Item = (&VoucherNullifier, &VoucherCm)> {
        self.known_vouchers.commitments_and_nullifiers()
    }

    #[must_use]
    pub const fn vouchers(&self) -> &Vouchers<VoucherId> {
        &self.known_vouchers
    }

    #[must_use]
    pub fn has_processed_block(&self, block_id: HeaderId) -> bool {
        self.wallet_states.contains_key(&block_id)
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        if self.wallet_states.contains_key(&block.id) {
            // Already processed this block
            return Ok(());
        }

        let block_wallet_state = self.wallet_state_at(block.parent)?.apply_block(
            &self.known_keys,
            &self.known_vouchers,
            block,
        );
        self.wallet_states.insert(block.id, block_wallet_state);
        Ok(())
    }

    /// Get the snapshotted Merkle path for a voucher.
    pub fn voucher_path_snapshot(
        &self,
        tip: HeaderId,
        cm: &VoucherCm,
    ) -> Result<Option<MerklePath>, WalletError> {
        Ok(self
            .wallet_state_at(tip)?
            .voucher_paths_snapshot
            .get(cm)
            .cloned())
    }

    /// Get the Merkle path for a voucher that is not yet snapshotted
    /// (only for testing)
    #[cfg(test)]
    fn voucher_path(
        &self,
        tip: HeaderId,
        cm: &VoucherCm,
    ) -> Result<Option<MerklePath>, WalletError> {
        Ok(self.wallet_state_at(tip)?.voucher_paths.get(cm).cloned())
    }

    pub fn balance(
        &self,
        tip: HeaderId,
        pk: ZkPublicKey,
    ) -> Result<Option<WalletBalance>, WalletError> {
        Ok(self.wallet_state_at(tip)?.balance(pk))
    }

    pub fn fund_tx<G: GasConstants>(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
    ) -> Result<MantleTxBuilder, WalletError> {
        self.wallet_state_at(tip)?
            .fund_tx::<G>(tx_builder, change_pk, funding_pks)
    }

    pub fn wallet_state_at(&self, tip: HeaderId) -> Result<WalletState, WalletError> {
        self.wallet_states
            .get(&tip)
            .cloned()
            .ok_or(WalletError::UnknownBlock(tip))
    }

    /// Prune wallet states for blocks that have been pruned from the chain.
    ///
    /// This removes wallet states for blocks that are no longer part of the
    /// chain after LIB advancement. Both stale blocks (from abandoned
    /// forks) and immutable blocks (before the new LIB) are removed.
    pub fn prune_states(&mut self, pruned_blocks: impl IntoIterator<Item = HeaderId>) {
        let mut removed_count = 0;

        for block_id in pruned_blocks {
            if self.wallet_states.remove(&block_id).is_some() {
                removed_count += 1;
            }
        }

        if removed_count > 0 {
            tracing::trace!(
                removed_states = removed_count,
                remaining_states = self.wallet_states.len(),
                "Pruned wallet states for pruned blocks"
            );
        }
    }

    pub fn prune_vouchers(
        &mut self,
        immutable_transactions: impl IntoIterator<Item = VoucherNullifier>,
    ) {
        for voucher_nullifier in immutable_transactions {
            if let Some(id) = self.known_vouchers.remove_by_nullifier(&voucher_nullifier) {
                tracing::trace!("Pruned voucher {:?} from wallet", id);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletBalance {
    pub balance: Value,
    pub notes: HashMap<NoteId, Value>,
}

#[cfg(test)]
mod tests {
    use std::{
        iter::empty,
        num::{NonZero, NonZeroU64},
        sync::Arc,
    };

    use lb_core::{
        crypto::{Hash, ZkDigest as _},
        mantle::{
            Note,
            channel::Channels,
            gas::MainnetGasConstants as Gas,
            ledger::{Inputs, Outputs},
            ops::channel::{ChannelId, MsgId, inscribe::InscriptionOp},
            tx::{GasPrices, MantleTxContext, MantleTxGasContext},
        },
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::Ed25519Key;
    use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards};
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
    use num_bigint::BigUint;
    use rpds::HashTrieSetSync;

    use super::*;

    fn pk(v: u64) -> ZkPublicKey {
        ZkPublicKey::from(BigUint::from(v))
    }

    fn tx_hash(v: u8) -> Hash {
        [v; 32]
    }

    fn voucher(key_id: u64, idx: u64) -> (VoucherCm, VoucherNullifier) {
        let secret =
            ZkHasher::digest(&[BigUint::from(key_id).into(), BigUint::from(idx).into()]).into();
        (
            VoucherCm::from_secret(secret),
            VoucherNullifier::from_secret(secret),
        )
    }

    type TestVoucherId = (u64, u64);

    #[test]
    fn test_initialization() {
        let alice = pk(1);
        let bob = pk(2);
        let (voucher_master_key, voucher_index) = (100, 0);
        let (voucher_cm, voucher_nf) = voucher(voucher_master_key, voucher_index);

        let genesis = HeaderId::from([0; 32]);

        let ledger = LedgerState::from_utxos(
            [
                Utxo::new(tx_hash(0), 0, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 1, Note::new(20, bob)),
                Utxo::new(tx_hash(0), 2, Note::new(4, alice)),
            ],
            &ledger_config(),
        );

        let wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            empty::<(ZkPublicKey, u64)>(),
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);
        assert!(wallet.vouchers().get(&voucher_cm).is_none());

        let wallet = Wallet::from_lib_ledger_state(
            [(alice, 1)],
            Vouchers::new([(voucher_cm, voucher_nf, (voucher_master_key, voucher_index))]),
            genesis,
            &ledger,
        );
        assert_eq!(
            wallet.balance(genesis, alice).unwrap().unwrap().balance,
            104
        );
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);
        assert_eq!(
            wallet.vouchers().get(&voucher_cm),
            Some(&(voucher_master_key, voucher_index))
        );

        let wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(bob, 2)],
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap().unwrap().balance, 20);

        let wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1), (bob, 2)],
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(
            wallet.balance(genesis, alice).unwrap().unwrap().balance,
            104
        );
        assert_eq!(wallet.balance(genesis, bob).unwrap().unwrap().balance, 20);
    }

    #[test]
    fn test_sync() {
        let alice = pk(1);
        let bob = pk(2);

        let genesis = HeaderId::from([0; 32]);

        let genesis_ledger = LedgerState::from_utxos([], &ledger_config());

        let (v1_cm, v1_nf) = voucher(1, 0);
        let (v2_cm, v2_nf) = voucher(1, 1);
        let (v3_cm, _v3_nf) = voucher(2, 0);

        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1), (bob, 2)],
            Vouchers::new([(v1_cm, v1_nf, (1, 0)), (v2_cm, v2_nf, (1, 1))]),
            genesis,
            &genesis_ledger,
        );

        // Block 1 (epoch 1)
        // - alice is minted 104 NMO in two notes (100 NMO and 4 NMO)
        // - voucher v1 is ours -> should be tracked
        let transfer1 = TransferOp {
            inputs: Inputs::new(vec![]),
            outputs: Outputs::new(vec![Note::new(100, alice), Note::new(4, alice)]),
        };
        // immediately lock the 2nd note from `transfer1`
        let locked_note = transfer1.outputs.utxo_by_index(1, &transfer1).unwrap().id();

        let block_1 = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v1_cm,
            spent_notes: vec![],
            transfers: vec![transfer1.clone()],
            locked_notes: HashSet::from([locked_note]),
            // Unknown unlocked note that will be ignored
            unlocked_notes: HashSet::from([NoteId::from(Fr::ONE)]),
        };

        wallet.apply_block(&block_1).unwrap();
        assert_locked_notes(&wallet, block_1.id, [locked_note]);
        // v1 is tracked but not yet claimable (no epoch transition yet)
        assert_tracked_but_not_snapshotted_voucher(&wallet, block_1.id, &v1_cm);

        // Block 2 (epoch 2) -- epoch transition snapshots v1's path as claimable
        //  - alice spends 100 NMO utxo, sending 20 NMO to bob and 80 to herself
        // - voucher v2 is ours -> should be tracked
        let alice_100_nmo_utxo = transfer1.outputs.utxo_by_index(0, &transfer1).unwrap();

        let block_2 = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: block_1.id,
            epoch: 2.into(),
            voucher_cm: v2_cm,
            spent_notes: vec![alice_100_nmo_utxo.id()],
            transfers: vec![TransferOp {
                inputs: Inputs::new(vec![alice_100_nmo_utxo.id()]),
                outputs: Outputs::new(vec![Note::new(20, bob), Note::new(80, alice)]),
            }],
            // Unknown locked note that will be ignored
            locked_notes: HashSet::from([NoteId::from(Fr::ONE)]),
            // Unlock the previously locked note
            unlocked_notes: HashSet::from([locked_note]),
        };
        wallet.apply_block(&block_2).unwrap();
        assert_locked_notes(&wallet, block_2.id, []);
        // v1 is now claimable after epoch transition
        assert_snapshotted_voucher(&wallet, block_2.id, &v1_cm);
        // v2 is ours, but not yet snapshotted
        assert_tracked_but_not_snapshotted_voucher(&wallet, block_2.id, &v2_cm);

        // Query the balance of for each pk at different points in the blockchain
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);

        assert_eq!(
            wallet.balance(block_1.id, alice).unwrap().unwrap().balance,
            104
        );
        assert_eq!(wallet.balance(block_1.id, bob).unwrap(), None);

        assert_eq!(
            wallet.balance(block_2.id, alice).unwrap().unwrap().balance,
            84
        );
        assert_eq!(
            wallet.balance(block_2.id, bob).unwrap().unwrap().balance,
            20
        );

        // Block 3 (still, epoch 2)
        // - alice spends the 80 NMO note through a non-transfer operation.
        // - voucher v3 is not ours -> should not be tracked
        let alice_80_nmo_utxo = block_2.transfers[0]
            .outputs
            .utxo_by_index(1, &block_2.transfers[0])
            .unwrap();

        let block_3 = WalletBlock {
            id: HeaderId::from([3; 32]),
            parent: block_2.id,
            epoch: 2.into(),
            voucher_cm: v3_cm,
            spent_notes: vec![alice_80_nmo_utxo.id()],
            transfers: vec![],
            locked_notes: HashSet::new(),
            unlocked_notes: HashSet::new(),
        };
        wallet.apply_block(&block_3).unwrap();

        assert_eq!(
            wallet.balance(block_3.id, alice).unwrap().unwrap().balance,
            4
        );
        assert_eq!(
            wallet.balance(block_3.id, bob).unwrap().unwrap().balance,
            20
        );

        // v1 is still claimable
        assert_snapshotted_voucher(&wallet, block_3.id, &v1_cm);
        // v2 is still not snapshotted
        assert_tracked_but_not_snapshotted_voucher(&wallet, block_3.id, &v2_cm);
        // v3 is not ours, so not tracked at all
        assert_not_tracked_voucher(&wallet, block_3.id, &v3_cm);
    }

    #[test]
    fn test_fund_tx_with_change() {
        let alice = pk(1);
        let utxo1 = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));
        let utxo2 = Utxo::new(tx_hash(0), 1, Note::new(5000, alice));
        let ledger_state = LedgerState::from_utxos([utxo1, utxo2], &ledger_config());

        let mut wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);
        // Lock `utxo1` deliberately to ensure that `fund_tx` excludes locked notes
        wallet_state.locked_notes = wallet_state.locked_notes.insert(utxo1.id());

        let tx_builder = MantleTxBuilder::new(MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        });

        // Fund the transaction
        let funded_tx_builder = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice])
            .unwrap();

        assert_eq!(
            794,
            funded_tx_builder.gas_cost::<Gas>().unwrap().into_inner()
        );
        assert_eq!(794, funded_tx_builder.net_balance());
        assert_eq!(0, funded_tx_builder.funding_delta::<Gas>().unwrap());

        let funded_tx = funded_tx_builder.build();

        if let Op::Transfer(transfer_op) = &funded_tx.ops()[funded_tx.ops().len() - 1] {
            // ensure alices utxo was used to pay the fee
            assert_eq!(transfer_op.inputs, Inputs::new(vec![utxo2.id()]));
            // ensure change was returned to alice
            assert_eq!(
                transfer_op.outputs,
                Outputs::new(vec![Note {
                    value: 4206,
                    pk: alice,
                }])
            );
        } else {
            panic!("last op must be a transfer")
        }
    }

    #[test]
    fn test_fund_tx_insufficient_funds() {
        let alice = pk(1);
        let ledger_state = LedgerState::from_utxos(
            [
                Utxo::new(tx_hash(0), 0, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 1, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 2, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 3, Note::new(100, alice)),
            ],
            &ledger_config(),
        );

        let builder_context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                ledger_state.mantle_ledger().channels(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: ledger_state.mantle_ledger().leader_reward_amount(),
        };

        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);
        let mut tx_builder = MantleTxBuilder::new(builder_context);

        // Add a costly inscription
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscription = Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: vec![0xAB; 1000],
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        });

        tx_builder = tx_builder.push_op(inscription);

        // Fund the transaction
        let fund_attempt = wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice]);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 400 }
        );
    }

    #[test]
    fn test_fund_tx_zero_funds() {
        let alice = pk(1);
        let ledger_state = LedgerState::from_utxos([], &ledger_config());

        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);

        let tx_builder = MantleTxBuilder::new(ledger_state.tx_context());

        // Fund the transaction
        let fund_attempt = wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice]);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn test_fund_tx_all_locked_notes() {
        let alice = pk(1);
        let utxo = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));
        let ledger_state = LedgerState::from_utxos([utxo], &ledger_config());

        let mut wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);
        // Lock `utxo` deliberately to ensure that `fund_tx` excludes locked notes
        wallet_state.locked_notes = wallet_state.locked_notes.insert(utxo.id());

        let tx_builder = MantleTxBuilder::new(ledger_state.tx_context());

        // Fund the transaction
        let fund_attempt = wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice]);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn test_fund_tx_respects_pk_list() {
        let alice = pk(1);
        let bob = pk(2);
        let ledger_state = LedgerState::from_utxos(
            [Utxo::new(tx_hash(0), 0, Note::new(1_000_000, bob))],
            &ledger_config(),
        );

        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1), (bob, 2)]), &ledger_state);

        let tx_builder = MantleTxBuilder::new(ledger_state.tx_context());

        // Attempt to fund the transaction with Alice's notes.
        let fund_attempt = wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice]);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );

        // Fund the transaction with Bob's notes.
        wallet_state
            .fund_tx::<Gas>(&tx_builder, bob, [bob])
            .unwrap(); // succesfully funded;
    }

    #[test]
    fn test_fund_tx_unfundable_region() {
        let alice = pk(1);

        let tx_builder = MantleTxBuilder::new(MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        });

        // Determine gas cost without change note
        assert_eq!(
            754,
            tx_builder
                .clone()
                .add_ledger_input(Utxo::new(tx_hash(0), 0, Note::new(0, pk(0))))
                .gas_cost::<Gas>()
                .unwrap()
                .into_inner()
        );

        // We can fund the tx if the note value is exactly the gas cost without change
        // note
        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos(
                [Utxo::new(tx_hash(0), 0, Note::new(754, alice))],
                &ledger_config(),
            ),
        );

        let funded_tx_wo_change = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice])
            .unwrap()
            .build(); // successfully funded the tx

        // verify that no change output was used.
        if let Op::Transfer(transfer_op) =
            &funded_tx_wo_change.ops()[funded_tx_wo_change.ops().len() - 1]
        {
            assert_eq!(transfer_op.outputs, Outputs::new(vec![]));
        } else {
            panic!("last op must be a transfer")
        }

        // Determine gas cost with change note
        assert_eq!(
            794,
            tx_builder
                .clone()
                .add_ledger_input(Utxo::new(tx_hash(0), 0, Note::new(0, pk(0))))
                .with_dummy_change_note()
                .gas_cost::<Gas>()
                .unwrap()
                .into_inner()
        );

        for value in 755..=794 {
            // this region of note values will fail to fund the tx.
            // We can fund the tx if the note value is exactly the gas cost without change
            // note
            let wallet_state = WalletState::from_ledger(
                &HashMap::from_iter([(alice, 1)]),
                &LedgerState::from_utxos(
                    [Utxo::new(tx_hash(0), 0, Note::new(value, alice))],
                    &ledger_config(),
                ),
            );

            let fund_attempt = wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice]);

            assert_eq!(
                fund_attempt.unwrap_err(),
                WalletError::InsufficientFunds { available: value }
            );
        }

        // We can fund the tx if the note value exceeds gas cost with change note
        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos(
                [Utxo::new(tx_hash(0), 0, Note::new(795, alice))],
                &ledger_config(),
            ),
        );

        let funded_tx_wo_change = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice])
            .unwrap()
            .build(); // successfully funded the tx

        // verify that indeed a change output was used.
        if let Op::Transfer(transfer_op) =
            &funded_tx_wo_change.ops()[funded_tx_wo_change.ops().len() - 1]
        {
            assert_eq!(transfer_op.outputs, Outputs::new(vec![Note::new(1, alice)]));
        } else {
            panic!("the last operation must be a transfer")
        }
    }

    #[must_use]
    fn ledger_config() -> lb_ledger::Config {
        lb_ledger::Config {
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                NonZero::new(1).unwrap(),
                NonNegativeRatio::new(1, 10.try_into().unwrap()),
                1f64.try_into().expect("1 > 0"),
            ),
            sdp_config: lb_ledger::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            lock_period: 10,
                            inactivity_period: 20,
                            retention_period: 100,
                            timestamp: 0,
                            session_duration: 10,
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_session: NonZeroU64::new(10).unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
        }
    }

    fn assert_locked_notes<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        notes: impl IntoIterator<Item = NoteId>,
    ) {
        let wallet_state = wallet.wallet_state_at(tip).unwrap();
        assert_eq!(wallet_state.locked_notes, HashTrieSetSync::from_iter(notes));
    }

    fn assert_snapshotted_voucher<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        cm: &VoucherCm,
    ) {
        assert!(wallet.voucher_path(tip, cm).unwrap().is_some());
        assert!(wallet.voucher_path_snapshot(tip, cm).unwrap().is_some());
    }

    fn assert_tracked_but_not_snapshotted_voucher<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        cm: &VoucherCm,
    ) {
        assert!(wallet.voucher_path(tip, cm).unwrap().is_some());
        assert!(wallet.voucher_path_snapshot(tip, cm).unwrap().is_none());
    }

    fn assert_not_tracked_voucher<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        cm: &VoucherCm,
    ) {
        assert!(wallet.voucher_path(tip, cm).unwrap().is_none());
        assert!(wallet.voucher_path_snapshot(tip, cm).unwrap().is_none());
    }
}
