mod config;
// The ledger is split into two modules:
// - `cryptarchia`: the base functionalities needed by the Cryptarchia consensus
//   algorithm, including a minimal UTxO model.
// - `mantle_ops` : our extensions in the form of Mantle operations, e.g. SDP.
pub mod cryptarchia;
pub mod mantle;

use std::{cmp::Ordering, collections::HashMap, hash::Hash};

pub use config::Config;
use cryptarchia::LedgerState as CryptarchiaLedger;
pub use cryptarchia::{EpochState, UtxoTree};
use lb_core::{
    block::BlockNumber,
    mantle::{
        AuthenticatedMantleTx, GenesisTx, NoteId, Op, OpProof, Utxo, Value,
        gas::{Gas, GasConstants},
    },
    proofs::leader_proof,
    sdp::{Declaration, DeclarationId, ProviderId, ProviderInfo, ServiceType, SessionNumber},
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Field as _, Fr};
use mantle::LedgerState as MantleLedger;
use thiserror::Error;

const WINDOW_SIZE: usize = 120;

/// Denominator of 1/(`I_max` * `D1_target` * `Delta_t` * `T`)
/// That correspond to `BLOCK_PER_YEAR` / (`MAX_INFLATION` * `KPI_FEE_TARGET` *
/// `WINDOW_SIZE`)
const A_SCALE: u128 = 120_000_000;

/// Numerator of 1/(`I_max` * `D1_target` * `Delta_t` * `T`)
/// That correspond to `BLOCK_PER_YEAR` / (`MAX_INFLATION` * `KPI_FEE_TARGET` *
/// `WINDOW_SIZE`)
const FEE_AVG_NUM: u128 = 10_512;

/// Numerator of `I_max` * `S_TGE` * `DELTA_t` / `f`
/// It corresponds to `MAX_INFLATION` * `TOKEN_GENESIS` * `BLOCK_PER_BLOCK` /
/// `BLOCK_PER_YEAR`
const INFLATION_NUMERATOR: u128 = 62_500;

/// Numerator of `I_max` * `S_TGE` * `DELTA_t` / `f`
/// It corresponds to `MAX_INFLATION` * `TOKEN_GENESIS` * `BLOCK_PER_BLOCK` /
/// `BLOCK_PER_YEAR`
const INFLATION_DENOMINATOR: u128 = 657;

const STAKE_TARGET: u128 = 3_000_000_000;

// That correspond to 40% of the block rewards for leaders
const LEADER_REWARD_SHARE_NUMERATOR: u128 = 4;

const LEADER_REWARD_SHARE_DENOMINATOR: u128 = 10;

// That correspond to 60% of the block rewards for blend nodes

const BLEND_REWARD_SHARE_NUMERATOR: u128 = 6;

const BLEND_REWARD_SHARE_DENOMINATOR: u128 = 10;
const EXECUTION_GAS_LIMIT: Gas = 3_193_360;

// While individual notes are constrained to be `u64`, intermediate calculations
// may overflow, so we use `i128` to avoid that and to easily represent negative
// balances which may arise in special circumstances (e.g. rewards calculation).
pub type Balance = i128;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LedgerError<Id> {
    #[error("Invalid block slot {block:?} for parent slot {parent:?}")]
    InvalidSlot { parent: Slot, block: Slot },
    #[error("Parent block not found: {0:?}")]
    ParentNotFound(Id),
    #[error("Invalid leader proof")]
    InvalidProof,
    #[error("Invalid note: {0:?}")]
    InvalidNote(NoteId),
    #[error("Insufficient balance")]
    InsufficientBalance,
    #[error("Unbalanced transaction, balance does not match fees")]
    UnbalancedTransaction,
    #[error("Overflow while calculating balance")]
    Overflow,
    #[error("Zero value note")]
    ZeroValueNote,
    #[error("Mantle error: {0}")]
    Mantle(#[from] mantle::Error),
    #[error("Locked note: {0:?}")]
    LockedNote(NoteId),
    #[error("Input note in genesis block: {0:?}")]
    InputInGenesis(NoteId),
    #[error("The first Transfer Operation is missing in genesis tx")]
    MissingTransferGenesis(),
    #[error("Unsupported operation")]
    UnsupportedOp,
    #[error("Fees don't cover the minimal execution base fee cost")]
    InsufficientExecutionFee,
    #[error("The execution gas of the block ({gas:?}) exceeds the maximum limit ({limit:?}")]
    TooMuchExecutionGas { gas: Gas, limit: Gas },
    #[error("Storage fees aren't equal to the storage fee of the current epoch")]
    InvalidStoragePrice,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ledger<Id: Eq + Hash> {
    states: HashMap<Id, LedgerState>,
    config: Config,
}

impl<Id> Ledger<Id>
where
    Id: Eq + Hash + Copy,
{
    pub fn new(id: Id, state: LedgerState, config: Config) -> Self {
        Self {
            states: std::iter::once((id, state)).collect(),
            config,
        }
    }

    /// Prepare adding a new [`LedgerState`] by applying the given proof and
    /// transactions on top of the parent state.
    ///
    /// On success, a new [`LedgerState`] is returned, which can then be
    /// committed by calling [`Self::commit_update`].
    pub fn prepare_update<LeaderProof, Constants>(
        &self,
        id: Id,
        parent_id: Id,
        slot: Slot,
        proof: &LeaderProof,
        txs: impl Iterator<Item = impl AuthenticatedMantleTx>,
    ) -> Result<(Id, LedgerState), LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
        Constants: GasConstants,
    {
        let parent_state = self
            .states
            .get(&parent_id)
            .ok_or(LedgerError::ParentNotFound(parent_id))?;

        let new_state =
            parent_state
                .clone()
                .try_update::<_, _, Constants>(slot, proof, txs, &self.config)?;

        Ok((id, new_state))
    }

    /// Commits a new [`LedgerState`] created by [`Self::prepare_update`].
    pub fn commit_update(&mut self, id: Id, state: LedgerState) {
        self.states.insert(id, state);
    }

    pub fn state(&self, id: &Id) -> Option<&LedgerState> {
        self.states.get(id)
    }

    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// Removes the state stored for the given block id.
    ///
    /// This function must be called only when the states being pruned won't be
    /// needed for any subsequent proof.
    ///
    /// ## Arguments
    ///
    /// The block ID to prune the state for.
    ///
    /// ## Returns
    ///
    /// `true` if the state was successfully removed, `false` otherwise.
    pub fn prune_state_at(&mut self, block: &Id) -> bool {
        self.states.remove(block).is_some()
    }

    /// Shrinks the map of ledger states to free up memory that has been pruned
    /// so far.
    ///
    /// This shouldn't be called frequently since the entire map is
    /// reconstructed.
    pub fn shrink(&mut self) {
        self.states.shrink_to_fit();
    }
}

/// A ledger state
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerState {
    block_number: BlockNumber,
    cryptarchia_ledger: CryptarchiaLedger,
    mantle_ledger: MantleLedger,
}

impl LedgerState {
    fn try_update<LeaderProof, Id, Constants>(
        self,
        slot: Slot,
        proof: &LeaderProof,
        txs: impl Iterator<Item = impl AuthenticatedMantleTx>,
        config: &Config,
    ) -> Result<Self, LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
        Constants: GasConstants,
    {
        self.try_apply_header(slot, proof, config)?
            .try_apply_contents::<_, Constants>(config, txs)
    }

    /// Apply header-related changed to the ledger state. These include
    /// leadership and in general any changes that not related to
    /// transactions that should be applied before that.
    pub fn try_apply_header<LeaderProof, Id>(
        self,
        slot: Slot,
        proof: &LeaderProof,
        config: &Config,
    ) -> Result<Self, LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        let mut cryptarchia_ledger = self
            .cryptarchia_ledger
            .try_apply_header::<LeaderProof, Id>(slot, proof, config)?;
        let (mantle_ledger, reward_utxos) = self.mantle_ledger.try_apply_header(
            cryptarchia_ledger.epoch_state(),
            *proof.voucher_cm(),
            config,
        )?;

        // Insert reward UTXOs into the cryptarchia ledger
        for utxo in reward_utxos {
            cryptarchia_ledger.utxos = cryptarchia_ledger.utxos.insert(utxo.id(), utxo).0;
        }

        Ok(Self {
            block_number: self
                .block_number
                .checked_add(1)
                .expect("Logos blockchain lived long and prospered"),
            cryptarchia_ledger,
            mantle_ledger,
        })
    }

    /// total estimated stake and on the average of fees consumed per block over
    /// the last `BLOCK_REWARD_WINDOW_SIZE` blocks. See the block rewards
    /// specification: <https://www.notion.so/nomos-tech/v1-1-Block-Rewards-Specification-326261aa09df80579edddaf092057b3d>
    fn compute_block_rewards(mut self, total_fee_burned: Gas, total_fee_tip: Gas) -> Self {
        let window_index = self.block_number as usize % WINDOW_SIZE;

        // First update the fee burned in the block
        self.cryptarchia_ledger
            .update_fee_window(window_index, total_fee_burned);

        // Then compute the amount of the block rewards

        // compute A_t'
        let sum_fees = self.cryptarchia_ledger.get_summed_fees();
        let a_numerator = STAKE_TARGET
            .saturating_add(FEE_AVG_NUM.saturating_mul(sum_fees))
            .saturating_sub(u128::from(self.cryptarchia_ledger.epoch_state.total_stake))
            .min(A_SCALE);

        let reward_numerator = INFLATION_NUMERATOR * a_numerator
            + INFLATION_DENOMINATOR
                * (A_SCALE - a_numerator)
                * u128::from(self.cryptarchia_ledger.get_fee_from_index(window_index));
        let reward_denominator = INFLATION_DENOMINATOR * A_SCALE;

        // blend get 60% of block rewards while leaders get the 40% remaining + the
        // tips. Casting as Value truncate the floating points
        let blend_reward = (reward_numerator * BLEND_REWARD_SHARE_NUMERATOR
            / (reward_denominator * BLEND_REWARD_SHARE_DENOMINATOR))
            as Value;
        let leader_reward = (reward_numerator * LEADER_REWARD_SHARE_NUMERATOR
            / (reward_denominator * LEADER_REWARD_SHARE_DENOMINATOR))
            as Value
            + total_fee_tip;

        self.mantle_ledger.leaders = self
            .mantle_ledger
            .leaders
            .add_pending_rewards(leader_reward);

        self.mantle_ledger.sdp.add_blend_income(blend_reward);

        self
    }

    /// For each block received, execution base fees and average execution
    /// consumption are updated based on the total execution gas consumed in the
    /// block and the smoothed average consumption. This function update the
    /// `average_execution_gas` and the `execution_base_fee` stored in the
    /// cryptarchia ledger. See the specification <https://www.notion.so/nomos-tech/v1-2-Execution-Market-Specification-326261aa09df8022b1cfcfe968bdb5e1>
    fn update_execution_market(self, block_execution_gas_consumed: Gas) -> Self {
        Self {
            cryptarchia_ledger: self
                .cryptarchia_ledger
                .update_execution_market(block_execution_gas_consumed),
            ..self
        }
    }

    /// Apply the contents of an update to the ledger state.
    pub fn try_apply_contents<Id, Constants: GasConstants>(
        mut self,
        config: &Config,
        txs: impl Iterator<Item = impl AuthenticatedMantleTx>,
    ) -> Result<Self, LedgerError<Id>> {
        let mut total_block_execution_gas: Gas = 0;
        let mut total_fee_burned: Gas = 0;
        let mut total_fee_tip: Gas = 0;
        for tx in txs {
            let balance;
            (self, balance) = self.try_apply_tx::<_, Constants>(config, &tx)?;

            // Check the transaction is balanced
            match balance.cmp(&tx.total_gas_cost::<Constants>().into()) {
                Ordering::Less => return Err(LedgerError::InsufficientBalance),
                Ordering::Greater => return Err(LedgerError::UnbalancedTransaction),
                Ordering::Equal => {} // OK!
            }

            // Update the total of fee burned and tipped in the block
            let tx_fee_burned = tx.execution_gas_consumption::<Constants>()
                * self.cryptarchia_ledger.execution_base_fee()
                + tx.storage_gas_cost();

            // Check that the transaction at least pays for the base fee
            if balance < tx_fee_burned.into() {
                return Err(LedgerError::InsufficientExecutionFee);
            }

            // Check that the transaction pays the correct storage fees
            // TODO: remove the storage price from the Mantle Transaction and wallet should
            // pull the price from ledger to get the fees to pay
            if tx.mantle_tx().storage_gas_price != *self.cryptarchia_ledger.storage_gas_price() {
                return Err(LedgerError::InvalidStoragePrice);
            }
            let tx_fee_tip = balance as Gas - tx_fee_burned;
            total_fee_burned += tx_fee_burned;
            total_fee_tip += tx_fee_tip;
            total_block_execution_gas += &tx.execution_gas_consumption::<Constants>();

            // Check that the block is not exceeding the Gas limit
            if total_block_execution_gas > EXECUTION_GAS_LIMIT {
                return Err(LedgerError::TooMuchExecutionGas {
                    gas: total_block_execution_gas,
                    limit: EXECUTION_GAS_LIMIT,
                });
            }
        }
        // Compute Block rewards and give tips
        self = self.compute_block_rewards(total_fee_burned, total_fee_tip);
        // Update Execution market state
        self = self.update_execution_market(total_block_execution_gas);
        Ok(self)
    }

    pub fn from_utxos(utxos: impl IntoIterator<Item = Utxo>, config: &Config) -> Self {
        let cryptarchia_ledger = CryptarchiaLedger::from_utxos(utxos, config, Fr::ZERO);
        let mantle_ledger = MantleLedger::new(config, cryptarchia_ledger.epoch_state());
        Self {
            block_number: 0,
            cryptarchia_ledger,
            mantle_ledger,
        }
    }

    pub fn from_genesis_tx<Id>(
        tx: impl GenesisTx,
        config: &Config,
        epoch_nonce: Fr,
    ) -> Result<Self, LedgerError<Id>> {
        let cryptarchia_ledger = CryptarchiaLedger::from_genesis_tx(&tx, config, epoch_nonce)?;
        let mantle_ledger = MantleLedger::from_genesis_tx(
            tx,
            config,
            cryptarchia_ledger.latest_utxos(),
            cryptarchia_ledger.epoch_state(),
        )?;
        Ok(Self {
            block_number: 0,
            cryptarchia_ledger,
            mantle_ledger,
        })
    }

    #[must_use]
    pub const fn slot(&self) -> Slot {
        self.cryptarchia_ledger.slot()
    }

    #[must_use]
    pub const fn epoch_state(&self) -> &EpochState {
        self.cryptarchia_ledger.epoch_state()
    }

    #[must_use]
    pub const fn next_epoch_state(&self) -> &EpochState {
        self.cryptarchia_ledger.next_epoch_state()
    }

    /// Computes the epoch state for a given slot.
    ///
    /// This handles the case where epochs have been skipped (no blocks
    /// produced).
    ///
    /// Returns [`LedgerError::InvalidSlot`] if the slot is in the past before
    /// the current ledger state.
    pub fn epoch_state_for_slot<Id>(
        &self,
        slot: Slot,
        config: &Config,
    ) -> Result<EpochState, LedgerError<Id>> {
        self.cryptarchia_ledger.epoch_state_for_slot(slot, config)
    }

    #[must_use]
    pub const fn latest_utxos(&self) -> &UtxoTree {
        self.cryptarchia_ledger.latest_utxos()
    }

    #[must_use]
    pub const fn aged_utxos(&self) -> &UtxoTree {
        self.cryptarchia_ledger.aged_utxos()
    }

    #[must_use]
    pub const fn mantle_ledger(&self) -> &MantleLedger {
        &self.mantle_ledger
    }

    #[must_use]
    pub fn sdp_declarations(&self) -> Vec<(DeclarationId, Declaration)> {
        self.mantle_ledger.sdp_declarations()
    }

    #[must_use]
    pub fn active_session_providers(
        &self,
        service_type: ServiceType,
    ) -> Option<HashMap<ProviderId, ProviderInfo>> {
        self.mantle_ledger.active_session_providers(service_type)
    }

    #[must_use]
    pub fn active_sessions(&self) -> HashMap<ServiceType, SessionNumber> {
        self.mantle_ledger.active_sessions()
    }

    fn try_apply_tx<Id, Constants: GasConstants>(
        mut self,
        config: &Config,
        tx: impl AuthenticatedMantleTx,
    ) -> Result<(Self, Balance), LedgerError<Id>> {
        let mut balance: Balance = 0;
        let tx_hash = tx.hash();
        let ops = tx.ops_with_proof().map(|(op, proof)| (op, Some(proof)));
        for (op, proof) in ops {
            match (op, proof) {
                // The signature for channel ops can be verified before reaching this point,
                // as you only need the signer's public key and tx hash
                // Callers are expected to validate the proof before calling this function.
                (Op::ChannelInscribe(op), _) => {
                    self.mantle_ledger = self.mantle_ledger.try_apply_channel_inscription(op)?;
                }
                (Op::ChannelSetKeys(op), Some(OpProof::Ed25519Sig(sig))) => {
                    self.mantle_ledger = self
                        .mantle_ledger
                        .try_apply_channel_set_keys(op, sig, &tx_hash)?;
                }
                (
                    Op::SDPDeclare(op),
                    Some(OpProof::ZkAndEd25519Sigs {
                        zk_sig,
                        ed25519_sig,
                    }),
                ) => {
                    self.mantle_ledger = self.mantle_ledger.try_apply_sdp_declaration(
                        op,
                        zk_sig,
                        ed25519_sig,
                        self.cryptarchia_ledger.latest_utxos(),
                        tx_hash,
                        config,
                    )?;
                }
                (Op::SDPActive(op), Some(OpProof::ZkSig(sig))) => {
                    self.mantle_ledger = self
                        .mantle_ledger
                        .try_apply_sdp_active(op, sig, tx_hash, config)?;
                }
                (Op::SDPWithdraw(op), Some(OpProof::ZkSig(sig))) => {
                    self.mantle_ledger = self
                        .mantle_ledger
                        .try_apply_sdp_withdraw(op, sig, tx_hash, config)?;
                }
                (Op::LeaderClaim(op), None) => {
                    // Correct derivation of the voucher nullifier and membership in the merkle tree
                    // can be verified outside of this function since public inputs are already
                    // available. Callers are expected to validate the proof
                    // before calling this function.
                    let leader_balance;
                    (self.mantle_ledger, leader_balance) =
                        self.mantle_ledger.try_apply_leader_claim(op)?;
                    balance += leader_balance;
                }
                (Op::Transfer(op), Some(OpProof::ZkSig(sig))) => {
                    let transfer_balance;
                    (self.cryptarchia_ledger, transfer_balance) =
                        self.cryptarchia_ledger.try_apply_transfer::<_, Constants>(
                            self.mantle_ledger.locked_notes(),
                            op,
                            sig,
                            tx_hash,
                        )?;
                    balance += transfer_balance;
                }
                _ => {
                    return Err(LedgerError::UnsupportedOp);
                }
            }
        }
        Ok((self, balance))
    }
}

#[cfg(test)]
mod tests {
    use cryptarchia::tests::{config, generate_proof, utxo};
    use lb_core::mantle::{
        GasCost as _, MantleTx, Note,
        OpProof::ZkSig,
        SignedMantleTx, Transaction as _,
        gas::MainnetGasConstants,
        ops::{
            channel::{ChannelId, MsgId, inscribe::InscriptionOp, set_keys::SetKeysOp},
            transfer::TransferOp,
        },
    };
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey, ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    type HeaderId = [u8; 32];

    fn create_tx(
        inputs: Vec<NoteId>,
        outputs: Vec<Note>,
        sks: &[ZkKey],
        execution_price: Gas,
        storage_price: Gas,
    ) -> SignedMantleTx {
        let transfer_op = TransferOp::new(inputs, outputs);
        let mantle_tx = MantleTx {
            ops: vec![Op::Transfer(transfer_op)],
            execution_gas_price: execution_price,
            storage_gas_price: storage_price,
        };
        SignedMantleTx {
            ops_proofs: vec![ZkSig(
                ZkKey::multi_sign(sks, mantle_tx.hash().as_ref()).unwrap(),
            )],
            mantle_tx,
        }
    }

    pub fn create_test_ledger() -> (Ledger<HeaderId>, HeaderId, Utxo) {
        let config = config();
        let utxo = utxo();
        let genesis_state = LedgerState::from_utxos([utxo], &config);
        let ledger = Ledger::new([0; 32], genesis_state, config);
        (ledger, [0; 32], utxo)
    }

    fn create_test_keys_with_seed(seed: u8) -> (Ed25519Key, Ed25519PublicKey) {
        let signing_key = Ed25519Key::from_bytes(&[seed; 32]);
        let verifying_key = signing_key.public_key();
        (signing_key, verifying_key)
    }

    fn create_signed_tx(op: Op, signing_key: &Ed25519Key) -> SignedMantleTx {
        create_multi_signed_tx(vec![op], vec![signing_key])
    }

    fn create_multi_signed_tx(ops: Vec<Op>, signing_keys: Vec<&Ed25519Key>) -> SignedMantleTx {
        let mantle_tx = MantleTx {
            ops: ops.clone(),
            execution_gas_price: 0,
            storage_gas_price: 0,
        };

        let tx_hash = mantle_tx.hash();
        let ops_proofs = signing_keys
            .into_iter()
            .zip(ops)
            .map(|(key, _)| {
                OpProof::Ed25519Sig(key.sign_payload(tx_hash.as_signing_bytes().as_ref()))
            })
            .collect();

        SignedMantleTx::new(mantle_tx, ops_proofs)
            .expect("Test transaction should have valid signatures")
    }

    #[test]
    fn test_ledger_creation() {
        let (ledger, genesis_id, utxo) = create_test_ledger();

        let state = ledger.state(&genesis_id).unwrap();
        assert!(state.latest_utxos().contains(&utxo.id()));
        assert_eq!(state.slot(), 0.into());
    }

    #[test]
    fn test_ledger_try_update_with_transaction() {
        let (mut ledger, genesis_id, utxo) = create_test_ledger();
        let mut output_note = Note::new(1, ZkPublicKey::new(BigUint::from(1u8).into()));
        let sk = ZkKey::from(BigUint::from(0u8));
        // determine fees
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            1,
            1,
        );
        let fees = tx.total_gas_cost::<MainnetGasConstants>();
        output_note.value = utxo.note.value - fees;
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk], 1, 1);

        // Create a dummy proof (using same structure as in cryptarchia tests)

        let proof = generate_proof(
            &ledger.state(&genesis_id).unwrap().cryptarchia_ledger,
            &utxo,
            Slot::from(1u64),
        );

        let new_id = [1; 32];
        let (_, state) = ledger
            .prepare_update::<_, MainnetGasConstants>(
                new_id,
                genesis_id,
                Slot::from(1u64),
                &proof,
                std::iter::once(&tx),
            )
            .unwrap();
        ledger.commit_update(new_id, state);

        // Verify the transaction was applied
        let new_state = ledger.state(&new_id).unwrap();
        assert!(!new_state.latest_utxos().contains(&utxo.id()));

        // Verify output was created
        if let Op::Transfer(transfer_op) = &tx.mantle_tx.ops[0] {
            let output_utxo = transfer_op.utxo_by_index(0).unwrap();
            assert!(new_state.latest_utxos().contains(&output_utxo.id()));
        } else {
            panic!("first op must be a transfer")
        }
    }

    #[test]
    fn test_channel_inscribe_operation() {
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([2; 32]);

        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: vec![1, 2, 3, 4],
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let tx = create_signed_tx(Op::ChannelInscribe(inscribe_op), &signing_key);
        let result = state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _) = result.unwrap();
        assert!(
            new_state
                .mantle_ledger
                .channels()
                .channels
                .contains_key(&channel_id)
        );
    }

    #[test]
    fn test_channel_set_keys_operation() {
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([3; 32]);

        let set_keys_op = SetKeysOp {
            channel: channel_id,
            keys: vec![verifying_key],
        };

        let tx = create_signed_tx(Op::ChannelSetKeys(set_keys_op), &signing_key);
        let result = state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _) = result.unwrap();
        assert!(
            new_state
                .mantle_ledger
                .channels()
                .channels
                .contains_key(&channel_id)
        );
        assert_eq!(
            new_state
                .mantle_ledger
                .channels()
                .channels
                .get(&channel_id)
                .unwrap()
                .keys,
            vec![verifying_key].into()
        );
    }

    #[test]
    fn test_invalid_parent_error() {
        let test_config = config();
        let mut state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([5; 32]);

        // First, create a channel with one message
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: vec![1, 2, 3],
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let first_tx = create_signed_tx(Op::ChannelInscribe(first_inscribe), &signing_key);
        state = state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, first_tx)
            .unwrap()
            .0;

        // Now try to add a message with wrong parent
        let wrong_parent = MsgId::from([99; 32]);
        let second_inscribe = InscriptionOp {
            channel_id,
            inscription: vec![4, 5, 6],
            parent: wrong_parent,
            signer: verifying_key,
        };

        let second_tx = create_signed_tx(Op::ChannelInscribe(second_inscribe), &signing_key);
        let result = state
            .clone()
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, second_tx);
        assert!(matches!(
            result,
            Err(LedgerError::Mantle(mantle::Error::Channel(
                mantle::channel::Error::InvalidParent { .. }
            )))
        ));

        // Writing into an empty channel with a parent != MsgId::root() should also fail
        let empty_channel_id = ChannelId::from([8; 32]);
        let empty_inscribe = InscriptionOp {
            channel_id: empty_channel_id,
            inscription: vec![7, 8, 9],
            parent: MsgId::from([1; 32]), // non-root parent
            signer: verifying_key,
        };

        let empty_tx = create_signed_tx(Op::ChannelInscribe(empty_inscribe), &signing_key);
        let empty_result =
            state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, empty_tx);
        assert!(matches!(
            empty_result,
            Err(LedgerError::Mantle(mantle::Error::Channel(
                mantle::channel::Error::InvalidParent { .. }
            )))
        ));
    }

    #[test]
    fn test_unauthorized_signer_error() {
        let test_config = config();
        let mut state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let (unauthorized_signing_key, unauthorized_verifying_key) = create_test_keys_with_seed(3);
        let channel_id = ChannelId::from([6; 32]);

        // First, create a channel with authorized signer
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: vec![1, 2, 3],
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let correct_parent = first_inscribe.id();
        let first_tx = create_signed_tx(Op::ChannelInscribe(first_inscribe), &signing_key);
        state = state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, first_tx)
            .unwrap()
            .0;

        // Now try to add a message with unauthorized signer
        let second_inscribe = InscriptionOp {
            channel_id,
            inscription: vec![4, 5, 6],
            parent: correct_parent,
            signer: unauthorized_verifying_key,
        };

        let second_tx = create_signed_tx(
            Op::ChannelInscribe(second_inscribe),
            &unauthorized_signing_key,
        );
        let result = state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, second_tx);
        assert!(matches!(
            result,
            Err(LedgerError::Mantle(mantle::Error::Channel(
                mantle::channel::Error::UnauthorizedSigner { .. }
            )))
        ));
    }

    #[test]
    fn test_empty_keys_error() {
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, _) = create_test_keys();
        let channel_id = ChannelId::from([7; 32]);

        let set_keys_op = SetKeysOp {
            channel: channel_id,
            keys: vec![],
        };

        let tx = create_signed_tx(Op::ChannelSetKeys(set_keys_op), &signing_key);
        let result = state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx);
        assert_eq!(
            result,
            Err(LedgerError::Mantle(mantle::Error::Channel(
                mantle::channel::Error::EmptyKeys { channel_id }
            )))
        );
    }

    #[test]
    fn test_multiple_operations_in_transaction() {
        // Create channel 1 by posting an inscription
        // Create channel 2 by posting an inscription
        // Change the keys for channel 1
        // Post another inscription in channel 1
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (sk1, vk1) = create_test_keys_with_seed(1);
        let (sk2, vk2) = create_test_keys_with_seed(2);
        let (_, vk3) = create_test_keys_with_seed(3);
        let (sk4, vk4) = create_test_keys_with_seed(4);

        let channel1 = ChannelId::from([10; 32]);
        let channel2 = ChannelId::from([20; 32]);

        let inscribe_op1 = InscriptionOp {
            channel_id: channel1,
            inscription: vec![1, 2, 3],
            parent: MsgId::root(),
            signer: vk1,
        };

        let inscribe_op2 = InscriptionOp {
            channel_id: channel2,
            inscription: vec![4, 5, 6],
            parent: MsgId::root(),
            signer: vk2,
        };

        let set_keys_op = SetKeysOp {
            channel: channel1,
            keys: vec![vk3, vk4],
        };

        let inscribe_op3 = InscriptionOp {
            channel_id: channel1,
            inscription: vec![7, 8, 9],
            parent: inscribe_op1.id(),
            signer: vk4,
        };

        let ops = vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
            Op::ChannelSetKeys(set_keys_op),
            Op::ChannelInscribe(inscribe_op3.clone()),
        ];
        let tx = create_multi_signed_tx(ops, vec![&sk1, &sk2, &sk1, &sk4]);

        let result = state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx)
            .unwrap()
            .0;

        assert!(
            result
                .mantle_ledger
                .channels()
                .channels
                .contains_key(&channel1)
        );
        assert!(
            result
                .mantle_ledger
                .channels()
                .channels
                .contains_key(&channel2)
        );
        assert_eq!(
            result
                .mantle_ledger
                .channels()
                .channels
                .get(&channel1)
                .unwrap()
                .tip,
            inscribe_op3.id()
        );
    }

    // TODO: Update this test to work with the new SDP API
    // This test needs to be rewritten to use the new SDP ledger API which no longer
    // exposes get_declaration() or uses declaration_id() methods.
    // #[test]
    // #[expect(clippy::, reason = "Test function.")]
    #[test]
    fn _test_sdp_withdraw_operation() {
        // This test has been disabled pending API updates
    }

    #[test]
    fn test_storage_price_rejection() {
        let utxo = utxo();
        let config = config();
        let ledger = LedgerState::from_utxos([utxo], &config);

        let mut output_note = Note::new(1, ZkPublicKey::new(BigUint::from(1u8).into()));
        let sk = ZkKey::from(BigUint::from(0u8));
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            1,
            0,
        );
        let fees = tx.total_gas_cost::<MainnetGasConstants>();
        output_note.value = utxo.note.value - fees;
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk], 1, 0);

        let result = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        assert_eq!(result, Err(LedgerError::InvalidStoragePrice));
    }

    #[test]
    fn test_base_fee_rejection() {
        let utxo = utxo();
        let config = config();
        let mut ledger = LedgerState::from_utxos([utxo], &config);

        let mut output_note = Note::new(1, ZkPublicKey::new(BigUint::from(0u8).into()));
        let sk = ZkKey::from(BigUint::from(0u8));
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            1,
            1,
        );
        // Pays 2925 fees = 2705 execution base fee + 0 execution tip + 220 storage
        let fees = tx.total_gas_cost::<MainnetGasConstants>();
        output_note.value = utxo.note.value - fees;
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk], 1, 1);

        let result = ledger
            .clone()
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The unwrap should succeed because the user pays at least the base fee of 2705
        result.unwrap();

        ledger.cryptarchia_ledger = ledger.cryptarchia_ledger.set_execution_base_fee(10);

        let result = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The transaction should be rejected because the price indicated for execution
        // doesn't cover the base fee that cost 27 050
        assert_eq!(result, Err(LedgerError::InsufficientExecutionFee));
    }

    #[test]
    fn test_priority_fees_go_to_leader() {
        let utxo = utxo();
        let config = config();
        let ledger = LedgerState::from_utxos([utxo], &config);

        let mut output_note = Note::new(1, ZkPublicKey::new(BigUint::from(0u8).into()));
        let sk = ZkKey::from(BigUint::from(0u8));
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            1,
            1,
        );
        // The tx ays 2925 fees = 2705 execution base fee + 0 execution tip + 220
        // storage
        let fees = tx.total_gas_cost::<MainnetGasConstants>();
        output_note.value = utxo.note.value - fees;
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            1,
            1,
        );

        let result = ledger
            .clone()
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The unwrap should succeed because the user pays at least the base fee of 2705
        let no_priority_fee_ledger = result.unwrap();

        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            2,
            1,
        );
        // The tx ays 5630 fees = 2705 execution base fee + 2705 execution tip + 220
        // storage
        let fees = tx.total_gas_cost::<MainnetGasConstants>();
        output_note.value = utxo.note.value - fees;
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk], 2, 1);
        let result = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The unwrap should succeed because the user pays at least the base fee of 2705
        let priority_fee_ledger = result.unwrap();

        assert_eq!(
            no_priority_fee_ledger
                .mantle_ledger
                .leaders
                .get_pending_rewards()
                + 2705,
            priority_fee_ledger
                .mantle_ledger
                .leaders
                .get_pending_rewards()
        );
    }
}
