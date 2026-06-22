mod config;
// The ledger is split into two modules:
// - `cryptarchia`: the base functionalities needed by the Cryptarchia consensus
//   algorithm, including a minimal UTxO model.
// - `mantle_ops`: our extensions in the form of Mantle operations, e.g. SDP.
pub mod cryptarchia;
pub mod mantle;

use std::hash::Hash;

pub use config::Config;
use cryptarchia::LedgerState as CryptarchiaLedger;
pub use cryptarchia::{EpochState, UtxoTree};
use lb_core::{
    block::BlockNumber,
    events::Events,
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
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{AdditiveGroup as _, Fr};
use mantle::LedgerState as MantleLedger;
use rpds::HashTrieMapSync;
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
    states: HashTrieMapSync<Id, LedgerState>,
    config: Config,
}

impl<Id> Ledger<Id>
where
    Id: Eq + Hash + Copy,
{
    pub fn new(id: Id, state: LedgerState, config: Config) -> Self {
        Self {
            states: HashTrieMapSync::new_sync().insert(id, state),
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
    ) -> Result<(Id, LedgerState, Events), LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
        Constants: GasConstants,
    {
        let parent_state = self
            .states
            .get(&parent_id)
            .ok_or(LedgerError::ParentNotFound(parent_id))?;

        let (new_state, events) =
            parent_state
                .clone()
                .try_update::<_, _, Constants>(slot, proof, txs, &self.config)?;

        Ok((id, new_state, events))
    }

    /// Commits a new [`LedgerState`] created by [`Self::prepare_update`].
    pub fn commit_update(&mut self, id: Id, state: LedgerState) {
        self.states.insert_mut(id, state);
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
        self.states.remove_mut(block)
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
    ) -> Result<(Self, Events), LedgerError<Id>>
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
        let last_epoch_state = self.cryptarchia_ledger.epoch_state().clone();
        let mut cryptarchia_ledger = self
            .cryptarchia_ledger
            .try_apply_header::<LeaderProof, Id>(
                slot,
                proof,
                // TODO: threading SDP here because EpochState is currently embedded in
                // CryptarchiaLedger.
                // In the future, we will pull EpochState up into LedgerState.
                &self.mantle_ledger.sdp,
                config,
            )?;
        let (mantle_ledger, reward_utxos) = self.mantle_ledger.try_apply_header(
            &last_epoch_state,
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
    ) -> Result<(Self, Events), LedgerError<Id>> {
        let mut total_block_execution_gas: Gas = 0.into();
        let mut total_fee_burned: GasCost = 0.into();
        let mut total_fee_tip: GasCost = 0.into();
        let mut block_events = Events::new();

        for tx in txs {
            let balance;
            let events;
            (self, balance, events) = self.try_apply_tx::<_, Constants>(config, &tx)?;
            block_events.extend(events);

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
        Ok((self, block_events))
    }

    pub fn from_utxos(utxos: impl IntoIterator<Item = Utxo>, config: &Config) -> Self {
        let cryptarchia_ledger = CryptarchiaLedger::from_utxos(utxos, config, Fr::ZERO);
        let mantle_ledger = MantleLedger::new(config, cryptarchia_ledger.epoch_state());
        // Seed the genesis epoch-state membership snapshots from the genesis SDP
        // ledger, which only exists after the mantle ledger is built.
        let cryptarchia_ledger = cryptarchia_ledger.with_genesis_sdp(&mantle_ledger.sdp, config);
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
    ) -> Result<(Self, Events), LedgerError<Id>> {
        let cryptarchia_ledger = CryptarchiaLedger::from_genesis_tx(&tx, config, epoch_nonce)?;
        let (mantle_ledger, events) = MantleLedger::from_genesis_tx(
            tx,
            config,
            cryptarchia_ledger.latest_utxos(),
            cryptarchia_ledger.epoch_state(),
        )?;
        // Seed the genesis epoch-state membership snapshots from the genesis SDP
        // ledger (which carries the genesis declarations applied above).
        let cryptarchia_ledger = cryptarchia_ledger.with_genesis_sdp(&mantle_ledger.sdp, config);
        Ok((
            Self {
                block_number: 0,
                cryptarchia_ledger,
                mantle_ledger,
            },
            events,
        ))
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
        self.cryptarchia_ledger
            .epoch_state_for_slot(slot, &self.mantle_ledger.sdp, config)
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
    ) -> Result<(Self, Balance, Events), LedgerError<Id>> {
        let operation_verification_helper =
            MantleOperationVerificationHelper::new(&self.mantle_ledger);
        tx.verify_ops_proofs_with_helper(&operation_verification_helper)
            .map_err(LedgerError::VerificationError)?;

        let mut balance: Balance = 0;
        let mut tx_events = Events::new();
        let tx_hash = tx.hash();
        for (op, proof) in tx.ops_with_proof() {
            match (op, proof) {
                // The signature for channel ops can be verified before reaching this point,
                // as you only need the signer's public key and tx hash
                // Callers are expected to validate the proof before calling this function.
                (Op::ChannelInscribe(op), OpProof::Ed25519Sig(sig)) => {
                    let (result, events) = self.mantle_ledger.try_apply_channel_inscription(
                        op,
                        sig,
                        tx_hash,
                        self.cryptarchia_ledger.slot,
                    )?;
                    self.mantle_ledger = result;
                    tx_events.extend(events);
                }
                (Op::ChannelConfig(op), OpProof::ChannelMultiSigProof(sig)) => {
                    let (result, events) = self.mantle_ledger.try_apply_channel_set_keys(
                        op,
                        sig,
                        &tx_hash,
                        self.cryptarchia_ledger.slot,
                    )?;
                    self.mantle_ledger = result;
                    tx_events.extend(events);
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
                    let (result, events) = op
                        .execute(DepositExecutionContext {
                            channels: channels.clone(),
                            locked_notes: locked_notes.clone(),
                            utxos: utxos.clone(),
                            tx_hash,
                        })
                        .map_err(mantle::Error::Channel)?;
                    self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                    self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
                    tx_events.extend(events);
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
                    let (result, events) = op
                        .execute(WithdrawExecutionContext {
                            channels: channels.clone(),
                            utxos: utxos.clone(),
                        })
                        .map_err(mantle::Error::Channel)?;
                    self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                    self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
                    tx_events.extend(events);
                }
                (
                    Op::SDPDeclare(op),
                    OpProof::ZkAndEd25519Sigs {
                        zk_sig,
                        ed25519_sig,
                    },
                ) => {
                    let (result, events) = self.mantle_ledger.try_apply_sdp_declaration(
                        op,
                        zk_sig,
                        ed25519_sig,
                        self.cryptarchia_ledger.latest_utxos(),
                        tx_hash,
                        config,
                    )?;
                    self.mantle_ledger = result;
                    tx_events.extend(events);
                }
                (Op::SDPActive(op), OpProof::ZkSig(sig)) => {
                    let (result, events) = self
                        .mantle_ledger
                        .try_apply_sdp_active(op, sig, tx_hash, config)?;
                    self.mantle_ledger = result;
                    tx_events.extend(events);
                }
                (Op::SDPWithdraw(op), OpProof::ZkSig(sig)) => {
                    let (result, events) = self
                        .mantle_ledger
                        .try_apply_sdp_withdraw(op, sig, tx_hash, config)?;
                    self.mantle_ledger = result;
                    tx_events.extend(events);
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
                    let (result, events) = op
                        .execute(LeaderClaimExecutionContext {
                            nullifiers: self.mantle_ledger.leaders.nullifiers_cloned(),
                            reward_amount: self.mantle_ledger.leaders.reward_amount(),
                            claimable_rewards: self.mantle_ledger.leaders.claimable_rewards(),
                            utxos: self.cryptarchia_ledger.latest_utxos().clone(),
                            tx_hash,
                        })
                        .map_err(mantle::Error::LeaderClaim)?;
                    self.mantle_ledger
                        .leaders
                        .update_nullifiers(result.nullifiers);
                    self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);

                    self.mantle_ledger
                        .leaders
                        .update_rewards(result.claimable_rewards);
                    tx_events.extend(events);
                }
                (Op::Transfer(op), OpProof::ZkSig(sig)) => {
                    let transfer_balance;
                    let events;
                    (self.cryptarchia_ledger, transfer_balance, events) =
                        self.cryptarchia_ledger.try_apply_transfer::<_, Constants>(
                            self.mantle_ledger.locked_notes(),
                            op,
                            sig,
                            tx_hash,
                        )?;
                    balance = balance
                        .checked_add(transfer_balance)
                        .ok_or(LedgerError::BalanceOverflow)?;
                    tx_events.extend(events);
                }
                _ => {
                    return Err(LedgerError::UnsupportedOp);
                }
            }
        }
        Ok((self, balance, tx_events))
    }
}

#[cfg(test)]
mod tests {
    use cryptarchia::tests::{config, generate_proof, utxo};
    use lb_core::{
        events::{Event, EventPayload},
        mantle::{
            MantleTx, Note, SignedMantleTx, Transaction as _, TxHash,
            encoding::Ops,
            gas::MainnetGasConstants,
            ledger::{Inputs, Outputs, Utxos},
            ops::{
                OpId as _,
                channel::{
                    ChannelId, MsgId,
                    config::ChannelConfigOp,
                    deposit::{DepositOp, Metadata},
                    inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
                leader_claim::{LeaderClaimError, LeaderClaimOp},
                sdp::SDPActiveOp,
                transfer::TransferOp,
            },
        },
        proofs::{
            channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
            leader_claim_proof::Groth16LeaderClaimProof,
        },
        sdp::{ActivityMetadata, DeclarationId, Nonce, blend::ActivityProof},
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{CompressedGroth16Proof, Field as _};
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey, ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        cryptarchia::tests::{
            apply_and_add_utxo, apply_and_add_utxo_and_declaration, declaration_in_snapshot,
            ledger, update_ledger, utxo_with_sk,
        },
        mantle::leader::LeaderState,
    };

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    type HeaderId = [u8; 32];

    fn create_tx(inputs: Vec<NoteId>, outputs: Vec<Note>, sks: &[ZkKey]) -> SignedMantleTx {
        let transfer_op = TransferOp::new(
            Inputs::try_new(inputs).expect("Invalid inputs size"),
            Outputs::try_new(outputs).expect("Invalid outputs size"),
        );
        let mantle_tx = MantleTx([Op::Transfer(transfer_op)].into());
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

    /// The genesis epoch-state active-declarations snapshots must be seeded
    /// from the genesis SDP ledger, not left as the empty default the
    /// cryptarchia genesis constructor initializes them with.
    #[test]
    fn genesis_seeds_epoch_state_sdp_from_mantle() {
        let config = config();
        let ledger = LedgerState::from_utxos([utxo()], &config);

        let expected_for_epoch_0 = ledger
            .mantle_ledger
            .sdp
            .active_declarations(0.into(), &config.sdp_config.service_params);
        let expected_for_epoch_1 = ledger
            .mantle_ledger
            .sdp
            .active_declarations(1.into(), &config.sdp_config.service_params);

        assert_eq!(
            *ledger.epoch_state().active_declarations,
            expected_for_epoch_0
        );
        assert_eq!(
            *ledger.next_epoch_state().active_declarations,
            expected_for_epoch_1
        );
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
        let mantle_tx = MantleTx(Ops::new_unchecked(ops.clone()));

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
                        inscription: [1, 2, 3, 4].into(),
                        parent: MsgId::root(),
                        signer: verifying_key,
                    }),
                    &Key::Ed25519(signing_key.clone()),
                ),
            )
            .unwrap()
            .0
    }

    #[expect(clippy::too_many_arguments, reason = "test fn")]
    fn apply_and_add_utxo_and_activity(
        ledger: &mut Ledger<HeaderId>,
        parent: HeaderId,
        slot: impl Into<Slot>,
        utxo_proof: Utxo,
        utxo_add: Utxo,
        declaration_id: DeclarationId,
        zk_key: ZkKey,
        nonce: Nonce,
    ) -> HeaderId {
        use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};

        let id = apply_and_add_utxo(ledger, parent, slot, utxo_proof, utxo_add);

        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let active_op = SDPActiveOp {
            declaration_id,
            nonce,
            metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
                // TODO: Create real proofs once the blend rewards module is enabled
                epoch: 0.into(),
                signing_key: signing_key.public_key(),
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]).into(),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([1; _]).into(),
            })),
        };
        let tx_hash = TxHash::from([1u8; 32]);
        let zk_sig = ZkKey::multi_sign(&[zk_key], &tx_hash.to_fr()).unwrap();
        let config = ledger.config().clone();
        let block_ledger = ledger.states.get_mut(&id).unwrap();
        block_ledger.mantle_ledger = block_ledger
            .mantle_ledger
            .clone()
            .try_apply_sdp_active(&active_op, &zk_sig, tx_hash, &config)
            .unwrap()
            .0;
        id
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
        let (_, state, events) = ledger
            .prepare_update::<_, MainnetGasConstants>(
                new_id,
                genesis_id,
                Slot::from(1u64),
                &proof,
                std::iter::once(&tx),
            )
            .unwrap();
        assert!(events.is_empty());
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
            inscription: [1, 2, 3, 4].into(),
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let tx = create_signed_tx(Op::ChannelInscribe(inscribe_op), &Key::Ed25519(signing_key));
        let result = state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _, events) = result.unwrap();
        assert!(
            new_state
                .mantle_ledger
                .channels()
                .channels
                .contains_key(&channel_id)
        );
        assert!(events.is_empty());
    }

    #[test]
    fn test_channel_config_operation() {
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([3; 32]);

        let config_op = ChannelConfigOp {
            channel: channel_id,
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            withdraw_threshold: 1,
        };

        let config_tx = MantleTx([Op::ChannelConfig(config_op.clone())].into());
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

        let (new_state, _, events) = result.unwrap();
        assert!(
            new_state
                .mantle_ledger
                .channels()
                .channels
                .contains_key(&channel_id)
        );
        assert_eq!(
            *new_state
                .mantle_ledger
                .channels()
                .channels
                .get(&channel_id)
                .unwrap()
                .accredited_keys,
            verifying_key.into()
        );
        assert!(events.is_empty());
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
            inputs: Inputs::new([utxo.id()]),
            metadata: [5, 6, 7, 8].into(),
        };
        let ops = vec![Op::ChannelDeposit(deposit.clone())];
        let tx = create_multi_signed_tx(ops, vec![&Key::Zk(sk)]);
        let result =
            ledger_state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, tx.clone());
        let (new_state, balance, events) = result.unwrap();
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

        assert_eq!(events.len(), 1);
        let Some(Event::Tx {
            tx_hash: event_tx_hash,
            op_id,
            payload:
                EventPayload::Deposit {
                    channel_id: event_channel_id,
                    amount,
                    metadata,
                },
        }) = events.iter().find(|event| {
            matches!(
                event,
                Event::Tx {
                    payload: EventPayload::Deposit { .. },
                    ..
                }
            )
        })
        else {
            panic!("events should include deposit event")
        };
        assert_eq!(*event_tx_hash, tx.hash());
        assert_eq!(*op_id, deposit.op_id());
        assert_eq!(*event_channel_id, deposit.channel_id);
        assert_eq!(*amount, utxo.note.value);
        assert_eq!(*metadata, deposit.metadata);
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
            inputs: Inputs::new([utxo.id()]),
            metadata: [5, 6, 7, 8].into(),
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
            outputs: Outputs::new([withdraw_note]),
            withdraw_nonce: 0,
        };
        let withdraw_tx = MantleTx([Op::ChannelWithdraw(withdraw.clone())].into());
        let withdraw_tx_hash = withdraw_tx.hash();
        let withdraw_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            signing_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.0.to_vec(),
            vec![&Key::MultiSequencer(withdraw_proof)],
        );

        let result =
            ledger_state.try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, signed_tx);
        assert!(result.is_ok());

        let (new_state, tx_balance, events) = result.unwrap();
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
        assert!(events.is_empty());
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
            inputs: Inputs::new([utxo.id()]),
            metadata: Metadata::empty(),
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
            outputs: Outputs::new([withdraw_note]),
            withdraw_nonce: 0,
        };
        let wrong_key = Ed25519Key::from_bytes(&[42; 32]);
        let withdraw_tx = MantleTx([Op::ChannelWithdraw(withdraw.clone())].into());
        let withdraw_tx_hash = withdraw_tx.hash();
        let invalid_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
            0,
            wrong_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.0.to_vec(),
            vec![&Key::MultiSequencer(invalid_proof), &Key::EmptyZk],
        );

        let err = ledger_state
            .clone()
            .try_apply_tx::<HeaderId, MainnetGasConstants>(&test_config, signed_tx)
            .unwrap_err();
        assert_eq!(
            err,
            LedgerError::VerificationError(
                VerificationError::ChannelMultiSigProofInvalidSignature {
                    op_index: 0,
                    signature_index: 0,
                }
            )
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
            inscription: [1, 2, 3].into(),
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
            inscription: [4, 5, 6].into(),
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
            inscription: [7, 8, 9].into(),
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
            inscription: [1, 2, 3].into(),
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
            inscription: [4, 5, 6].into(),
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
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: vk1,
        };

        let inscribe_op2 = InscriptionOp {
            channel_id: channel2,
            inscription: [4, 5, 6].into(),
            parent: MsgId::root(),
            signer: vk2,
        };

        let config_op = ChannelConfigOp {
            channel: channel1,
            keys: [vk3, vk4].into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            withdraw_threshold: 1,
        };

        let inscribe_op3 = InscriptionOp {
            channel_id: channel1,
            inscription: [7, 8, 9].into(),
            parent: config_op.id(),
            signer: vk3,
        };

        let ops = vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
            Op::ChannelConfig(config_op),
            Op::ChannelInscribe(inscribe_op3.clone()),
        ];
        let config_tx = MantleTx(Ops::new_unchecked(ops.clone()));
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

    /// Tests the snapshot-finalization-delay scenario behind the
    /// `inactivity_period >= SNAPSHOT_FINALIZATION_DELAY` invariant:
    ///
    ///   - A declaration is created at epoch 3 — `active = created +
    ///     SNAPSHOT_FINALIZATION_DELAY = 5`.
    ///   - An activity message accepted at epoch 6 refreshes the live SDP's
    ///     `active` to 6.
    ///   - But the snapshot for epoch 7 was built at the 5→6 transition, before
    ///     the activity message landed. The snapshot sees the decl with
    ///     `active=5`, not 6.
    ///
    /// With `inactivity_period = SNAPSHOT_FINALIZATION_DELAY = 2`, the
    /// filter at epoch 7 is `5 + 2 ≥ 7` → INCLUDED. The decl survives the
    /// finalization-delay gap.
    #[test]
    fn snapshot_includes_decl_when_active_refresh_lags_finalization() {
        let leader_utxo = utxo();
        let (sdp_utxo_key, sdp_utxo) = utxo_with_sk();
        let new_utxo_1 = utxo();
        let new_utxo_2 = utxo();
        let config = config();
        let epoch_length = config.epoch_length();
        let (mut ledger, genesis) = ledger(&[leader_utxo, sdp_utxo], config);

        // Declare at the first slot of epoch 3 — `active = 3 + 2 = 5`.
        let (h_3, declare, zk_key) = apply_and_add_utxo_and_declaration(
            &mut ledger,
            genesis,
            3 * epoch_length,
            leader_utxo,
            new_utxo_1,
            sdp_utxo,
            sdp_utxo_key,
        );

        // Advance to the first slot of epoch 6 and submit an Active message.
        // This sets the live SDP's `active` to 6.
        let h_6 = apply_and_add_utxo_and_activity(
            &mut ledger,
            h_3,
            6 * epoch_length,
            leader_utxo,
            new_utxo_2,
            declare.id(),
            zk_key,
            1,
        );

        // Advance to epoch 7. The snapshot for epoch 7 was built at the 5→6
        // transition, before the epoch-6 Active was applied.
        let h_7 = update_ledger(&mut ledger, h_6, 7 * epoch_length, leader_utxo).unwrap();
        assert_eq!(
            ledger.states[&h_7].cryptarchia_ledger.epoch_state.epoch,
            Epoch::new(7)
        );
        let decl = declaration_in_snapshot(&ledger, &h_7, &declare.id()).expect(
            "decl must be in the epoch-7 because inactivity_period >= SNAPSHOT_FINALIZATION_DELAY",
        );
        assert_eq!(
            decl.active,
            Epoch::new(5),
            "decl must have active=5 because the snapshot was taken at the end of epoch 5 before the activity message was accepted"
        );

        // Advance to epoch 8. The snapshot for epoch 8 was built at the 6→7
        // transition, after the epoch-6 Active was applied.
        let h_8 = update_ledger(&mut ledger, h_7, 8 * epoch_length, leader_utxo).unwrap();
        assert_eq!(
            ledger.states[&h_8].cryptarchia_ledger.epoch_state.epoch,
            Epoch::new(8)
        );
        let decl = declaration_in_snapshot(&ledger, &h_8, &declare.id()).expect(
            "decl must be in the epoch-7 because inactivity_period >= SNAPSHOT_FINALIZATION_DELAY",
        );
        assert_eq!(
            decl.active,
            Epoch::new(6),
            "decl must have active=6 because the snapshot was taken at the end of epoch 6 after the activity message was accepted"
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

        let err = ledger
            .try_apply_contents::<HeaderId, MainnetGasConstants>(&config, std::iter::once(&tx))
            .unwrap_err();
        // The transaction should be rejected because the price indicated for execution
        // doesn't cover the base fee that cost 27 050
        assert_eq!(err, LedgerError::InsufficientBalance);
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
        let (no_priority_fee_ledger, events) = result.unwrap();
        assert!(events.is_empty());

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
        let (priority_fee_ledger, events) = result.unwrap();

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
        assert!(events.is_empty());
    }

    #[test]
    fn test_leader_claim_operation() {
        let leaders = LeaderState::new();
        // Add 3 vouchers (blocks) at epoch 1
        let leaders = leaders.try_apply_header(1.into(), Fr::ZERO.into()).unwrap();
        let leaders = leaders.try_apply_header(1.into(), Fr::ONE.into()).unwrap();
        let leaders = leaders
            .try_apply_header(1.into(), Fr::from(2u64).into())
            .unwrap();
        // Advance to epoch 2 by adding a voucher (block)
        let mut leaders = leaders
            .try_apply_header(2.into(), Fr::from(3u64).into())
            .unwrap();
        // Set rewards to 300 which can be distributed to the 3 vouchers
        // collected so far (during epoch 1).
        leaders.update_rewards(300);

        // For each of the 3 vouchers, claim the reward.
        for nf in [Fr::ZERO, Fr::ONE, Fr::from(2u64)] {
            assert_eq!(leaders.reward_amount(), 100);
            let op = LeaderClaimOp {
                rewards_root: leaders.vouchers_snapshot_root(),
                voucher_nullifier: nf.into(),
                pk: ZkPublicKey::zero(),
            };
            // Skip `op.validate` in this test to avoid having to generate a valid proof
            let (result, _events) = op
                .execute(LeaderClaimExecutionContext {
                    nullifiers: leaders.nullifiers_cloned(),
                    reward_amount: leaders.reward_amount(),
                    claimable_rewards: leaders.claimable_rewards(),
                    utxos: Utxos::new(),
                })
                .unwrap();
            leaders.update_nullifiers(result.nullifiers);
            leaders.update_rewards(result.claimable_rewards);

            assert_eq!(result.utxos.size(), 1);
            let (_, (utxo, _)) = result.utxos.utxos().iter().next().unwrap();
            assert_eq!(utxo.note.value, 100);
        }

        // All rewards have been claimed.
        assert_eq!(leaders.claimable_rewards(), 0);
    }

    #[test]
    fn test_duplicate_leader_claim_is_rejected() {
        let leaders = LeaderState::new();
        // Add a voucher (block) at epoch 1
        let leaders = leaders.try_apply_header(1.into(), Fr::ZERO.into()).unwrap();
        // Advance to epoch 2 by adding a voucher (block)
        let mut leaders = leaders.try_apply_header(2.into(), Fr::ONE.into()).unwrap();
        // Set rewards to 100 which can be distributed to the vouchers
        // collected so far (during epoch 1).
        leaders.update_rewards(100);

        // Claim the reward for the 1st voucher.
        let op = LeaderClaimOp {
            rewards_root: leaders.vouchers_snapshot_root(),
            voucher_nullifier: Fr::ZERO.into(), // nf of the 1st voucher
            pk: ZkPublicKey::zero(),
        };
        // Skip `op.validate` in this test to avoid having to generate a valid proof
        let (result, _events) = op
            .execute(LeaderClaimExecutionContext {
                nullifiers: leaders.nullifiers_cloned(),
                reward_amount: leaders.reward_amount(),
                claimable_rewards: leaders.claimable_rewards(),
                utxos: Utxos::new(),
            })
            .unwrap();
        leaders.update_nullifiers(result.nullifiers);
        leaders.update_rewards(result.claimable_rewards);
        assert_eq!(result.utxos.size(), 1);
        let (_, (utxo, _)) = result.utxos.utxos().iter().next().unwrap();
        assert_eq!(utxo.note.value, 100);

        // Try to claim the reward using the same nullifier.
        let err = op
            .validate(&LeaderClaimValidationContext {
                nullifiers: leaders.nullifiers(),
                claimable_vouchers_root: &leaders.vouchers_snapshot_root(),
                // Use a dummy proof since duplication is detected before proof verification
                proof_of_claim: &Groth16LeaderClaimProof::new(
                    CompressedGroth16Proof::from_bytes(&[0u8; 128]),
                    Fr::ZERO.into(),
                ),
                tx_hash: &TxHash::from([0u8; 32]),
            })
            .unwrap_err();
        assert_eq!(err, LeaderClaimError::DuplicatedVoucherNullifier);
    }
}
