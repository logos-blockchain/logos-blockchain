mod config;
// The ledger is split into two modules:
// - `cryptarchia`: the base functionalities needed by the Cryptarchia consensus
//   algorithm, including a minimal UTxO model.
// - `mantle_ops`: our extensions in the form of Mantle operations, e.g. SDP.
pub mod cryptarchia;
pub mod mantle;

use std::{collections::HashMap, hash::Hash};

pub use config::Config;
use cryptarchia::LedgerState as CryptarchiaLedger;
pub use cryptarchia::{EpochState, UtxoTree};
use lb_core::{
    block::BlockNumber,
    mantle::{
        AuthenticatedMantleTx, GenesisTx, NoteId, Op, OpProof, Utxo, Value, VerificationError,
        gas::{Gas, GasConstants, GasCost, GasOverflow},
        ledger::Operation as _,
        ops::{
            channel::{
                deposit::{DepositExecutionContext, DepositValidationContext},
                withdraw::{WithdrawExecutionContext, WithdrawValidationContext},
            },
            leader_claim::{LeaderClaimExecutionContext, LeaderClaimValidationContext},
        },
        tx::{GasPrices, MantleTxContext, MantleTxGasContext},
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
    #[error("Insufficient balance")]
    InsufficientBalance,
    #[error("Applying this transaction would cause a balance overflow")]
    BalanceOverflow,
    #[error("Unbalanced transaction, balance does not match fees")]
    UnbalancedTransaction,
    #[error(transparent)]
    GasOverflow(#[from] GasOverflow),
    #[error("Mantle error: {0}")]
    Mantle(#[from] mantle::Error),
    #[error("Inputs error: {0}")]
    Inputs(#[from] lb_core::mantle::ledger::InputsError),
    #[error("Mantle error: {0}")]
    Outputs(#[from] lb_core::mantle::ledger::OutputsError),
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
        txs: impl Iterator<Item = impl AuthenticatedMantleTx<Context = GasPrices>>,
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
        txs: impl Iterator<Item = impl AuthenticatedMantleTx<Context = GasPrices>>,
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

    #[must_use]
    pub const fn get_gas_prices(&self) -> GasPrices {
        GasPrices {
            execution_base_gas_price: *self.cryptarchia_ledger.execution_base_fee(),
            storage_gas_price: *self.cryptarchia_ledger.storage_gas_price(),
        }
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
        txs: impl Iterator<Item = impl AuthenticatedMantleTx<Context = GasPrices>>,
    ) -> Result<Self, LedgerError<Id>> {
        let mut total_block_execution_gas: Gas = 0.into();
        let mut total_fee_burned: GasCost = 0.into();
        let mut total_fee_tip: GasCost = 0.into();
        for tx in txs {
            let balance;
            (self, balance) = self.try_apply_tx::<_, Constants>(config, &tx)?;

            let gas_prices = GasPrices {
                execution_base_gas_price: *self.cryptarchia_ledger.execution_base_fee(),
                storage_gas_price: *self.cryptarchia_ledger.storage_gas_price(),
            };
            // Check the transaction is balanced
            let total_gas_cost =
                AuthenticatedMantleTx::total_gas_cost::<Constants>(&tx, gas_prices.clone())?;
            tracing::debug!(
                balance,
                total_gas_cost = total_gas_cost.into_inner(),
                storage_gas_price = ?self.cryptarchia_ledger.storage_gas_price(),
                execution_gas_price = ?self.cryptarchia_ledger.execution_base_fee(),
                "tx balance check"
            );

            // Check that the transaction at least pays for the base execution fee and
            // storage
            if balance < Balance::from(total_gas_cost.into_inner()) {
                return Err(LedgerError::InsufficientBalance);
            }

            // Update the total of fee burned and tipped in the block
            let tx_fee_burned = GasCost::calculate(
                AuthenticatedMantleTx::execution_gas_consumption::<Constants>(
                    &tx,
                    gas_prices.clone(),
                )?,
                gas_prices.execution_base_gas_price,
            )?
            .checked_add(AuthenticatedMantleTx::storage_gas_cost(
                &tx,
                gas_prices.clone(),
            )?)?;

            let tx_fee_tip = GasCost::from(balance as Value).checked_sub(tx_fee_burned)?;
            total_fee_burned = total_fee_burned.checked_add(tx_fee_burned)?;
            total_fee_tip = total_fee_tip.checked_add(tx_fee_tip)?;
            total_block_execution_gas = total_block_execution_gas.checked_add(
                AuthenticatedMantleTx::execution_gas_consumption::<Constants>(&tx, gas_prices)?,
            )?;

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
            gas_context: MantleTxGasContext::from_channels(
                self.mantle_ledger().channels(),
                self.get_gas_prices(),
            ),
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
    #[expect(clippy::too_many_lines, reason = "We need to refactor this.")]
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
                (Op::ChannelInscribe(op), OpProof::Ed25519Sig(sig)) => {
                    self.mantle_ledger = self.mantle_ledger.try_apply_channel_inscription(
                        op,
                        sig,
                        tx_hash,
                        self.cryptarchia_ledger.slot,
                    )?;
                }
                (Op::ChannelConfig(op), OpProof::ChannelMultiSigProof(sig)) => {
                    self.mantle_ledger = self.mantle_ledger.try_apply_channel_set_keys(
                        op,
                        sig,
                        &tx_hash,
                        self.cryptarchia_ledger.slot,
                    )?;
                }
                (Op::ChannelDeposit(op), OpProof::ZkSig(sig)) => {
                    let channels = self.mantle_ledger.channels();
                    let locked_notes = self.mantle_ledger.locked_notes();
                    let utxos = self.cryptarchia_ledger.latest_utxos();

                    // Validate the Deposit
                    op.validate(&DepositValidationContext {
                        channels,
                        locked_notes,
                        utxos,
                        tx_hash: &tx_hash,
                        deposit_sig: sig,
                    })
                    .map_err(mantle::Error::Channel)?;

                    // Execute the SetKeys
                    let result = op
                        .execute(DepositExecutionContext {
                            channels: channels.clone(),
                            locked_notes: locked_notes.clone(),
                            utxos: utxos.clone(),
                        })
                        .map_err(mantle::Error::Channel)?;
                    self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                    self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
                }
                (Op::ChannelWithdraw(op), OpProof::ChannelMultiSigProof(sigs)) => {
                    let channels = self.mantle_ledger.channels();
                    let utxos = self.cryptarchia_ledger.latest_utxos();

                    // Validate the Withdraw
                    op.validate(&WithdrawValidationContext {
                        channels,
                        tx_hash: &tx_hash,
                        withdraw_sigs: sigs,
                    })
                    .map_err(mantle::Error::Channel)?;

                    // Execute the Withdraw
                    let result = op
                        .execute(WithdrawExecutionContext {
                            channels: channels.clone(),
                            utxos: utxos.clone(),
                        })
                        .map_err(mantle::Error::Channel)?;
                    self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                    self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
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
                (Op::LeaderClaim(op), OpProof::PoC(poc)) => {
                    // Validate the LeaderClaim
                    op.validate(&LeaderClaimValidationContext {
                        nullifiers: self.mantle_ledger.leaders.nullifiers(),
                        claimable_vouchers_root: &self
                            .mantle_ledger
                            .leaders
                            .vouchers_snapshot_root(),
                        proof_of_claim: poc,
                        tx_hash: &tx_hash,
                    })
                    .map_err(mantle::Error::LeaderClaim)?;

                    // Execute the LeaderClaim
                    let result = op
                        .execute(LeaderClaimExecutionContext {
                            nullifiers: self.mantle_ledger.leaders.nullifiers_cloned(),
                            reward_amount: self.mantle_ledger.leaders.reward_amount(),
                            claimable_rewards: self.mantle_ledger.leaders.claimable_rewards(),
                            utxos: self.cryptarchia_ledger.latest_utxos().clone(),
                        })
                        .map_err(mantle::Error::LeaderClaim)?;
                    self.mantle_ledger
                        .leaders
                        .update_nullifiers(result.nullifiers);
                    self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);

                    self.mantle_ledger
                        .leaders
                        .update_rewards(result.claimable_rewards);
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
            gas::MainnetGasConstants,
            ledger::{Inputs, Outputs},
            ops::{
                channel::{
                    ChannelId, MsgId, config::ChannelConfigOp, deposit::DepositOp,
                    inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
                },
                transfer::TransferOp,
            },
        },
        proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
    };
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey, ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::cryptarchia::tests::utxo_with_sk;

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    type HeaderId = [u8; 32];

    fn create_tx(inputs: Vec<NoteId>, outputs: Vec<Note>, sks: &[ZkKey]) -> SignedMantleTx {
        let transfer_op = TransferOp::new(Inputs::new(inputs), Outputs::new(outputs));
        let mantle_tx = MantleTx(vec![Op::Transfer(transfer_op)]);
        SignedMantleTx {
            ops_proofs: vec![OpProof::ZkSig(
                ZkKey::multi_sign(sks, &mantle_tx.hash().to_fr()).unwrap(),
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

    fn update_ledger_prices(ledger_state: &mut LedgerState, new_execution: u64, new_storage: u64) {
        ledger_state.cryptarchia_ledger = ledger_state
            .cryptarchia_ledger
            .clone()
            .set_storage_price(new_storage.into());
        ledger_state.cryptarchia_ledger = ledger_state
            .cryptarchia_ledger
            .clone()
            .set_execution_base_fee(new_execution.into());
    }

    enum Key {
        Ed25519(Ed25519Key),
        Zk(ZkKey),
        EmptyZk,
        MultiSequencer(ChannelMultiSigProof),
    }

    fn create_signed_tx(op: Op, signing_key: &Key) -> SignedMantleTx {
        create_multi_signed_tx(vec![op], vec![signing_key])
    }

    fn create_multi_signed_tx(ops: Vec<Op>, signing_keys: Vec<&Key>) -> SignedMantleTx {
        let mantle_tx = MantleTx(ops.clone());

        let tx_hash = mantle_tx.hash();
        let ops_proofs = signing_keys
            .into_iter()
            .zip(ops)
            .map(|(key, _)| match key {
                Key::Ed25519(key) => {
                    OpProof::Ed25519Sig(key.sign_payload(tx_hash.as_signing_bytes().as_ref()))
                }
                Key::Zk(key) => OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(key), &tx_hash.to_fr()).unwrap(),
                ),
                Key::EmptyZk => OpProof::ZkSig(ZkKey::multi_sign(&[], &tx_hash.to_fr()).unwrap()),
                Key::MultiSequencer(proof) => OpProof::ChannelMultiSigProof(proof.clone()),
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
        );
        let fees =
            AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx, GasPrices::default())
                .unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk]);

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
        if let Op::Transfer(transfer_op) = &tx.mantle_tx.ops()[0] {
            let output_utxo = transfer_op.outputs.utxo_by_index(0, transfer_op).unwrap();
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
    fn test_channel_config_operation() {
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([3; 32]);

        let config_op = ChannelConfigOp {
            channel: channel_id,
            keys: vec![verifying_key],
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            withdraw_threshold: 1,
        };

        let config_tx = MantleTx(vec![Op::ChannelConfig(config_op.clone())]);
        let config_tx_hash = config_tx.hash();
        let config_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            signing_key.sign_payload(config_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let tx = create_signed_tx(
            Op::ChannelConfig(config_op),
            &Key::MultiSequencer(config_proof),
        );
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
                .accredited_keys,
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
            inputs: Inputs::new(vec![utxo.id()]),
            metadata: vec![5, 6, 7, 8],
        };
        let ops = vec![Op::ChannelDeposit(deposit)];
        let result = ledger_state.try_apply_tx::<HeaderId, MainnetGasConstants>(
            &test_config,
            create_multi_signed_tx(ops, vec![&Key::Zk(sk)]),
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
            utxo.note.value,
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
            inputs: Inputs::new(vec![utxo.id()]),
            metadata: vec![5, 6, 7, 8],
        };
        let deposit_ops = vec![Op::ChannelDeposit(deposit)];
        ledger_state = ledger_state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(
                &test_config,
                create_multi_signed_tx(deposit_ops, vec![&Key::Zk(sk)]),
            )
            .unwrap()
            .0;

        assert_eq!(
            ledger_state
                .mantle_ledger
                .channels()
                .channels
                .get(&channel_id)
                .expect("channel_created")
                .balance,
            utxo.note.value
        );

        // Withdraw some funds from the channel
        let recipient_sk = ZkKey::from(BigUint::from(99u8));
        let recipient_pk = recipient_sk.to_public_key();
        let withdraw_note = Note {
            value: 500,
            pk: recipient_pk,
        };
        let withdraw = ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new(vec![withdraw_note]),
            withdraw_nonce: 0,
        };
        let withdraw_tx = MantleTx(vec![Op::ChannelWithdraw(withdraw.clone())]);
        let withdraw_tx_hash = withdraw_tx.hash();
        let withdraw_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            signing_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let signed_tx =
            create_multi_signed_tx(withdraw_tx.0, vec![&Key::MultiSequencer(withdraw_proof)]);

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
        assert_eq!(channel_balance, utxo.note.value - withdraw_note.value);
        let withdraw_utxo = withdraw
            .outputs
            .utxos(&withdraw)
            .next()
            .expect("withdraw should have at least one utxo")
            .id();
        assert!(new_state.latest_utxos().contains(&withdraw_utxo));
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
            inputs: Inputs::new(vec![utxo.id()]),
            metadata: vec![],
        };
        let deposit_ops = vec![Op::ChannelDeposit(deposit)];
        ledger_state = ledger_state
            .try_apply_tx::<HeaderId, MainnetGasConstants>(
                &test_config,
                create_multi_signed_tx(deposit_ops, vec![&Key::Zk(sk)]),
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
        let recipient_sk = ZkKey::from(BigUint::from(99u8));
        let recipient_pk = recipient_sk.to_public_key();
        let withdraw_note = Note {
            value: 500,
            pk: recipient_pk,
        };
        let withdraw = ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new(vec![withdraw_note]),
            withdraw_nonce: 0,
        };
        let wrong_key = Ed25519Key::from_bytes(&[42; 32]);
        let withdraw_tx = MantleTx(vec![Op::ChannelWithdraw(withdraw.clone())]);
        let withdraw_tx_hash = withdraw_tx.hash();
        let invalid_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            wrong_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.0,
            vec![&Key::MultiSequencer(invalid_proof), &Key::EmptyZk],
        );

        let result = ledger_state
            .clone()
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, signed_tx);
        assert_eq!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelMultiSigProofInvalidSignature {
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
        assert_eq!(channel_balance_after_deposit, utxo.note.value);
        assert_eq!(
            channel_balance_after_deposit,
            channel_balance_after_withdraw
        );
        let withdraw_utxo = withdraw
            .outputs
            .utxos(&withdraw)
            .next()
            .expect("withdraw should have at least one utxo")
            .id();
        assert!(!ledger_state.latest_utxos().contains(&withdraw_utxo));
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

        let config_op = ChannelConfigOp {
            channel: channel_id,
            keys: vec![],
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            withdraw_threshold: 1,
        };

        let config_tx = MantleTx(vec![Op::ChannelConfig(config_op.clone())]);
        let config_tx_hash = config_tx.hash();
        let config_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            signing_key.sign_payload(config_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let tx = create_signed_tx(
            Op::ChannelConfig(config_op),
            &Key::MultiSequencer(config_proof),
        );
        let result = state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx);
        assert_eq!(
            result,
            Err(LedgerError::Mantle(mantle::Error::Channel(
                mantle::channel::Error::InvalidChannelConfig
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
        let (sk3, vk3) = create_test_keys_with_seed(3);
        let (_, vk4) = create_test_keys_with_seed(4);

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

        let config_op = ChannelConfigOp {
            channel: channel1,
            keys: vec![vk3, vk4],
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            withdraw_threshold: 1,
        };

        let inscribe_op3 = InscriptionOp {
            channel_id: channel1,
            inscription: vec![7, 8, 9],
            parent: config_op.id(),
            signer: vk3,
        };

        let ops = vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
            Op::ChannelConfig(config_op),
            Op::ChannelInscribe(inscribe_op3.clone()),
        ];
        let config_tx = MantleTx(ops.clone());
        let config_tx_hash = config_tx.hash();
        let config_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            sk1.sign_payload(config_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let tx = create_multi_signed_tx(
            ops,
            vec![
                &Key::Ed25519(sk1),
                &Key::Ed25519(sk2),
                &Key::MultiSequencer(config_proof),
                &Key::Ed25519(sk3),
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
                .tip_message,
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
    #[ignore = "TODO: enable once we determine non-zero genesis execution gas price"]
    fn test_fee_rejection() {
        let utxo = utxo();
        let config = config();
        let mut ledger = LedgerState::from_utxos([utxo], &config);
        update_ledger_prices(&mut ledger, 1, 1);

        let mut output_note = Note::new(1, ZkPublicKey::new(BigUint::from(0u8).into()));
        let sk = ZkKey::from(BigUint::from(0u8));
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
        );
        // Pays 2925 fees = 2705 execution base fee + 0 execution tip + 220 storage
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(
            &tx,
            ledger.get_gas_prices(),
        )
        .unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk]);

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
        assert_eq!(result, Err(LedgerError::InsufficientBalance));
    }

    #[test]
    #[ignore = "TODO: enable once we determine non-zero genesis execution/storage gas price"]
    fn test_priority_fees_go_to_leader() {
        let utxo = utxo();
        let config = config();
        let mut ledger = LedgerState::from_utxos([utxo], &config);

        let mut output_note = Note::new(1, ZkPublicKey::new(BigUint::from(0u8).into()));
        let sk = ZkKey::from(BigUint::from(0u8));
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
        );
        update_ledger_prices(&mut ledger, 1, 1);
        // The tx pays 794 fees = 590 execution base fee + 0 execution tip + 204
        // storage
        let fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(
            &tx,
            ledger.get_gas_prices(),
        )
        .unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
        );

        let result = ledger
            .clone()
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The unwrap should succeed because the user pays at least the base fee of 794
        let no_priority_fee_ledger = result.unwrap();

        // The tx ays 1794 fees = 590 execution base fee + 1000 execution tip + 204
        // storage
        output_note.value = utxo.note.value - fees.into_inner() - 1000;
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
        );

        let result = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx));
        // The unwrap should succeed because the user pays at least the base fee of 794
        let priority_fee_ledger = result.unwrap();

        assert_eq!(
            no_priority_fee_ledger
                .mantle_ledger
                .leaders
                .get_pending_rewards()
                + 1000,
            priority_fee_ledger
                .mantle_ledger
                .leaders
                .get_pending_rewards()
        );
    }
}
