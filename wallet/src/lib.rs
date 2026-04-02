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
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, GasConstants, NoteId, Utxo, Value,
        ops::{
            leader_claim::{VoucherCm, VoucherNullifier},
            transfer::TransferOp,
        },
        tx_builder::MantleTxBuilder,
    },
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_ledger::LedgerState;

pub use crate::voucher::Vouchers;

pub struct WalletBlock {
    pub id: HeaderId,
    pub parent: HeaderId,
    pub transfers: Vec<TransferOp>,
}

impl<Tx: AuthenticatedMantleTx> From<Block<Tx>> for WalletBlock {
    fn from(block: Block<Tx>) -> Self {
        let transfers: Vec<TransferOp> = block
            .transactions()
            .flat_map(|auth_tx| auth_tx.mantle_tx().transfers())
            .collect();
        Self {
            id: block.header().id(),
            parent: block.header().parent(),
            transfers,
        }
    }
}

#[derive(Clone)]
pub struct WalletState {
    pub utxos: rpds::HashTrieMapSync<NoteId, Utxo>,
    pub pk_index: rpds::HashTrieMapSync<ZkPublicKey, rpds::HashTrieSetSync<NoteId>>,
}

impl WalletState {
    pub fn from_ledger<KeyId>(
        known_keys: &HashMap<ZkPublicKey, KeyId>,
        ledger: &LedgerState,
    ) -> Self {
        let mut utxos = rpds::HashTrieMapSync::new_sync();
        let mut pk_index = rpds::HashTrieMapSync::new_sync();

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
            }
        }

        Self { utxos, pk_index }
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
        // Get all UTXOs owned by the provided PKs, excluding any that are already being
        // used as inputs in the tx builder.
        let inputs = tx_builder.input_notes().collect::<HashSet<_>>();
        let mut utxos = self
            .utxos_owned_by_pks(pks)
            .into_iter()
            .filter(|utxo| !inputs.contains(&utxo.id()))
            .collect::<Vec<_>>();

        // Consume large valued notes first to ensure we converge.
        utxos.sort_by_key(|utxo| -i128::from(utxo.note.value));

        for i in 0..utxos.len() {
            let funded_tx_builder = tx_builder
                .clone()
                .extend_ledger_inputs(utxos[..=i].iter().copied());

            let funding_delta = funded_tx_builder.funding_delta::<G>();

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
                    if let Some(tx_with_change) = funded_tx_builder.return_change::<G>(change_pk) {
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
    pub fn balance(&self, pk: ZkPublicKey) -> Option<Value> {
        let balance = self
            .pk_index
            .get(&pk)?
            .iter()
            .map(|id| self.utxos[id].note.value)
            .sum();

        Some(balance)
    }

    #[must_use]
    pub fn apply_block<KeyId>(
        &self,
        known_keys: &HashMap<ZkPublicKey, KeyId>,
        block: &WalletBlock,
    ) -> Self {
        let mut utxos = self.utxos.clone();
        let mut pk_index = self.pk_index.clone();

        // Process each transaction in the block
        for transfer in &block.transfers {
            // Remove spent UTXOs (inputs)
            for spent_id in &transfer.inputs {
                if let Some(utxo) = utxos.get(spent_id) {
                    let pk = utxo.note.pk;
                    utxos = utxos.remove(spent_id);

                    if let Some(note_set) = pk_index.get(&pk) {
                        let updated_set = note_set.remove(spent_id);
                        if updated_set.is_empty() {
                            pk_index = pk_index.remove(&pk);
                        } else {
                            pk_index = pk_index.insert(pk, updated_set);
                        }
                    }
                }
            }

            // Add new UTXOs (outputs) - only if they belong to our known keys
            for utxo in transfer.utxos() {
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

        Self { utxos, pk_index }
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
    pub fn from_lib(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_vouchers: Vouchers<VoucherId>,
        lib: HeaderId,
        ledger: &LedgerState,
    ) -> Self {
        let known_keys = known_keys.into_iter().collect();
        let wallet_state = WalletState::from_ledger(&known_keys, ledger);

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

        let block_wallet_state = self
            .wallet_state_at(block.parent)?
            .apply_block(&self.known_keys, block);
        self.wallet_states.insert(block.id, block_wallet_state);
        Ok(())
    }

    pub fn balance(&self, tip: HeaderId, pk: ZkPublicKey) -> Result<Option<Value>, WalletError> {
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

#[cfg(test)]
mod tests {
    use std::{
        iter::empty,
        num::{NonZero, NonZeroU64},
        sync::Arc,
    };

    use lb_core::{
        crypto::{ZkDigest as _, ZkHasher},
        mantle::{
            Note, Op, TxHash,
            gas::MainnetGasConstants as Gas,
            ops::channel::{ChannelId, MsgId, inscribe::InscriptionOp},
        },
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_key_management_system_keys::keys::Ed25519Key;
    use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards};
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
    use num_bigint::BigUint;

    use super::*;

    fn pk(v: u64) -> ZkPublicKey {
        ZkPublicKey::from(BigUint::from(v))
    }

    fn tx_hash(v: u64) -> TxHash {
        TxHash::from(BigUint::from(v))
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

        let wallet = Wallet::<_, TestVoucherId>::from_lib(
            empty::<(ZkPublicKey, u64)>(),
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);
        assert!(wallet.vouchers().get(&voucher_cm).is_none());

        let wallet = Wallet::from_lib(
            [(alice, 1)],
            Vouchers::new([(voucher_cm, voucher_nf, (voucher_master_key, voucher_index))]),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), Some(104));
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);
        assert_eq!(
            wallet.vouchers().get(&voucher_cm),
            Some(&(voucher_master_key, voucher_index))
        );

        let wallet =
            Wallet::<_, TestVoucherId>::from_lib([(bob, 2)], Vouchers::default(), genesis, &ledger);
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), Some(20));

        let wallet = Wallet::<_, TestVoucherId>::from_lib(
            [(alice, 1), (bob, 2)],
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), Some(104));
        assert_eq!(wallet.balance(genesis, bob).unwrap(), Some(20));
    }

    #[test]
    fn test_sync() {
        let alice = pk(1);
        let bob = pk(2);

        let genesis = HeaderId::from([0; 32]);

        let genesis_ledger = LedgerState::from_utxos([], &ledger_config());

        let mut wallet = Wallet::<_, TestVoucherId>::from_lib(
            [(alice, 1), (bob, 2)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );

        // Block 1
        // - alice is minted 104 NMO in two notes (100 NMO and 4 NMO)
        let transfer1 = TransferOp {
            inputs: vec![],
            outputs: vec![Note::new(100, alice), Note::new(4, alice)],
        };

        let block_1 = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            transfers: vec![transfer1.clone()],
        };

        wallet.apply_block(&block_1).unwrap();

        // Block 2
        //  - alice spends 100 NMO utxo, sending 20 NMO to bob and 80 to herself
        let alice_100_nmo_utxo = transfer1.utxo_by_index(0).unwrap();

        let block_2 = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: block_1.id,
            transfers: vec![TransferOp {
                inputs: vec![alice_100_nmo_utxo.id()],
                outputs: vec![Note::new(20, bob), Note::new(80, alice)],
            }],
        };
        wallet.apply_block(&block_2).unwrap();

        // Query the balance of for each pk at different points in the blockchain
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);

        assert_eq!(wallet.balance(block_1.id, alice).unwrap(), Some(104));
        assert_eq!(wallet.balance(block_1.id, bob).unwrap(), None);

        assert_eq!(wallet.balance(block_2.id, alice).unwrap(), Some(84));
        assert_eq!(wallet.balance(block_2.id, bob).unwrap(), Some(20));
    }

    #[test]
    fn test_fund_tx_with_change() {
        let alice = pk(1);
        let alice_utxo = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));

        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos([alice_utxo], &ledger_config()),
        );

        let tx_builder = MantleTxBuilder::new()
            .set_execution_gas_price(1)
            .set_storage_gas_price(1);

        // Fund the transaction
        let funded_tx_builder = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice])
            .unwrap();

        assert_eq!(2925, funded_tx_builder.gas_cost::<Gas>());
        assert_eq!(2925, funded_tx_builder.net_balance());
        assert_eq!(0, funded_tx_builder.funding_delta::<Gas>());

        let funded_tx = funded_tx_builder.build();

        if let Op::Transfer(transfer_op) = &funded_tx.ops[funded_tx.ops.len() - 1] {
            // ensure alices utxo was used to pay the fee
            assert_eq!(transfer_op.inputs, vec![alice_utxo.id()]);
            // ensure change was returned to alice
            assert_eq!(
                transfer_op.outputs,
                vec![Note {
                    value: 2075,
                    pk: alice,
                }]
            );
        } else {
            panic!("last op must be a transfer")
        }
    }

    #[test]
    fn test_fund_tx_insufficient_funds() {
        let alice = pk(1);

        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos(
                [
                    Utxo::new(tx_hash(0), 0, Note::new(100, alice)),
                    Utxo::new(tx_hash(0), 1, Note::new(100, alice)),
                    Utxo::new(tx_hash(0), 2, Note::new(100, alice)),
                    Utxo::new(tx_hash(0), 3, Note::new(100, alice)),
                ],
                &ledger_config(),
            ),
        );

        let mut tx_builder = MantleTxBuilder::new()
            .set_execution_gas_price(1)
            .set_storage_gas_price(1);

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

        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos([], &ledger_config()),
        );

        let tx_builder = MantleTxBuilder::new()
            .set_execution_gas_price(1)
            .set_storage_gas_price(1);

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

        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1), (bob, 2)]),
            &LedgerState::from_utxos(
                [Utxo::new(tx_hash(0), 0, Note::new(1_000_000, bob))],
                &ledger_config(),
            ),
        );

        let tx_builder = MantleTxBuilder::new()
            .set_execution_gas_price(1)
            .set_storage_gas_price(1);

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

        let tx_builder = MantleTxBuilder::new()
            .set_execution_gas_price(1)
            .set_storage_gas_price(1);

        // Determine gas cost without change note
        assert_eq!(
            2885,
            tx_builder
                .clone()
                .add_ledger_input(Utxo::new(tx_hash(0), 0, Note::new(0, pk(0))))
                .gas_cost::<Gas>()
        );

        // We can fund the tx if the note value is exactly the gas cost without change
        // note
        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos(
                [Utxo::new(tx_hash(0), 0, Note::new(2885, alice))],
                &ledger_config(),
            ),
        );

        let funded_tx_wo_change = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice])
            .unwrap()
            .build(); // successfully funded the tx

        // verify that no change output was used.
        if let Op::Transfer(transfer_op) =
            &funded_tx_wo_change.ops[funded_tx_wo_change.ops.len() - 1]
        {
            assert_eq!(transfer_op.outputs, vec![]);
        } else {
            panic!("last op must be a transfer")
        }

        // Determine gas cost with change note
        assert_eq!(
            2925,
            tx_builder
                .clone()
                .add_ledger_input(Utxo::new(tx_hash(0), 0, Note::new(0, pk(0))))
                .with_dummy_change_note()
                .gas_cost::<Gas>()
        );

        for value in 2886..=2925 {
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
                [Utxo::new(tx_hash(0), 0, Note::new(2926, alice))],
                &ledger_config(),
            ),
        );

        let funded_tx_wo_change = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice])
            .unwrap()
            .build(); // successfully funded the tx

        // verify that indeed a change output was used.
        if let Op::Transfer(transfer_op) =
            &funded_tx_wo_change.ops[funded_tx_wo_change.ops.len() - 1]
        {
            assert_eq!(transfer_op.outputs, vec![Note::new(1, alice)]);
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
}
