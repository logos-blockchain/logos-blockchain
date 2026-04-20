mod config;
// The ledger is split into two modules:
// - `cryptarchia`: the base functionalities needed by the Cryptarchia consensus
//   algorithm, including a minimal UTxO model.
// - `mantle_ops`: our extensions in the form of Mantle operations, e.g. SDP.
pub mod cryptarchia;
pub mod mantle;

use std::{cmp::Ordering, collections::HashMap, hash::Hash};

pub use config::Config;
use cryptarchia::LedgerState as CryptarchiaLedger;
pub use cryptarchia::{EpochState, UtxoTree};
use lb_core::{
    block::BlockNumber,
    mantle::{
        AuthenticatedMantleTx, GenesisTx, NoteId, Op, OpProof, Utxo, Value, VerificationError,
        gas::{Gas, GasConstants, GasCost, GasOverflow},
        tx::MantleTxContext,
    },
    proofs::leader_proof,
    sdp::{Declaration, DeclarationId, ProviderId, ProviderInfo, ServiceType, SessionNumber},
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Field as _, Fr};
use mantle::LedgerState as MantleLedger;
use thiserror::Error;

use crate::mantle::helpers::MantleOperationVerificationHelper;

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
const EXECUTION_GAS_LIMIT: Gas = Gas::new(3_193_360);

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
    #[error("Applying this transaction would cause a balance overflow")]
    BalanceOverflow,
    #[error("Unbalanced transaction, balance does not match fees")]
    UnbalancedTransaction,
    #[error(transparent)]
    GasOverflow(#[from] GasOverflow),
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
    #[error("Verification error: {0}")]
    VerificationError(#[from] VerificationError),
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
    fn compute_block_rewards(
        mut self,
        total_fee_burned: GasCost,
        total_fee_tip: GasCost,
    ) -> Result<Self, GasOverflow> {
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
                * u128::from(
                    self.cryptarchia_ledger
                        .get_fee_from_index(window_index)
                        .into_inner(),
                );
        let reward_denominator = INFLATION_DENOMINATOR * A_SCALE;

        // blend get 60% of block rewards while leaders get the 40% remaining + the
        // tips. Casting as Value truncate the floating points
        let blend_reward = (reward_numerator * BLEND_REWARD_SHARE_NUMERATOR
            / (reward_denominator * BLEND_REWARD_SHARE_DENOMINATOR))
            as Value;
        let leader_reward = GasCost::from(
            (reward_numerator * LEADER_REWARD_SHARE_NUMERATOR
                / (reward_denominator * LEADER_REWARD_SHARE_DENOMINATOR)) as Value,
        )
        .checked_add(total_fee_tip)?;

        self.mantle_ledger.leaders = self
            .mantle_ledger
            .leaders
            .add_pending_rewards(leader_reward.into_inner());

        self.mantle_ledger.sdp.add_blend_income(blend_reward);

        Ok(self)
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
        let mut total_block_execution_gas: Gas = 0.into();
        let mut total_fee_burned: GasCost = 0.into();
        let mut total_fee_tip: GasCost = 0.into();
        for tx in txs {
            let balance;
            (self, balance) = self.try_apply_tx::<_, Constants>(config, &tx)?;

            // Check the transaction is balanced
            let total_gas_cost = AuthenticatedMantleTx::total_gas_cost::<Constants>(&tx)?;
            tracing::debug!(
                balance,
                total_gas_cost = total_gas_cost.into_inner(),
                storage_gas_price = ?tx.mantle_tx().storage_gas_price,
                execution_gas_price = ?tx.mantle_tx().execution_gas_price,
                "tx balance check"
            );
            match balance.cmp(&Balance::from(total_gas_cost.into_inner())) {
                Ordering::Less => return Err(LedgerError::InsufficientBalance),
                Ordering::Greater => return Err(LedgerError::UnbalancedTransaction),
                Ordering::Equal => {} // OK!
            }

            // Update the total of fee burned and tipped in the block
            let tx_fee_burned = GasCost::calculate(
                AuthenticatedMantleTx::execution_gas_consumption::<Constants>(&tx)?,
                *self.cryptarchia_ledger.execution_base_fee(),
            )?
            .checked_add(AuthenticatedMantleTx::storage_gas_cost(&tx)?)?;

            // Check that the transaction at least pays for the base fee
            if balance < Balance::from(tx_fee_burned.into_inner()) {
                return Err(LedgerError::InsufficientExecutionFee);
            }

            // Check that the transaction pays the correct storage fees
            // TODO: remove the storage price from the Mantle Transaction and wallet should
            // pull the price from ledger to get the fees to pay
            if tx.mantle_tx().storage_gas_price != *self.cryptarchia_ledger.storage_gas_price() {
                return Err(LedgerError::InvalidStoragePrice);
            }
            let tx_fee_tip = GasCost::from(balance as Value).checked_sub(tx_fee_burned)?;
            total_fee_burned = total_fee_burned.checked_add(tx_fee_burned)?;
            total_fee_tip = total_fee_tip.checked_add(tx_fee_tip)?;
            total_block_execution_gas = total_block_execution_gas
                .checked_add(AuthenticatedMantleTx::execution_gas_consumption::<Constants>(&tx)?)?;

            // Check that the block is not exceeding the Gas limit
            if total_block_execution_gas > EXECUTION_GAS_LIMIT {
                return Err(LedgerError::TooMuchExecutionGas {
                    gas: total_block_execution_gas,
                    limit: EXECUTION_GAS_LIMIT,
                });
            }
        }
        // Compute Block rewards and give tips
        self = self.compute_block_rewards(total_fee_burned, total_fee_tip)?;
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

    #[must_use]
    pub fn tx_context(&self) -> MantleTxContext {
        MantleTxContext {
            gas_context: self.mantle_ledger().channels().into(),
            leader_reward_amount: self.mantle_ledger().leader_reward_amount(),
        }
    }

    /// Applies a transaction to the ledger state, returning the updated state
    /// and the net balance change.
    ///
    /// # Prerequisites
    ///
    /// A transaction must not be applied unless all required proofs have been
    /// fully verified.
    ///
    /// Proof verification is currently split across multiple paths depending on
    /// the operation:
    /// - `SignedMantleTx::verify_ops_proofs`: Invoked during construction
    ///   (`SignedMantleTx::new`, e.g. on deserialization). Handles:
    ///   `ChannelInscribe`, `LeaderClaim`.
    /// - `SignedMantleTx::verify_ops_proofs_with_helper`: Invoked here before
    ///   applying the transaction. Handles: `ChannelWithdraw`.
    /// - Additional validation: Performed by the ledger or implicitly satisfied
    ///   by certain operations.
    ///
    /// This fragmented design means verification may be:
    /// - Distributed across different stages, and
    /// - Potentially duplicated or missed if assumptions about prior
    ///   verification are incorrect.
    ///
    /// Callers are responsible for ensuring that all required proofs have been
    /// verified before applying the transaction.
    ///
    /// TODO: A refactor into a typed state model to enforce verification at
    /// compile is planned.
    fn try_apply_tx<Id, Constants: GasConstants>(
        mut self,
        config: &Config,
        tx: impl AuthenticatedMantleTx,
    ) -> Result<(Self, Balance), LedgerError<Id>> {
        let operation_verification_helper =
            MantleOperationVerificationHelper::new(&self.mantle_ledger);
        tx.verify_ops_proofs_with_helper(&operation_verification_helper)
            .map_err(LedgerError::VerificationError)?;

        let mut balance: Balance = 0;
        let tx_hash = tx.hash();
        for (op, proof) in tx.ops_with_proof() {
            match (op, proof) {
                // The signature for channel ops can be verified before reaching this point,
                // as you only need the signer's public key and tx hash
                // Callers are expected to validate the proof before calling this function.
                (Op::ChannelInscribe(op), _) => {
                    self.mantle_ledger = self.mantle_ledger.try_apply_channel_inscription(op)?;
                }
                (Op::ChannelSetKeys(op), OpProof::Ed25519Sig(sig)) => {
                    self.mantle_ledger = self
                        .mantle_ledger
                        .try_apply_channel_set_keys(op, sig, &tx_hash)?;
                }
                (Op::ChannelDeposit(op), OpProof::NoProof) => {
                    let deposit_amount;
                    (self.mantle_ledger, deposit_amount) =
                        self.mantle_ledger.try_apply_channel_deposit(op)?;
                    balance = balance
                        .checked_sub(deposit_amount.into())
                        .ok_or(LedgerError::BalanceOverflow)?;
                }
                (Op::ChannelWithdraw(op), OpProof::ChannelWithdrawProof(_proof)) => {
                    let withdraw_amount;
                    (self.mantle_ledger, withdraw_amount) =
                        self.mantle_ledger.try_apply_channel_withdraw(op)?;
                    balance = balance
                        .checked_add(withdraw_amount.into())
                        .ok_or(LedgerError::BalanceOverflow)?;
                }
                (
                    Op::SDPDeclare(op),
                    OpProof::ZkAndEd25519Sigs {
                        zk_sig,
                        ed25519_sig,
                    },
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
                (Op::SDPActive(op), OpProof::ZkSig(sig)) => {
                    self.mantle_ledger = self
                        .mantle_ledger
                        .try_apply_sdp_active(op, sig, tx_hash, config)?;
                }
                (Op::SDPWithdraw(op), OpProof::ZkSig(sig)) => {
                    self.mantle_ledger = self
                        .mantle_ledger
                        .try_apply_sdp_withdraw(op, sig, tx_hash, config)?;
                }
                (Op::LeaderClaim(op), OpProof::PoC(_)) => {
                    // Correct derivation of the voucher nullifier and membership in the merkle tree
                    // can be verified outside of this function since public inputs are already
                    // available. Callers are expected to validate the proof
                    // before calling this function.
                    let reward;
                    (self.mantle_ledger, reward) = self.mantle_ledger.try_apply_leader_claim(op)?;
                    balance = balance
                        .checked_add(reward.into())
                        .ok_or(LedgerError::BalanceOverflow)?;
                }
                (Op::Transfer(op), OpProof::ZkSig(sig)) => {
                    let transfer_balance;
                    (self.cryptarchia_ledger, transfer_balance) =
                        self.cryptarchia_ledger.try_apply_transfer::<_, Constants>(
                            self.mantle_ledger.locked_notes(),
                            op,
                            sig,
                            tx_hash,
                        )?;
                    balance = balance
                        .checked_add(transfer_balance)
                        .ok_or(LedgerError::BalanceOverflow)?;
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
    use lb_core::{
        mantle::{
            MantleTx, Note, SignedMantleTx, Transaction as _,
            gas::{GasPrice, MainnetGasConstants},
            genesis_tx::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE},
            ops::{
                channel::{
                    ChannelId, MsgId, deposit::DepositOp, inscribe::InscriptionOp,
                    set_keys::SetKeysOp, withdraw::ChannelWithdrawOp,
                },
                transfer::TransferOp,
            },
        },
        proofs::channel_withdraw_proof::{ChannelWithdrawProof, WithdrawSignature},
    };
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey, ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::cryptarchia::tests::utxo_with_sk;

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    type HeaderId = [u8; 32];

    fn create_tx(
        inputs: Vec<NoteId>,
        outputs: Vec<Note>,
        sks: &[ZkKey],
        execution_price: GasPrice,
        storage_price: GasPrice,
    ) -> SignedMantleTx {
        let transfer_op = TransferOp::new(inputs, outputs);
        let mantle_tx = MantleTx {
            ops: vec![Op::Transfer(transfer_op)],
            execution_gas_price: execution_price,
            storage_gas_price: storage_price,
        };
        SignedMantleTx {
            ops_proofs: vec![OpProof::ZkSig(
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

    enum Key {
        Ed25519(Ed25519Key),
        Zk(ZkKey),
        EmptyZk,
        Withdraw(ChannelWithdrawProof),
        None,
    }

    fn create_signed_tx(op: Op, signing_key: &Key) -> SignedMantleTx {
        create_multi_signed_tx(vec![op], vec![signing_key])
    }

    fn create_multi_signed_tx(ops: Vec<Op>, signing_keys: Vec<&Key>) -> SignedMantleTx {
        let mantle_tx = MantleTx {
            ops: ops.clone(),
            execution_gas_price: 0.into(),
            storage_gas_price: 0.into(),
        };

        let tx_hash = mantle_tx.hash();
        let ops_proofs = signing_keys
            .into_iter()
            .zip(ops)
            .map(|(key, _)| match key {
                Key::Ed25519(key) => {
                    OpProof::Ed25519Sig(key.sign_payload(tx_hash.as_signing_bytes().as_ref()))
                }
                Key::Zk(key) => OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(key), tx_hash.as_ref()).unwrap(),
                ),
                Key::EmptyZk => OpProof::ZkSig(ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap()),
                Key::Withdraw(proof) => OpProof::ChannelWithdrawProof(proof.clone()),
                Key::None => OpProof::NoProof,
            })
            .collect();

        SignedMantleTx::new(mantle_tx, ops_proofs)
            .expect("Test transaction should have valid signatures")
    }

    fn create_channel(
        ledger_state: LedgerState,
        config: &Config,
        id: ChannelId,
        signing_key: &Ed25519Key,
        verifying_key: Ed25519PublicKey,
    ) -> LedgerState {
        ledger_state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(
                config,
                create_signed_tx(
                    Op::ChannelInscribe(InscriptionOp {
                        channel_id: id,
                        inscription: vec![1, 2, 3, 4],
                        parent: MsgId::root(),
                        signer: verifying_key,
                    }),
                    &Key::Ed25519(signing_key.clone()),
                ),
            )
            .unwrap()
            .0
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
            GENESIS_EXECUTION_GAS_PRICE,
            GENESIS_STORAGE_GAS_PRICE,
        );
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx).unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            &[sk],
            GENESIS_EXECUTION_GAS_PRICE,
            GENESIS_STORAGE_GAS_PRICE,
        );

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

        let tx = create_signed_tx(Op::ChannelInscribe(inscribe_op), &Key::Ed25519(signing_key));
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

        let tx = create_signed_tx(Op::ChannelSetKeys(set_keys_op), &Key::Ed25519(signing_key));
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
    fn test_channel_deposit_operation() {
        let test_config = config();
        let (sk, utxo) = utxo_with_sk();
        let mut ledger_state = LedgerState::from_utxos([utxo], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([4; 32]);

        // First, create a channel by submitting an inscription
        ledger_state = create_channel(
            ledger_state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key,
        );
        assert!(
            ledger_state
                .mantle_ledger()
                .channels()
                .channels
                .contains_key(&channel_id)
        );

        // Submit a deposit operation
        let deposit = DepositOp {
            channel_id,
            amount: 10,
            metadata: vec![5, 6, 7, 8],
        };
        let ops = vec![
            Op::ChannelDeposit(deposit.clone()),
            Op::Transfer(TransferOp {
                inputs: vec![utxo.id()],
                outputs: vec![Note::new(
                    utxo.note.value - deposit.amount,
                    sk.to_public_key(),
                )],
            }),
        ];
        let result = ledger_state.try_apply_tx::<HeaderId, MainnetGasConstants>(
            &test_config,
            create_multi_signed_tx(ops, vec![&Key::None, &Key::Zk(sk)]),
        );
        let (new_state, balance) = result.unwrap();
        assert_eq!(
            new_state
                .mantle_ledger()
                .channels()
                .channels
                .get(&channel_id)
                .unwrap()
                .balance,
            deposit.amount,
        );
        assert_eq!(balance, Balance::from(0));
    }

    #[test]
    fn test_channel_withdraw_operation() {
        let test_config = config();
        let (sk, utxo) = utxo_with_sk();
        let mut ledger_state = LedgerState::from_utxos([utxo], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([9; 32]);

        ledger_state = create_channel(
            ledger_state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key,
        );

        // Deposit some funds into the channel
        let deposit = DepositOp {
            channel_id,
            amount: 10,
            metadata: vec![5, 6, 7, 8],
        };
        let deposit_ops = vec![
            Op::ChannelDeposit(deposit.clone()),
            Op::Transfer(TransferOp {
                inputs: vec![utxo.id()],
                outputs: vec![Note::new(
                    utxo.note.value - deposit.amount,
                    sk.to_public_key(),
                )],
            }),
        ];
        ledger_state = ledger_state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(
                &test_config,
                create_multi_signed_tx(deposit_ops, vec![&Key::None, &Key::Zk(sk)]),
            )
            .unwrap()
            .0;

        // Withdraw some funds from the channel
        let withdraw = ChannelWithdrawOp {
            channel_id,
            amount: 6,
        };
        let recipient_sk = ZkKey::from(BigUint::from(99u8));
        let recipient_pk = recipient_sk.to_public_key();
        let transfer_op = TransferOp {
            inputs: vec![],
            outputs: vec![Note::new(withdraw.amount, recipient_pk)],
        };
        let withdraw_tx = MantleTx {
            ops: vec![
                Op::ChannelWithdraw(withdraw.clone()),
                Op::Transfer(transfer_op.clone()),
            ],
            execution_gas_price: 0.into(),
            storage_gas_price: 0.into(),
        };
        let withdraw_tx_hash = withdraw_tx.hash();
        let withdraw_proof = ChannelWithdrawProof::new(vec![WithdrawSignature::new(
            0,
            signing_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.ops,
            vec![&Key::Withdraw(withdraw_proof), &Key::EmptyZk],
        );

        let result =
            ledger_state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, signed_tx);
        assert!(result.is_ok());

        let (new_state, tx_balance) = result.unwrap();
        assert_eq!(tx_balance, 0);
        let channel_balance = new_state
            .mantle_ledger()
            .channels()
            .channels
            .get(&channel_id)
            .unwrap()
            .balance;
        assert_eq!(channel_balance, 4);
        let output_utxo = transfer_op.utxo_by_index(0).unwrap();
        assert_eq!(output_utxo.note.value, withdraw.amount);
        assert_eq!(output_utxo.note.pk, recipient_sk.to_public_key());
        assert!(new_state.latest_utxos().contains(&output_utxo.id()));
    }

    #[test]
    fn test_channel_withdraw_invalid_helper_backed_proof_fails_on_apply() {
        let test_config = config();
        let (sk, utxo) = utxo_with_sk();
        let mut ledger_state = LedgerState::from_utxos([utxo], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([10; 32]);

        ledger_state = create_channel(
            ledger_state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key,
        );

        // Deposit some funds into the channel
        let deposit = DepositOp {
            channel_id,
            amount: 10,
            metadata: vec![],
        };
        let deposit_ops = vec![
            Op::ChannelDeposit(deposit.clone()),
            Op::Transfer(TransferOp {
                inputs: vec![utxo.id()],
                outputs: vec![Note::new(
                    utxo.note.value - deposit.amount,
                    sk.to_public_key(),
                )],
            }),
        ];
        ledger_state = ledger_state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(
                &test_config,
                create_multi_signed_tx(deposit_ops, vec![&Key::None, &Key::Zk(sk)]),
            )
            .unwrap()
            .0;
        let channel_balance_after_deposit = ledger_state
            .mantle_ledger()
            .channels()
            .channels
            .get(&channel_id)
            .unwrap()
            .balance;

        // Try to withdraw some funds from the channel, but with an invalid proof
        let withdraw = ChannelWithdrawOp {
            channel_id,
            amount: 6,
        };
        let wrong_key = Ed25519Key::from_bytes(&[42; 32]);
        let recipient_sk = ZkKey::from(BigUint::from(100u8));
        let recipient_pk = recipient_sk.to_public_key();
        let transfer_op = TransferOp {
            inputs: vec![],
            outputs: vec![Note::new(withdraw.amount, recipient_pk)],
        };
        let withdraw_tx = MantleTx {
            ops: vec![
                Op::ChannelWithdraw(withdraw),
                Op::Transfer(transfer_op.clone()),
            ],
            execution_gas_price: 0.into(),
            storage_gas_price: 0.into(),
        };
        let withdraw_tx_hash = withdraw_tx.hash();
        let invalid_proof = ChannelWithdrawProof::new(vec![WithdrawSignature::new(
            0,
            wrong_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.ops,
            vec![&Key::Withdraw(invalid_proof), &Key::EmptyZk],
        );

        let result = ledger_state
            .clone()
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, signed_tx);
        assert_eq!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelWithdrawProofInvalidSignature {
                    op_index: 0,
                    signature_index: 0,
                }
            ))
        );

        let channel_balance_after_withdraw = ledger_state
            .mantle_ledger()
            .channels()
            .channels
            .get(&channel_id)
            .unwrap()
            .balance;
        assert_eq!(channel_balance_after_deposit, 10);
        assert_eq!(
            channel_balance_after_deposit,
            channel_balance_after_withdraw
        );
        let recipient_utxo = transfer_op.utxo_by_index(0).unwrap();
        assert!(!ledger_state.latest_utxos().contains(&recipient_utxo.id()));
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

        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
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

        let second_tx = create_signed_tx(
            Op::ChannelInscribe(second_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
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

        let empty_tx = create_signed_tx(
            Op::ChannelInscribe(empty_inscribe),
            &Key::Ed25519(signing_key),
        );
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
        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key),
        );
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
            &Key::Ed25519(unauthorized_signing_key),
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

        let tx = create_signed_tx(Op::ChannelSetKeys(set_keys_op), &Key::Ed25519(signing_key));
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
        let tx = create_multi_signed_tx(
            ops,
            vec![
                &Key::Ed25519(sk1.clone()),
                &Key::Ed25519(sk2),
                &Key::Ed25519(sk1),
                &Key::Ed25519(sk4),
            ],
        );

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
            GENESIS_EXECUTION_GAS_PRICE,
            (GENESIS_STORAGE_GAS_PRICE.into_inner() + 1).into(), // wrong storage gas price
        );
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx).unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            &[sk],
            GENESIS_EXECUTION_GAS_PRICE,
            (GENESIS_STORAGE_GAS_PRICE.into_inner() + 1).into(), // wrong storage gas price
        );

        let result = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        assert_eq!(result, Err(LedgerError::InvalidStoragePrice));
    }

    #[test]
    #[ignore = "TODO: enable once we determine non-zero genesis execution gas price"]
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
            1.into(),
            1.into(),
        );
        // Pays 2925 fees = 2705 execution base fee + 0 execution tip + 220 storage
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx).unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            &[sk],
            1.into(),
            1.into(),
        );

        let result = ledger
            .clone()
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The unwrap should succeed because the user pays at least the base fee of 2705
        result.unwrap();

        ledger.cryptarchia_ledger = ledger.cryptarchia_ledger.set_execution_base_fee(10.into());

        let result = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The transaction should be rejected because the price indicated for execution
        // doesn't cover the base fee that cost 27 050
        assert_eq!(result, Err(LedgerError::InsufficientExecutionFee));
    }

    #[test]
    #[ignore = "TODO: enable once we determine non-zero genesis execution/storage gas price"]
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
            1.into(),
            1.into(),
        );
        // The tx ays 2925 fees = 2705 execution base fee + 0 execution tip + 220
        // storage
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx).unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
            1.into(),
            1.into(),
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
            2.into(),
            1.into(),
        );
        // The tx ays 5630 fees = 2705 execution base fee + 2705 execution tip + 220
        // storage
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx).unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            &[sk],
            2.into(),
            1.into(),
        );
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
