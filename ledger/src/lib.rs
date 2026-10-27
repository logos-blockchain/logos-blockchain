pub mod block_rewards;
pub mod config;
// The ledger is split into two modules:
// - `cryptarchia`: the base functionalities needed by the Cryptarchia consensus
//   algorithm, including a minimal UTxO model.
// - `mantle_ops`: our extensions in the form of Mantle operations, e.g. SDP.
pub mod cryptarchia;
mod gas_and_fees;
mod intent;
pub mod mantle;
mod update;

use std::hash::Hash;

pub use config::{Config, ConsensusExcludedKeys};
pub use cryptarchia::EpochState;
use cryptarchia::LedgerState as CryptarchiaLedger;
#[cfg(test)]
use gas_and_fees::EXECUTION_GAS_LIMIT;
pub use gas_and_fees::GasAndFees;
pub use intent::{Intent, IntentStatus};
use lb_core::{
    block::BlockNumber,
    crypto::Hash as BlockHash,
    events::{BlockEvent, Events, HeaderEvent, TxEvent, TxEventPayload},
    mantle::{
        Value, VerificationError,
        batch::DeferredProofs,
        gas::{Gas, GasCost, GasOverflow, GasProfile},
        ledger::verification_mode::StandardMode,
        note_set::{LEDGER_SET, NoteSetError, NoteSets},
        notes,
        ops::{
            SignedOp,
            channel::{deposit::DepositExecutionContext, withdraw::WithdrawExecutionContext},
            pow::ClaimPoWRewardExecutionContext,
            program::ProgramRegisterExecutionContext,
            sdp::{SDPActiveOp, SDPDeclareOp, SdpError},
        },
        traits::{GenesisTx, PreverifiedMantleTransaction, StorageSize, genesis::GenesisOps},
        transactions::{
            GasPrices,
            hash::{TxHash, TxHashView},
            states::Verified,
            tx_list::ops::{OpsContext, OpsGasContext},
        },
    },
    proofs::leader_proof,
};
use lb_cryptarchia_engine::{Slot, UncleSlots};
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_log_targets::{diagnostic::BLEND_REACHABILITY, ledger};
use mantle::LedgerState as MantleLedger;
use rpds::HashTrieMapSync;
use thiserror::Error;

use crate::{
    block_rewards::BlockRewardsError,
    config::RewardPoWConfig,
    mantle::helpers::MantleOperationVerificationHelper,
    update::{BatchVerifiedUpdate, PreparedUpdate},
};

const LOG_TARGET: &str = ledger::ROOT;

// While individual notes are constrained to be `u64`, intermediate calculations
// may overflow, so we use `i128` to avoid that and to easily represent negative
// balances which may arise in special circumstances (e.g. rewards calculation).
pub type Balance = i128;

// What applying one transaction yields: the new state, the transaction balance,
// the execution gas its operations consumed, the events and the deferred
// proofs.
type AppliedTxOutcome = (LedgerState, Balance, Gas, Vec<TxEvent>, DeferredProofs);

// Mantle Validation step 2: the excess of TRANSFER, CHANNEL_DEPOSIT and
// SDP_WITHDRAW adds to a checked uint64 balance.
fn add_excess<Id>(balance: Balance, excess_value: Value) -> Result<Balance, LedgerError<Id>> {
    balance
        .checked_add(Balance::from(excess_value))
        .filter(|sum| *sum <= Balance::from(Value::MAX))
        .ok_or(LedgerError::BalanceOverflow)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LedgerError<Id> {
    #[error("Invalid block slot {block:?} for parent slot {parent:?}")]
    InvalidSlot { parent: Slot, block: Slot },
    #[error("Parent block could not be found: {0:?}")]
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
    #[error("Input note in genesis block: {0:?}")]
    InputInGenesis(notes::NoteNullifier),
    #[error("The first Transfer Operation is missing in genesis tx")]
    MissingTransferGenesis(),
    #[error("Fees don't cover the minimal execution base fee cost")]
    InsufficientExecutionFee,
    #[error("The execution gas of the transaction ({gas:?}) exceeds the block limit ({limit:?})")]
    TooMuchTransactionExecutionGas { gas: Gas, limit: Gas },
    #[error("The execution gas of the block ({gas:?}) exceeds the maximum limit ({limit:?})")]
    TooMuchExecutionGas { gas: Gas, limit: Gas },
    #[error("Storage fees aren't equal to the storage fee of the current epoch")]
    InvalidStoragePrice,
    #[error("Verification error: {0}")]
    VerificationError(#[from] VerificationError),
    #[error("Note set error: {0}")]
    NoteSet(#[from] NoteSetError),
    #[error(transparent)]
    BlockRewards(#[from] BlockRewardsError),
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
    pub fn new(id: Id, mut state: LedgerState, config: Config) -> Self
    where
        Id: Into<BlockHash>,
    {
        // Record the root block among the recently seen blocks, so early
        // `PoW` reward claims can anchor to it. Later blocks are recorded
        // as they are applied in `try_update`.
        state
            .mantle_ledger
            .add_seen_block(id.into(), state.slot(), &config);
        Self {
            states: HashTrieMapSync::new_sync().insert(id, state),
            config,
        }
    }

    /// Prepare to add a new [`LedgerState`] by applying the given proof and
    /// transactions on top of the parent state.
    ///
    /// On success, a new [`LedgerState`] is returned, which can then be
    /// committed by calling [`Self::commit_update`].
    pub fn prepare_update<Tx, LeaderProof, Profile>(
        &self,
        id: Id,
        parent_id: Id,
        slot: Slot,
        proof: &LeaderProof,
        uncle_slots: &UncleSlots,
        txs: impl Iterator<Item = Tx>,
    ) -> Result<PreparedUpdate<Id>, LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction + StorageSize + Clone,
        LeaderProof: leader_proof::LeaderProof,
        Profile: GasProfile,
        Id: Into<BlockHash>,
    {
        let parent_state = self
            .states
            .get(&parent_id)
            .ok_or(LedgerError::ParentNotFound(parent_id))?;

        let (new_state, events, deferred_proofs) = parent_state
            .clone()
            .try_update::<_, _, _, Profile>(id, slot, proof, uncle_slots, txs, &self.config)?;

        Ok(PreparedUpdate::new(id, new_state, events, deferred_proofs))
    }

    /// Commits a new [`LedgerState`] created by [`Self::prepare_update`].
    pub fn commit_update(&mut self, update: BatchVerifiedUpdate<Id>) -> Events {
        self.states.insert_mut(update.id, update.state);
        update.events
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
    fn try_update<Tx, LeaderProof, Id, Profile>(
        self,
        block_id: Id,
        slot: Slot,
        proof: &LeaderProof,
        uncle_slots: &UncleSlots,
        txs: impl Iterator<Item = Tx>,
        config: &Config,
    ) -> Result<(Self, Events, DeferredProofs), LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction + StorageSize + Clone,
        LeaderProof: leader_proof::LeaderProof,
        Profile: GasProfile,
        Id: Into<BlockHash>,
    {
        let (mut state, header_events) = self.try_apply_header(slot, proof, uncle_slots, config)?;
        // Record the applied block among the recently seen blocks `PoW`
        // claims may anchor to. This is the canonical apply path, where the
        // block's id is known — unlike a proposer's direct
        // `try_apply_header` call for a block still being built.
        let block_hash: BlockHash = block_id.into();
        state.mantle_ledger.add_seen_block(block_hash, slot, config);
        // Count the block's transactions into the epoch totals the Blend `PoW`
        // difficulty is retargeted from, for the same reason as above: only a
        // block that is actually applied, contents included, belongs in the
        // epoch's average.
        let mut txs_in_block = 0u64;
        let (mut state, tx_events, block_events, deferred_proofs) = state
            .try_apply_block_contents::<_, _, Profile>(
                config,
                *proof.reward_key(),
                txs.inspect(|_| txs_in_block = txs_in_block.saturating_add(1)),
            )?;
        state.mantle_ledger.pow.record_block_txs(txs_in_block);
        state.update_pow_reward_difficulty(
            // count all claimed rewards
            tx_events
                .iter()
                .filter(|TxEvent { payload, .. }| {
                    matches!(payload, TxEventPayload::PoWRewardClaimed { .. })
                })
                .count() as u64,
            config,
        );
        let events = header_events
            .into_iter()
            .map(Into::into)
            .chain(tx_events.into_iter().map(Into::into))
            .chain(block_events.into_iter().map(Into::into))
            .collect::<Events>();
        Ok((state, events, deferred_proofs))
    }

    /// Apply header-related changed to the ledger state. These include
    /// leadership and in general any changes that not related to
    /// transactions that should be applied before that.
    ///
    /// Returns any [`HeaderEvent`]s emitted while processing the header.
    pub fn try_apply_header<LeaderProof, Id>(
        self,
        slot: Slot,
        proof: &LeaderProof,
        uncle_slots: &UncleSlots,
        config: &Config,
    ) -> Result<(Self, Vec<HeaderEvent>), LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        let last_epoch_state = self.cryptarchia_ledger.epoch_state().clone();
        let mut cryptarchia_ledger = self
            .cryptarchia_ledger
            .try_apply_header::<LeaderProof, Id>(
                slot,
                proof,
                uncle_slots,
                // TODO: threading SDP here because EpochState is currently embedded in
                // CryptarchiaLedger.
                // In the future, we will pull EpochState up into LedgerState.
                &self.mantle_ledger.sdp,
                &self.mantle_ledger.pow,
                config,
            )?;
        let (mantle_ledger, effect) = self.mantle_ledger.try_apply_header(
            &last_epoch_state,
            cryptarchia_ledger.epoch_state(),
            config,
        )?;

        cryptarchia_ledger = cryptarchia_ledger
            .pay_service_rewards::<Id>(&effect.reward_notes, effect.settled_rewards)?;

        // SDP Epoch Finalization: the outputs of the removed withdrawals enter the
        // ledger note set.
        if !effect.released_outputs.is_empty() {
            let ledger_set = cryptarchia_ledger
                .note_sets
                .get(LEDGER_SET)
                .expect("the ledger note set always exists")
                .clone()
                .execute_adding(&effect.released_outputs)
                .map_err(|error| mantle::Error::Sdp(SdpError::NoteSet(error).into()))?;
            cryptarchia_ledger.note_sets =
                cryptarchia_ledger.note_sets.update(LEDGER_SET, ledger_set);
        }

        // Block Execution step 2: the channel withdrawals due at this slot, before the
        // transactions.
        let (cryptarchia_ledger, released) = cryptarchia_ledger.release_withdrawals(slot)?;
        let mut events = effect.events;
        events.extend(released.into_iter().map(|withdrawal| {
            HeaderEvent::ChannelWithdrawalReleased {
                note_set: withdrawal.note_set,
                inputs: withdrawal.inputs,
                outputs: withdrawal.outputs,
            }
        }));

        Ok((
            Self {
                block_number: self
                    .block_number
                    .checked_add(1)
                    .expect("Logos blockchain lived long and prospered"),
                cryptarchia_ledger,
                mantle_ledger,
            },
            events,
        ))
    }

    /// Verifies a leadership proof for a block at `slot` whose parent is the
    /// block this state belongs to, leaving the state untouched.
    pub fn verify_proof_of_leadership<LeaderProof, Id>(
        &self,
        slot: Slot,
        proof: &LeaderProof,
        config: &Config,
    ) -> Result<(), LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        self.cryptarchia_ledger.verify_proof_of_leadership(
            slot,
            proof,
            &self.mantle_ledger.sdp,
            &self.mantle_ledger.pow,
            config,
        )
    }

    #[must_use]
    pub const fn get_gas_prices(&self) -> GasPrices {
        GasPrices {
            execution_base_gas_price: *self.cryptarchia_ledger.execution_base_fee(),
            storage_gas_price: *self.cryptarchia_ledger.storage_gas_price(),
        }
    }

    /// total estimated stake and on the average of fees consumed per block over
    /// the last `POOLED_FEES_WINDOW` blocks. See the block rewards
    /// specification: <https://lip.logos.co/blockchain/raw/block-rewards.html>
    fn compute_block_rewards<Id>(
        mut self,
        total_fee_burned: GasCost,
        total_fee_tip: GasCost,
        reward_key: ZkPublicKey,
        reward_config: &RewardPoWConfig,
    ) -> Result<(Self, notes::Note), LedgerError<Id>> {
        // Divert the PoW share of the collected fees to the PoW reward pool
        // before the rest is pooled for block rewards. See the proof of work
        // specification: <https://lip.logos.co/blockchain/raw/proof-of-work.html>
        let pow_refill = GasCost::from(reward_config.pow_fee_share(total_fee_burned.into_inner()));
        let pooled_fees = total_fee_burned.checked_sub(pow_refill)?;

        let (cryptarchia_ledger, blend_share, leader_note) =
            self.cryptarchia_ledger.distribute_block_reward(
                pooled_fees.into_inner(),
                total_fee_tip.into_inner(),
                reward_key,
            )?;
        self.cryptarchia_ledger = cryptarchia_ledger;

        self.mantle_ledger.sdp.add_blend_income(blend_share);
        self.mantle_ledger
            .pow
            .add_reward_refill_rewards(pow_refill.into_inner())
            .map_err(mantle::Error::from)?;

        Ok((self, leader_note))
    }

    /// For each block received, execution base fees and average execution
    /// consumption are updated based on the total execution gas consumed in the
    /// block and the smoothed average consumption. This function updates the
    /// `average_execution_gas` and the `execution_base_fee` stored in the
    /// cryptarchia ledger. See the specification <https://lip.logos.co/blockchain/raw/execution-market.html>
    fn update_execution_market(self, block_execution_gas_consumed: Gas) -> Self {
        Self {
            cryptarchia_ledger: self
                .cryptarchia_ledger
                .update_execution_market(block_execution_gas_consumed),
            ..self
        }
    }

    /// Accumulates the storage gas consumed by the block into the current
    /// epoch's counter, which drives the storage price update at the next
    /// epoch rotation.
    fn add_storage_gas_consumed<Id>(
        self,
        block_storage_gas_consumed: Gas,
    ) -> Result<Self, LedgerError<Id>> {
        Ok(Self {
            cryptarchia_ledger: self
                .cryptarchia_ledger
                .add_storage_gas_consumed(block_storage_gas_consumed)?,
            ..self
        })
    }

    /// Validates and applies one transaction to the current ledger state.
    ///
    /// Returns the updated state, gas and fees, emitted events, and
    /// deferred proof verifications. Block-wide accounting is not finalized.
    pub fn try_apply_transaction<Tx, Id, Profile: GasProfile>(
        self,
        config: &Config,
        tx: &Tx,
    ) -> Result<(Self, GasAndFees, Vec<TxEvent>, DeferredProofs), LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction + StorageSize + Clone,
    {
        let (ledger_state, balance, execution_gas, events, deferred_proofs) =
            self.try_apply_tx_operations::<_, _, Profile>(config, tx.clone())?;

        let gas_prices = ledger_state.get_gas_prices();
        let storage_gas = Gas::new(tx.storage_size() as u64);
        let execution_base_fees =
            GasCost::calculate(execution_gas, gas_prices.execution_base_gas_price)?;
        let permanent_storage_fees = GasCost::calculate(storage_gas, gas_prices.storage_gas_price)?;
        let total_gas_cost = execution_base_fees.checked_add(permanent_storage_fees)?;
        tracing::debug!(
            target: LOG_TARGET,
            balance,
            total_gas_cost = total_gas_cost.into_inner(),
            storage_gas_price = ?gas_prices.storage_gas_price,
            execution_gas_price = ?gas_prices.execution_base_gas_price,
            "tx balance check"
        );

        if balance < Balance::from(total_gas_cost.into_inner()) {
            return Err(LedgerError::InsufficientBalance);
        }

        let fee_burned = total_gas_cost;
        let fee_tip = GasCost::from(balance as Value).checked_sub(fee_burned)?;
        let gas_and_fees = GasAndFees {
            execution_gas,
            storage_gas,
            fee_burned,
            fee_tip,
        };

        Ok((ledger_state, gas_and_fees, events, deferred_proofs))
    }

    /// Applies all transactions in a block and finalizes block-wide accounting.
    #[expect(clippy::type_complexity, reason = "the block outcome tuple")]
    pub fn try_apply_block_contents<Tx, Id, Profile: GasProfile>(
        mut self,
        config: &Config,
        reward_key: ZkPublicKey,
        txs: impl Iterator<Item = Tx>,
    ) -> Result<(Self, Vec<TxEvent>, Vec<BlockEvent>, DeferredProofs), LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction + StorageSize + Clone,
    {
        let mut gas_and_fees = GasAndFees::default();
        let mut tx_events = Vec::new();
        let mut deferred_proofs = DeferredProofs::new();

        for tx in txs {
            let (next_state, tx_gas_and_fees, events, deferred) =
                self.try_apply_transaction::<_, Id, Profile>(config, &tx)?;
            gas_and_fees = gas_and_fees.checked_add::<Id>(tx_gas_and_fees)?;
            self = next_state;
            tx_events.extend(events);
            deferred_proofs.extend(deferred);
        }

        // Compute Block rewards and give tips
        let leader_note;
        (self, leader_note) = self.compute_block_rewards(
            gas_and_fees.fee_burned,
            gas_and_fees.fee_tip,
            reward_key,
            &config.pow_config.reward,
        )?;
        let (cryptarchia_ledger, updates) = self
            .cryptarchia_ledger
            .end_block_with_updates(self.block_number);
        self.cryptarchia_ledger = cryptarchia_ledger;
        let block_events = std::iter::once(BlockEvent::LeaderRewardPaid { note: leader_note })
            .chain(updates.into_iter().map(BlockEvent::NoteSetUpdated))
            .collect();
        // Update Execution market state
        self = self.update_execution_market(gas_and_fees.execution_gas);
        // Accumulate storage gas consumed so the storage market can update the
        // price at the next epoch rotation.
        self = self.add_storage_gas_consumed(gas_and_fees.storage_gas)?;
        Ok((self, tx_events, block_events, deferred_proofs))
    }

    pub fn from_notes(notes: impl IntoIterator<Item = notes::Note>, config: &Config) -> Self {
        let cryptarchia_ledger = CryptarchiaLedger::from_notes(notes, config, Fr::ZERO);
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
    ) -> Result<(Self, Vec<TxEvent>), LedgerError<Id>> {
        let total_stake = CryptarchiaLedger::genesis_total_stake(&tx.distribution(), config)?;
        let tx_hash_view = TxHashView::from(tx.hash());
        let GenesisOps {
            transfer,
            inscription,
            distribution_inscription,
            declarations,
        } = tx.into_genesis_ops();
        let cryptarchia_ledger =
            CryptarchiaLedger::from_genesis_tx(&transfer, total_stake, config, epoch_nonce)?;
        let (mantle_ledger, note_sets, events) = MantleLedger::from_genesis_tx(
            [inscription, distribution_inscription],
            declarations,
            &tx_hash_view,
            config,
            cryptarchia_ledger.latest_note_sets().clone(),
            cryptarchia_ledger.epoch_state(),
        )?;
        // Once the whole genesis transaction is executed every note set records its
        // root, and the epoch states and the Blend inputs are taken from the result.
        let cryptarchia_ledger = cryptarchia_ledger.with_genesis_note_sets(note_sets.end_block(0));
        let mantle_ledger =
            mantle_ledger.with_genesis_epoch_state(config, cryptarchia_ledger.epoch_state());
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
        self.cryptarchia_ledger.epoch_state_for_slot(
            slot,
            &self.mantle_ledger.sdp,
            &self.mantle_ledger.pow,
            config,
        )
    }

    #[must_use]
    pub const fn latest_note_sets(&self) -> &NoteSets {
        self.cryptarchia_ledger.latest_note_sets()
    }

    #[must_use]
    pub const fn aged_note_sets(&self) -> &NoteSets {
        self.cryptarchia_ledger.aged_note_sets()
    }

    #[must_use]
    pub const fn mantle_ledger(&self) -> &MantleLedger {
        &self.mantle_ledger
    }

    #[must_use]
    pub fn tx_context(&self) -> OpsContext {
        OpsContext {
            gas_context: OpsGasContext::from_channels(
                self.mantle_ledger().channels(),
                self.get_gas_prices(),
            ),
        }
    }

    fn log_sdp_declaration_evaluation(
        &self,
        op: &SDPDeclareOp,
        result: &MantleLedger,
        config: &Config,
    ) {
        if !tracing::enabled!(tracing::Level::TRACE) {
            return;
        }
        let Some(declaration) = result.sdp_ledger().get_declaration(&op.id()) else {
            return;
        };
        let inactivity_period = config
            .sdp_config
            .service_params
            .get(&declaration.service_type)
            .map_or(0, |parameters| {
                parameters.inactivity_period.into_inner().into_inner()
            });
        tracing::trace!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "sdp_declaration_applied",
            evaluation_context = "candidate",
            canonical = false,
            provider_id = ?declaration.provider_id,
            declaration_id = ?op.id(),
            ledger_epoch = u32::from(self.cryptarchia_ledger.epoch_state().epoch),
            ledger_slot = u64::from(self.cryptarchia_ledger.slot()),
            initial_active_epoch = u32::from(declaration.active),
            inactivity_period,
            "Evaluated SDP declaration on a candidate block"
        );
    }

    fn log_sdp_activity_evaluation(
        &self,
        op: &SDPActiveOp,
        result: &MantleLedger,
        config: &Config,
        tx_hash: &TxHash,
        previous_active_epoch: Option<lb_cryptarchia_engine::Epoch>,
    ) {
        if !tracing::enabled!(tracing::Level::TRACE) {
            return;
        }
        let Some(new_declaration) = result.sdp_ledger().get_declaration(&op.declaration_id) else {
            return;
        };
        let slot = self.cryptarchia_ledger.slot();
        let epoch = self.cryptarchia_ledger.epoch_state().epoch;
        let inactivity_period = config
            .sdp_config
            .service_params
            .get(&new_declaration.service_type)
            .map_or(0, |parameters| {
                parameters.inactivity_period.into_inner().into_inner()
            });
        tracing::trace!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "sdp_activity_applied",
            evaluation_context = "candidate",
            canonical = false,
            proof_epoch = u32::from(op.metadata.origin_epoch()),
            ledger_epoch = u32::from(epoch),
            ledger_slot = u64::from(slot),
            tx_id = %tx_hash,
            provider_id = ?new_declaration.provider_id,
            declaration_id = %op.declaration_id,
            previous_active_epoch = ?previous_active_epoch,
            new_active_epoch = ?new_declaration.active,
            active_until_epoch = u32::from(new_declaration.active).saturating_add(inactivity_period),
            "Evaluated SDP activity message on a candidate block"
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This will be refactored in an upcoming PR."
    )]
    fn try_apply_op<Id, Profile: GasProfile>(
        mut self,
        signed_op: SignedOp<Verified, StandardMode>,
        config: &Config,
        tx_hash: &TxHash,
        mut balance: Balance,
        mut tx_events: Vec<TxEvent>,
    ) -> Result<(Self, Balance, Vec<TxEvent>), LedgerError<Id>> {
        match signed_op {
            SignedOp::ChannelInscribe(signed_operation) => {
                let (result, note_sets, events) =
                    self.mantle_ledger.try_apply_channel_inscription(
                        signed_operation,
                        self.cryptarchia_ledger.latest_note_sets().clone(),
                        self.cryptarchia_ledger.slot,
                    )?;
                self.mantle_ledger = result;
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_note_sets(note_sets);
                tx_events.extend(events);
            }
            SignedOp::ChannelConfig(signed_operation) => {
                let (result, note_sets, events) = self.mantle_ledger.try_apply_channel_config(
                    signed_operation,
                    self.cryptarchia_ledger.latest_note_sets().clone(),
                    self.cryptarchia_ledger.slot,
                )?;
                self.mantle_ledger = result;
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_note_sets(note_sets);
                tx_events.extend(events);
            }
            SignedOp::ChannelDeposit(signed_operation) => {
                let excess_value = signed_operation.operation().excess_value;
                let (result, events) = signed_operation
                    .execute(DepositExecutionContext {
                        channels: self.mantle_ledger.channels(),
                        note_sets: self.cryptarchia_ledger.latest_note_sets().clone(),
                        tx_hash: *tx_hash,
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::Channel(error))?;
                self.cryptarchia_ledger =
                    self.cryptarchia_ledger.update_note_sets(result.note_sets);
                balance = add_excess(balance, excess_value)?;
                tx_events.extend(events);
            }
            SignedOp::ChannelWithdraw(signed_operation) => {
                let (result, events) = signed_operation
                    .execute(WithdrawExecutionContext {
                        channels: self.mantle_ledger.channels(),
                        pending_withdrawals: self.cryptarchia_ledger.pending_withdrawals.clone(),
                        block_slot: self.cryptarchia_ledger.slot,
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::Channel(error))?;
                self.cryptarchia_ledger.pending_withdrawals = result.pending_withdrawals;
                tx_events.extend(events);
            }
            SignedOp::SDPDeclare(signed_operation) => {
                let operation = signed_operation.operation().clone();
                let (mantle_ledger, note_sets, events) =
                    self.mantle_ledger.try_apply_sdp_declaration(
                        signed_operation,
                        self.cryptarchia_ledger.latest_note_sets().clone(),
                        config,
                    )?;
                self.mantle_ledger = mantle_ledger;
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_note_sets(note_sets);
                self.log_sdp_declaration_evaluation(&operation, &self.mantle_ledger, config);
                tx_events.extend(events);
            }
            SignedOp::SDPActive(signed_operation) => {
                let operation = signed_operation.operation().clone();
                let previous_active_epoch = tracing::enabled!(tracing::Level::TRACE).then(|| {
                    self.mantle_ledger
                        .sdp_ledger()
                        .get_declaration(&operation.declaration_id)
                        .map(|declaration| declaration.active)
                });
                let (mantle_ledger, events) = self
                    .mantle_ledger
                    .try_apply_sdp_active(signed_operation, config)?;
                self.mantle_ledger = mantle_ledger;
                self.log_sdp_activity_evaluation(
                    &operation,
                    &self.mantle_ledger,
                    config,
                    tx_hash,
                    previous_active_epoch.flatten(),
                );
                tx_events.extend(events);
            }
            SignedOp::SDPWithdraw(signed_operation) => {
                let excess_value = signed_operation.operation().excess_value;
                let (result, note_sets, events) = self.mantle_ledger.try_apply_sdp_withdraw(
                    signed_operation,
                    self.cryptarchia_ledger.latest_note_sets().clone(),
                    config,
                )?;
                self.mantle_ledger = result;
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_note_sets(note_sets);
                balance = add_excess(balance, excess_value)?;
                tx_events.extend(events);
            }
            SignedOp::Transfer(signed_operation) => {
                let transfer_excess;
                let events;
                (self.cryptarchia_ledger, transfer_excess, events) =
                    self.cryptarchia_ledger
                        .try_apply_transfer::<_, Profile>(signed_operation)?;
                balance = add_excess(balance, transfer_excess)?;
                tx_events.extend(events);
            }
            SignedOp::ClaimPowReward(signed_operation) => {
                let (result, events) = signed_operation
                    .execute(ClaimPoWRewardExecutionContext {
                        reward_pool: self.mantle_ledger.pow.reward_pool(),
                        // TODO: check correctness of epoch reward, as it should be from the op
                        // specified epoch
                        epoch_reward: self.mantle_ledger.pow.epoch_reward(),
                        nullifiers: self.mantle_ledger.pow.nullifiers().clone(),
                        tx_hash: *tx_hash,
                        note_sets: self.cryptarchia_ledger.latest_note_sets().clone(),
                        block_slots: self.mantle_ledger.pow.block_slots().clone(),
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::ClaimPow(error))?;
                self.mantle_ledger
                    .pow
                    .update_from_claim_execution_result(&result);
                self.cryptarchia_ledger =
                    self.cryptarchia_ledger.update_note_sets(result.note_sets);
                tx_events.extend(events);
            }
            SignedOp::ProgramRegister(signed_operation) => {
                let (result, events) = signed_operation
                    .execute(ProgramRegisterExecutionContext {
                        programs: self.mantle_ledger.programs().clone(),
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::Program(error))?;
                self.mantle_ledger = self.mantle_ledger.update_programs(result.programs);
                tx_events.extend(events);
            }
        }

        Ok((self, balance, tx_events))
    }

    /// Applies a transaction to the ledger state.
    ///
    /// # Note
    ///
    /// Verification is interleaved with execution: each operation is verified
    /// against the current ledger state (via
    /// [`SignedOps::verified_ops`]) immediately before it is executed,
    /// so an operation may depend on state produced by earlier operations
    /// in the same transaction.
    ///
    /// # Returns
    ///
    /// On success, returns the updated ledger state, the net balance change of
    /// the transaction, and any events emitted during execution.
    ///
    /// If any operation fails verification or execution, returns a
    /// [`LedgerError`] describing the failure.
    fn try_apply_tx_operations<Tx, Id, Profile: GasProfile>(
        mut self,
        config: &Config,
        tx: Tx,
    ) -> Result<AppliedTxOutcome, LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction,
    {
        // The buffers start empty with every Mantle Transaction.
        self.cryptarchia_ledger = self.cryptarchia_ledger.start_transaction();
        let mut verified_operations = tx.into_verified_operations();

        let mut balance: Balance = 0;
        let mut execution_gas = Gas::new(0);
        let mut tx_events = Vec::new();
        let mut deferred_proofs = DeferredProofs::new();

        loop {
            let helper = MantleOperationVerificationHelper::new(
                &self.mantle_ledger,
                &self.cryptarchia_ledger,
                self.block_number,
                config,
            );

            let Some((remaining_verified_operations, signed_op)) = verified_operations
                .next(&helper, &mut deferred_proofs)
                .transpose()?
            else {
                // All operations have been processed, exit the loop.
                break;
            };
            verified_operations = remaining_verified_operations;

            // Price the operation against the state it was just verified
            // against, before executing it.
            execution_gas = execution_gas.checked_add(
                signed_op
                    .operation()
                    .execution_gas::<Profile>(self.mantle_ledger.channels())?,
            )?;

            (self, balance, tx_events) = self.try_apply_op::<_, Profile>(
                signed_op,
                config,
                verified_operations.tx_hash_view().tx_hash(),
                balance,
                tx_events,
            )?;
        }

        Ok((self, balance, execution_gas, tx_events, deferred_proofs))
    }

    fn update_pow_reward_difficulty(&mut self, claims_in_block: u64, config: &Config) {
        self.mantle_ledger
            .pow
            .update_difficulty(claims_in_block, &config.pow_config.reward);
    }
}

#[cfg(test)]
mod tests {
    use cryptarchia::tests::{config, generate_proof};
    use lb_binary_codec::canonical::BinaryEncode as _;
    use lb_core::{
        block::genesis::GenesisBlockBuilder,
        mantle::{
            CryptarchiaParameter, GenesisTime, Op, OpProof, SignedOps,
            channel::Channels,
            gas::{MainnetGasProfile, TxGasCalculator as _},
            note_set::{CommitmentMmr, LEDGER_SET, NoteSet, NoteSetUpdate, SDP_SET},
            notes::{
                InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments,
                derive_note_nonce, derive_note_nullifier,
            },
            ops::{
                ChannelInscribeProof, OpId as _, SignedOperation, StepProof, ZkAndEd25519Proof,
                ZkTransferProof,
                channel::{
                    ChannelId, MsgId, NULL_CHANNEL_ID,
                    config::ChannelConfigOp,
                    deposit::{DepositOp, Metadata},
                    inscribe::{ChannelStep, ChannelSteps, Inscription, InscriptionOp, Step},
                    program_step::{Payment, ProgramOutputs, ProgramStep},
                    withdraw::{
                        ChannelWithdrawOp, PendingWithdrawal, PendingWithdrawals, WITHDRAW_DELAY,
                    },
                },
                op_proof::samples::SampleProof as _,
                program::ProgramRegisterOp,
                sdp::SDPWithdrawOp,
                transfer::{TransferError, TransferOp, test_utils::prove_transfer},
            },
            programs::{ProgramVerificationKey, RegisteredProgram, derive_program_id},
            traits::{GenesisTx as _, Hashable as _},
            transactions::{
                Distribution, GenesisAllocation, GenesisTx, OpProofs, Ops,
                states::{Preverified, Unverified},
            },
        },
        proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
        sdp::{ActivityMetadata, DeclarationId, Nonce},
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{CompressedGroth16Proof, Field as _};
    use lb_key_management_system_keys::keys::{
        Ed25519Key, Ed25519PublicKey, Ed25519Signature, UnverifiedEd25519PublicKey, ZkKey,
        ZkPublicKey,
    };
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        block_rewards::POOLED_FEES_WINDOW,
        config::ModulusShift,
        cryptarchia::tests::{
            apply_and_add_utxo, apply_and_add_utxo_and_declaration, declaration_in_snapshot,
            ledger, note, note_with_sk, update_ledger,
        },
        mantle::sdp::test_utils::generate_activity_proof,
    };

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    type HeaderId = [u8; 32];

    fn create_tx(
        inputs: Vec<NoteNullifier>,
        outputs: Vec<NoteCommitment>,
        commitment_merkle_root: Fr,
        excess_value: Value,
    ) -> SignedOps<Unverified, StandardMode> {
        let transfer_op = TransferOp::new(
            InputNullifiers::try_from(inputs).expect("Invalid inputs size"),
            OutputCommitments::try_from(outputs).expect("Invalid outputs size"),
            commitment_merkle_root,
            excess_value,
        );
        let mantle_tx = Ops::from([Op::Transfer(transfer_op)]);
        let ops_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(
            CompressedGroth16Proof::from_bytes(&[0u8; 128]),
        ))]);
        SignedOps::from_parts(mantle_tx, ops_proofs).unwrap()
    }

    fn ledger_root(state: &LedgerState) -> Fr {
        state
            .latest_note_sets()
            .get(LEDGER_SET)
            .unwrap()
            .commitment_root()
    }

    pub fn create_test_ledger() -> (Ledger<HeaderId>, HeaderId, (Fr, notes::Note)) {
        let config = config();
        let (secret_key, note) = note_with_sk();
        let mut genesis_state = LedgerState::from_notes([note], &config);
        update_ledger_prices(&mut genesis_state, 1, 1);
        let ledger = Ledger::new([0; 32], genesis_state, config);
        (ledger, [0; 32], (secret_key, note))
    }

    #[test]
    fn block_contents_record_the_note_set_roots() {
        let test_config = config();
        let genesis_note =
            notes::Note::new(10_000, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64)));
        let mut state = LedgerState::from_notes([genesis_note], &test_config);
        state.block_number = 5;
        let old_root = state
            .latest_note_sets()
            .get(LEDGER_SET)
            .unwrap()
            .commitment_root();
        let added = NoteCommitment(Fr::from(77u64));
        let ledger_set = state
            .latest_note_sets()
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .execute_adding(&[added])
            .unwrap();
        state.cryptarchia_ledger.note_sets = state
            .cryptarchia_ledger
            .note_sets
            .update(LEDGER_SET, ledger_set);

        let (state, _, _, _) = state
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                ZkPublicKey::zero(),
                std::iter::empty::<SignedOps<Preverified, StandardMode>>(),
            )
            .unwrap();

        let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
        let spent = [NoteNullifier(Fr::from(9u64))];
        assert!(ledger_set.assert_spendable(&spent, &old_root, 1028).is_ok());
        assert_eq!(
            ledger_set.assert_spendable(&spent, &old_root, 1029),
            Err(NoteSetError::UnknownRoot(old_root))
        );
        assert!(
            ledger_set
                .assert_spendable(&spent, &ledger_set.commitment_root(), 5000)
                .is_ok()
        );
        let expected = NoteSets::new()
            .update(
                LEDGER_SET,
                NoteSet::empty()
                    .execute_adding(&[
                        genesis_note.commitment(),
                        added,
                        notes::Note::new(38_051_750_380, Fr::ZERO, ZkPublicKey::zero())
                            .commitment(),
                    ])
                    .unwrap(),
            )
            .end_block(5);
        assert_eq!(
            state.latest_note_sets().eligible_root(),
            expected.eligible_root()
        );
    }

    #[test]
    fn empty_block_pays_the_leader_note_before_the_roots_are_recorded() {
        let test_config = config();
        let genesis_note =
            notes::Note::new(10_000, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64)));
        let mut state = LedgerState::from_notes([genesis_note], &test_config);
        state.block_number = 1;
        let reward_key = ZkPublicKey::new(Fr::from(0x4444u64));

        let (state, events, _, _) = state
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                reward_key,
                std::iter::empty::<SignedOps<Preverified, StandardMode>>(),
            )
            .unwrap();

        let leader_note = notes::Note::new(38_051_750_380, Fr::ZERO, reward_key);
        let expected = CommitmentMmr::new()
            .push(genesis_note.commitment())
            .unwrap()
            .push(leader_note.commitment())
            .unwrap()
            .frontier_root();
        let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
        assert_eq!(ledger_set.commitment_root(), expected);
        // The recorded root of block 1 includes the leader note.
        assert!(
            ledger_set
                .assert_spendable(&[NoteNullifier(Fr::from(9u64))], &expected, 2)
                .is_ok()
        );
        assert_eq!(events, []);
        let block_rewards = &state.cryptarchia_ledger.block_rewards;
        assert_eq!(block_rewards.rewards_reserve(), 999_999_904_870_624_049);
        assert_eq!(block_rewards.rewards_pool(), 0);
        assert_eq!(block_rewards.blend_rewards_pending(), 57_077_625_571);
    }

    #[test]
    fn block_contents_report_the_leader_note_and_the_note_set_updates() {
        let test_config = config();
        let genesis_note =
            notes::Note::new(10_000, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64)));
        let mut state = LedgerState::from_notes([genesis_note], &test_config);
        state.block_number = 1;
        let reward_key = ZkPublicKey::new(Fr::from(0x4444u64));

        let (_, tx_events, block_events, _) = state
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                reward_key,
                std::iter::empty::<SignedOps<Preverified, StandardMode>>(),
            )
            .unwrap();

        // The empty block reference: stake 10_000, the maximum release, no tips.
        let leader_note = notes::Note::new(38_051_750_380, Fr::ZERO, reward_key);
        assert_eq!(tx_events, []);
        assert_eq!(
            block_events,
            vec![
                BlockEvent::LeaderRewardPaid { note: leader_note },
                BlockEvent::NoteSetUpdated(NoteSetUpdate {
                    note_set: LEDGER_SET,
                    nullifiers: vec![],
                    commitments: vec![leader_note.commitment()],
                }),
            ]
        );
    }

    #[test]
    fn blend_rewards_pending_is_the_income_the_sdp_ledger_holds() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        for _ in 0..2 {
            (state, _, _, _) = state
                .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                    &test_config,
                    ZkPublicKey::zero(),
                    std::iter::empty::<SignedOps<Preverified, StandardMode>>(),
                )
                .unwrap();
        }
        assert_eq!(
            state
                .cryptarchia_ledger
                .block_rewards
                .blend_rewards_pending(),
            114_155_251_142
        );
        assert_eq!(
            state.mantle_ledger.sdp.pending_rewards_income(),
            114_155_251_142
        );
    }

    #[test]
    fn each_transaction_starts_with_empty_buffers() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let root = state
            .latest_note_sets()
            .get(LEDGER_SET)
            .unwrap()
            .commitment_root();
        let buffered = state
            .latest_note_sets()
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .buffer_outputs(&[NoteCommitment(Fr::from(5u64))]);
        state.cryptarchia_ledger.note_sets = state
            .cryptarchia_ledger
            .note_sets
            .update(LEDGER_SET, buffered);
        let (signing_key, verifying_key) = create_test_keys();

        let state = create_channel(
            state,
            &test_config,
            ChannelId::from([0u8; 32]),
            &signing_key,
            verifying_key.into_unverified(),
        );

        let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
        assert_eq!(ledger_set.tx_commitment_root(&root, 0), Ok(root));
    }

    fn transfer_tx(transfers: Vec<TransferOp>) -> SignedOps<Preverified, StandardMode> {
        let proofs = transfers
            .iter()
            .map(|_| {
                OpProof::ZkTransfer(ZkTransferProof::new(CompressedGroth16Proof::from_bytes(
                    &[0u8; 128],
                )))
            })
            .collect();
        SignedOps::from_parts(
            Ops::new_unchecked(transfers.into_iter().map(Op::Transfer).collect()),
            OpProofs::new_unchecked(proofs),
        )
        .unwrap()
        .preverify()
        .unwrap()
    }

    fn spend(nullifier: u64, commitment: u64, root: Fr, excess_value: Value) -> TransferOp {
        TransferOp::new(
            [NoteNullifier(Fr::from(nullifier))].into(),
            [NoteCommitment(Fr::from(commitment))].into(),
            root,
            excess_value,
        )
    }

    #[test]
    fn transfer_spends_an_output_of_an_earlier_operation_of_the_transaction() {
        let test_config = config();
        let genesis_note = note();
        let state = LedgerState::from_notes([genesis_note], &test_config);
        let root = ledger_root(&state);

        let (_, _, _, _, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                transfer_tx(vec![spend(1, 10, root, 0), spend(2, 11, root, 0)]),
            )
            .unwrap();

        let extended = CommitmentMmr::new()
            .push(genesis_note.commitment())
            .unwrap()
            .push(NoteCommitment(Fr::from(10u64)))
            .unwrap()
            .frontier_root();
        let tx_commitment_roots: Vec<Fr> = deferred
            .zk_transfers()
            .iter()
            .map(|deferred| deferred.inputs().tx_commitment_root.into_inner())
            .collect();
        assert_eq!(tx_commitment_roots, vec![root, extended]);
    }

    #[test]
    fn transactions_of_a_block_do_not_chain() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let root = ledger_root(&state);
        let (state, _, _, _, _) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                transfer_tx(vec![spend(1, 10, root, 0)]),
            )
            .unwrap();
        let unrecorded = ledger_root(&state);

        let (_, _, _, _, deferred) = state
            .clone()
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                transfer_tx(vec![spend(2, 11, root, 0)]),
            )
            .unwrap();
        assert_eq!(
            deferred.zk_transfers()[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            root
        );

        assert_eq!(
            state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                    &test_config,
                    transfer_tx(vec![spend(2, 11, unrecorded, 0)])
                )
                .err(),
            Some(LedgerError::VerificationError(
                VerificationError::TransferVerificationError(TransferError::NoteSet(
                    NoteSetError::UnknownRoot(unrecorded)
                ))
            ))
        );
    }

    #[test]
    fn a_nullifier_spent_by_an_earlier_transaction_of_the_block_is_rejected() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        update_ledger_prices(&mut state, 1, 1);
        let root = ledger_root(&state);
        let gas_context = OpsGasContext::from_channels(&Channels::new(), state.get_gas_prices());
        let fees = transfer_tx(vec![spend(1, 10, root, 0)])
            .op_refs()
            .total_gas_cost::<MainnetGasProfile>(&gas_context)
            .unwrap()
            .into_inner();
        let first = transfer_tx(vec![spend(1, 10, root, fees)]);
        let second = transfer_tx(vec![spend(1, 11, root, fees)]);
        let other = transfer_tx(vec![spend(2, 12, root, fees)]);

        assert_eq!(
            state
                .clone()
                .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                    &test_config,
                    ZkPublicKey::zero(),
                    [first.clone(), other.clone(), second.clone()].into_iter(),
                )
                .err(),
            Some(LedgerError::VerificationError(
                VerificationError::TransferVerificationError(TransferError::NoteSet(
                    NoteSetError::AlreadySpent(NoteNullifier(Fr::ONE))
                ))
            ))
        );

        // In the next block the nullifier is still spent, the other one too.
        let (state, _, _, _) = state
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                ZkPublicKey::zero(),
                [first, other].into_iter(),
            )
            .unwrap();
        let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
        assert!(ledger_set.contains_nullifier(&NoteNullifier(Fr::ONE)));
        assert!(ledger_set.contains_nullifier(&NoteNullifier(Fr::from(2u64))));
        assert_eq!(
            state
                .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                    &test_config,
                    ZkPublicKey::zero(),
                    std::iter::once(second),
                )
                .err(),
            Some(LedgerError::VerificationError(
                VerificationError::TransferVerificationError(TransferError::NoteSet(
                    NoteSetError::AlreadySpent(NoteNullifier(Fr::ONE))
                ))
            ))
        );
    }

    #[test]
    fn transaction_balance_sums_the_excess_values() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let root = ledger_root(&state);
        let (_, balance, _, _, _) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                transfer_tx(vec![spend(1, 10, root, 6), spend(2, 11, root, 7)]),
            )
            .unwrap();
        assert_eq!(balance, 13);
    }

    #[test]
    fn program_register_stores_the_key_once_and_charges_each_registration() {
        use lb_core::mantle::ops::NoOpProof;

        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let key = ProgramVerificationKey::try_from(&lb_groth16::test_utils::three_input_setup(1).0)
            .unwrap();
        let register = Op::ProgramRegister(ProgramRegisterOp {
            verification_key: key.clone(),
        });
        let tx = SignedOps::from_parts(
            Ops::new_unchecked(vec![register.clone(), register]),
            OpProofs::new_unchecked(vec![OpProof::None(NoOpProof), OpProof::None(NoOpProof)]),
        )
        .unwrap()
        .preverify()
        .unwrap();

        let (state, _, execution_gas, events, _) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
            .unwrap();

        assert_eq!(execution_gas, Gas::new(2 * 1_896));
        assert_eq!(events, []);
        assert_eq!(state.mantle_ledger.programs().len(), 1);
        assert_eq!(
            state
                .mantle_ledger
                .programs()
                .get(&derive_program_id(&key))
                .map(RegisteredProgram::key),
            Some(&key)
        );
    }

    #[test]
    fn transaction_balance_above_u64_is_rejected() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let root = ledger_root(&state);
        assert_eq!(
            state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                    &test_config,
                    transfer_tx(vec![spend(1, 10, root, u64::MAX), spend(2, 11, root, 1)]),
                )
                .err(),
            Some(LedgerError::BalanceOverflow)
        );
    }

    // One SDP Operation proven by a zero ZkTransfer and signed by the provider of
    // `SDPDeclareOp::sample()`.
    fn provider_signed_tx(op: Op) -> SignedOps<Preverified, StandardMode> {
        let ops = Ops::from([op]);
        let signature = Ed25519Key::from_bytes(&[24; 32])
            .sign_payload(TxHashView::from(ops.hash()).as_bytes().as_ref());
        let proof = ZkAndEd25519Proof::new(
            ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            signature,
        );
        SignedOps::from_parts(ops, OpProofs::from([OpProof::ZkAndEd25519Sigs(proof)]))
            .unwrap()
            .preverify()
            .unwrap()
    }

    fn service_note_root(declare: &SDPDeclareOp) -> Fr {
        CommitmentMmr::new()
            .push(declare.service_note().commitment())
            .unwrap()
            .frontier_root()
    }

    #[test]
    fn sdp_declare_excess_does_not_pay_fees() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let root = ledger_root(&state);
        let declare = SDPDeclareOp {
            commitment_merkle_root: root,
            ..SDPDeclareOp::sample()
        };

        let (state, balance, _, _, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                provider_signed_tx(Op::SDPDeclare(declare.clone())),
            )
            .unwrap();

        assert_eq!(balance, 0);
        let public = deferred.zk_transfers()[0].inputs();
        assert_eq!(public.excess_value.into_inner(), Fr::from(45u64));
        assert_eq!(public.tx_commitment_root.into_inner(), root);
        let sets = state.latest_note_sets();
        assert!(
            sets.get(LEDGER_SET)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::from(26u64)))
        );
        assert_eq!(
            sets.get(SDP_SET).unwrap().commitment_root(),
            service_note_root(&declare)
        );
        assert!(
            state
                .mantle_ledger()
                .sdp_ledger()
                .get_declaration(&declare.id())
                .is_some()
        );
    }

    #[test]
    fn sdp_withdraw_excess_adds_to_the_balance() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let declare = SDPDeclareOp::sample();
        let (mantle_ledger, note_sets, _) = state
            .mantle_ledger
            .clone()
            .try_apply_sdp_declaration(
                SignedOperation::new(declare.clone(), ZkAndEd25519Proof::sample())
                    .into_state_trusted(),
                state.latest_note_sets().clone(),
                &test_config,
            )
            .unwrap();
        state.mantle_ledger = mantle_ledger;
        state.cryptarchia_ledger = state.cryptarchia_ledger.update_note_sets(note_sets);
        let root_before = ledger_root(&state);
        let withdraw = SDPWithdrawOp {
            declaration_id: declare.id(),
            ..SDPWithdrawOp::sample()
        };

        let (state, balance, _, _, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                provider_signed_tx(Op::SDPWithdraw(withdraw)),
            )
            .unwrap();

        assert_eq!(balance, 47);
        assert_eq!(
            deferred.zk_transfers()[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            service_note_root(&declare)
        );
        assert!(
            state
                .latest_note_sets()
                .get(SDP_SET)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::from(28u64)))
        );
        assert_eq!(ledger_root(&state), root_before);
        assert_eq!(
            state
                .mantle_ledger()
                .sdp_ledger()
                .get_declaration(&declare.id())
                .unwrap()
                .withdraw_outputs,
            [NoteCommitment(Fr::from(46u64))].into()
        );
    }

    #[test]
    fn verification_helper_reads_the_epoch_of_the_applied_header() {
        use lb_core::mantle::transactions::OperationVerificationHelper as _;

        let leader = note();
        let config = config();
        let epoch_length = config.epoch_length();
        let (mut ledger, genesis) = ledger(&[leader], config);
        let block = update_ledger(&mut ledger, genesis, epoch_length, leader).unwrap();

        let state = ledger.state(&block).unwrap();
        let helper = MantleOperationVerificationHelper::new(
            &state.mantle_ledger,
            &state.cryptarchia_ledger,
            state.block_number,
            ledger.config(),
        );
        assert_eq!(helper.get_epoch(), Epoch::new(1));
    }

    #[test]
    fn withdrawal_outputs_enter_the_ledger_set_the_epoch_after_withdraw_at() {
        let leader_utxo = note();
        let config = config();
        let epoch_length = config.epoch_length();
        let (mut ledger, genesis) = ledger(&[leader_utxo], config.clone());
        let h_1 = update_ledger(&mut ledger, genesis, epoch_length, leader_utxo).unwrap();

        // Declared and withdrawn in epoch 1: `withdraw_at = 3`, released in the first
        // block of epoch 4.
        let declare = SDPDeclareOp::sample();
        let withdraw = SDPWithdrawOp {
            declaration_id: declare.id(),
            ..SDPWithdrawOp::sample()
        };
        let state = ledger.states.get_mut(&h_1).unwrap();
        let (mantle_ledger, note_sets, _) = state
            .mantle_ledger
            .clone()
            .try_apply_sdp_declaration(
                SignedOperation::new(declare.clone(), ZkAndEd25519Proof::sample())
                    .into_state_trusted(),
                state.latest_note_sets().clone(),
                &config,
            )
            .unwrap();
        let (mantle_ledger, note_sets, _) = mantle_ledger
            .try_apply_sdp_withdraw(
                SignedOperation::new(withdraw, ZkAndEd25519Proof::sample()).into_state_trusted(),
                note_sets,
                &config,
            )
            .unwrap();
        state.mantle_ledger = mantle_ledger;
        state.cryptarchia_ledger = state.cryptarchia_ledger.clone().update_note_sets(note_sets);
        let ledger_set_before = state.latest_note_sets().get(LEDGER_SET).unwrap().clone();

        // Every block also pays its leader the maximum release share, the stake being
        // far below target.
        let leader_note =
            notes::Note::new(38_051_750_380, Fr::ZERO, ZkPublicKey::zero()).commitment();
        let h_2 = update_ledger(&mut ledger, h_1, 2 * epoch_length, leader_utxo).unwrap();
        let h_3 = update_ledger(&mut ledger, h_2, 3 * epoch_length, leader_utxo).unwrap();
        assert_eq!(
            ledger_root(&ledger.states[&h_3]),
            ledger_set_before
                .clone()
                .execute_adding(&[leader_note, leader_note])
                .unwrap()
                .commitment_root()
        );
        let h_4 = update_ledger(&mut ledger, h_3, 4 * epoch_length, leader_utxo).unwrap();
        assert_eq!(
            ledger_root(&ledger.states[&h_4]),
            ledger_set_before
                .execute_adding(&[
                    leader_note,
                    leader_note,
                    NoteCommitment(Fr::from(46u64)),
                    leader_note
                ])
                .unwrap()
                .commitment_root()
        );
        assert!(
            ledger.states[&h_4]
                .mantle_ledger()
                .sdp_ledger()
                .get_declaration(&declare.id())
                .is_none()
        );
    }

    /// The genesis epoch-state active-declarations snapshots must be seeded
    /// from the genesis SDP ledger, not left as the empty default the
    /// cryptarchia genesis constructor initializes them with.
    #[test]
    fn genesis_seeds_epoch_state_sdp_from_mantle() {
        let config = config();
        let ledger = LedgerState::from_notes([note()], &config);

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

    fn cryptarchia_inscription() -> InscriptionOp {
        InscriptionOp {
            channel_id: NULL_CHANNEL_ID,
            inscription: Inscription::new_unchecked(
                CryptarchiaParameter {
                    chain_id: "test-chain".to_owned().try_into().unwrap(),
                    genesis_time: GenesisTime::new(1000),
                    epoch_nonce: Fr::ZERO,
                }
                .encode_to_vec(),
            ),
            parent: MsgId::root(),
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            steps: ChannelSteps::default(),
        }
    }

    // Nine distributed notes, the first one staked by a Blend declaration.
    fn genesis_with_a_declaration() -> (GenesisTx, [GenesisAllocation; 9], SDPDeclareOp) {
        let stake_key = ZkKey::new(Fr::from(1u64));
        let allocations: [GenesisAllocation; 9] = core::array::from_fn(|i| GenesisAllocation {
            value: 1_000 * (i as u64 + 1),
            public_key: if i == 0 {
                stake_key.to_public_key()
            } else {
                ZkPublicKey::new(Fr::from(100 + i as u64))
            },
        });
        let declaration = SDPDeclareOp {
            inputs: [derive_note_nullifier(
                &allocations[0].note(0).commitment(),
                stake_key.as_fr(),
            )]
            .into(),
            commitment_merkle_root: CommitmentMmr::new().frontier_root(),
            amount: 1_000,
            ..SDPDeclareOp::sample()
        };
        let block = GenesisBlockBuilder::new()
            .add_notes(allocations)
            .set_inscription(cryptarchia_inscription())
            .add_declaration(declaration.clone())
            .build_proven(&[stake_key])
            .unwrap();
        (block.genesis_tx().clone(), allocations, declaration)
    }

    #[test]
    fn genesis_records_the_roots_after_the_whole_transaction() {
        let config = config();
        let (tx, allocations, declaration) = genesis_with_a_declaration();
        let (state, _) = LedgerState::from_genesis_tx::<HeaderId>(tx, &config, Fr::ZERO).unwrap();

        let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
        let ledger_root = allocations
            .iter()
            .enumerate()
            .fold(CommitmentMmr::new(), |mmr, (index, allocation)| {
                mmr.push(allocation.note(index).commitment()).unwrap()
            })
            .frontier_root();
        assert_eq!(ledger_set.commitment_root(), ledger_root);
        assert!(ledger_set.contains_nullifier(&declaration.inputs.as_slice()[0]));
        // The buffer is closed: a block 1 transaction sees the recorded root alone.
        assert_eq!(
            ledger_set.tx_commitment_root(&ledger_root, 1),
            Ok(ledger_root)
        );
        assert_eq!(
            state
                .latest_note_sets()
                .get(SDP_SET)
                .unwrap()
                .commitment_root(),
            CommitmentMmr::new()
                .push(declaration.service_note().commitment())
                .unwrap()
                .frontier_root()
        );
        assert!(
            state
                .mantle_ledger()
                .sdp_ledger()
                .get_declaration(&declaration.id())
                .is_some()
        );
        assert!(
            state
                .mantle_ledger()
                .channels()
                .channel_state(&NULL_CHANNEL_ID)
                .unwrap()
                .note_set
                .is_none()
        );

        // C_LEAD of epochs 0 and 1 is the eligible root after the genesis transaction.
        let eligible_root = state.latest_note_sets().eligible_root();
        assert_ne!(eligible_root, NoteSets::new().eligible_root());
        assert_eq!(state.epoch_state().eligible_root(), eligible_root);
        assert_eq!(state.next_epoch_state().eligible_root(), eligible_root);

        // The Blend leader inputs were taken from that epoch state, not from the sets
        // before the declarations.
        let rewards_params = &config.sdp_config.service_rewards_params;
        let sdp = state.mantle_ledger().sdp_ledger().clone();
        assert_eq!(
            sdp.clone()
                .with_genesis_epoch_state(rewards_params, state.epoch_state()),
            sdp
        );
        let before_declarations = EpochState {
            note_sets: NoteSets::new(),
            ..state.epoch_state().clone()
        };
        assert_ne!(
            sdp.clone()
                .with_genesis_epoch_state(rewards_params, &before_declarations),
            sdp
        );
    }

    #[test]
    fn genesis_total_stake_is_the_published_distribution() {
        let config = config();
        let (tx, allocations, _) = genesis_with_a_declaration();
        let (state, _) = LedgerState::from_genesis_tx::<HeaderId>(tx, &config, Fr::ZERO).unwrap();
        let distributed: Value = allocations.iter().map(|allocation| allocation.value).sum();
        assert_eq!(distributed, 45_000);
        assert_eq!(state.epoch_state().total_stake(), distributed);
        assert_eq!(state.next_epoch_state().total_stake(), distributed);

        let overflowing = Distribution::from([
            GenesisAllocation {
                value: u64::MAX,
                public_key: ZkPublicKey::new(Fr::from(1u64)),
            },
            GenesisAllocation {
                value: 1,
                public_key: ZkPublicKey::new(Fr::from(2u64)),
            },
        ]);
        assert_eq!(
            CryptarchiaLedger::genesis_total_stake::<HeaderId>(&overflowing, &config),
            Err(LedgerError::BalanceOverflow)
        );
    }

    #[test]
    fn genesis_initializes_the_block_reward_stocks() {
        let (tx, _, _) = genesis_with_a_declaration();
        let (state, _) = LedgerState::from_genesis_tx::<HeaderId>(tx, &config(), Fr::ZERO).unwrap();
        let block_rewards = &state.cryptarchia_ledger.block_rewards;
        assert_eq!(block_rewards.rewards_reserve(), 1_000_000_000_000_000_000);
        assert_eq!(block_rewards.rewards_pool(), 0);
        assert_eq!(block_rewards.blend_rewards_pending(), 0);
        assert!(
            block_rewards
                .pooled_fees_window()
                .iter()
                .all(|fees| *fees == 0)
        );
    }

    #[test]
    fn genesis_rejects_a_second_declaration_of_a_provider_id_or_zk_id() {
        use lb_core::sdp::{ProviderId, ServiceType};

        let stake_keys = [ZkKey::new(Fr::from(1u64)), ZkKey::new(Fr::from(2u64))];
        let allocations: [GenesisAllocation; 2] = core::array::from_fn(|i| GenesisAllocation {
            value: 1_000,
            public_key: stake_keys[i].to_public_key(),
        });
        let declaration = |index: usize, provider_seed: u8, zk_id: u64| SDPDeclareOp {
            provider_id: ProviderId(Ed25519Key::from_bytes(&[provider_seed; 32]).public_key()),
            zk_id: ZkPublicKey::new(Fr::from(zk_id)),
            inputs: [derive_note_nullifier(
                &allocations[index].note(index).commitment(),
                stake_keys[index].as_fr(),
            )]
            .into(),
            commitment_merkle_root: CommitmentMmr::new().frontier_root(),
            amount: 1_000,
            ..SDPDeclareOp::sample()
        };
        let apply = |second: SDPDeclareOp| {
            let block = GenesisBlockBuilder::new()
                .add_notes(allocations)
                .set_inscription(cryptarchia_inscription())
                .add_declaration(declaration(0, 1, 7))
                .add_declaration(second)
                .unwrap()
                .build_proven(&stake_keys)
                .unwrap();
            LedgerState::from_genesis_tx::<HeaderId>(
                block.genesis_tx().clone(),
                &config(),
                Fr::ZERO,
            )
            .map(|_| ())
        };

        assert_eq!(
            apply(declaration(1, 1, 8)),
            Err(LedgerError::Mantle(mantle::Error::Sdp(
                SdpError::DuplicateProviderId {
                    service_type: ServiceType::BlendNetwork,
                    provider_id: Box::new(declaration(1, 1, 8).provider_id),
                }
                .into()
            )))
        );
        assert_eq!(
            apply(declaration(1, 2, 7)),
            Err(LedgerError::Mantle(mantle::Error::Sdp(
                SdpError::DuplicateZkId {
                    service_type: ServiceType::BlendNetwork,
                    zk_id: ZkPublicKey::new(Fr::from(7u64)),
                }
                .into()
            )))
        );
        assert_eq!(apply(declaration(1, 2, 8)), Ok(()));
    }

    #[test]
    fn genesis_rejects_a_declaration_whose_zk_transfer_does_not_verify() {
        let (proven, _, declaration) = genesis_with_a_declaration();
        let allocations = proven.distribution();
        let unproven = GenesisBlockBuilder::new()
            .try_add_notes(allocations.iter().copied())
            .unwrap()
            .set_inscription(cryptarchia_inscription())
            .add_declaration(declaration)
            .build()
            .unwrap();
        assert_eq!(
            LedgerState::from_genesis_tx::<HeaderId>(
                unproven.genesis_tx().clone(),
                &config(),
                Fr::ZERO
            )
            .err(),
            Some(LedgerError::Mantle(mantle::Error::Sdp(
                SdpError::InvalidZkTransfer.into()
            )))
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
        MultiSequencer(ChannelMultiSigProof),
    }

    fn create_signed_tx(op: Op, signing_key: &Key) -> SignedOps<Preverified, StandardMode> {
        create_multi_signed_tx(vec![op], vec![signing_key])
    }

    fn create_multi_signed_tx(
        ops_vec: Vec<Op>,
        signing_keys: Vec<&Key>,
    ) -> SignedOps<Preverified, StandardMode> {
        let ops = Ops::new_unchecked(ops_vec);

        let tx_hash = ops.hash();
        let ops_proofs = signing_keys
            .into_iter()
            .zip(ops.iter())
            .map(|(key, _)| match key {
                Key::Ed25519(key) => OpProof::ChannelInscribe(ChannelInscribeProof::without_steps(
                    key.sign_payload(tx_hash.as_signing_bytes()),
                )),
                Key::MultiSequencer(proof) => OpProof::ChannelMultiSigProof(proof.clone()),
            })
            .collect::<Vec<_>>();
        let ops_proofs = OpProofs::new_unchecked(ops_proofs);
        SignedOps::from_parts(ops, ops_proofs)
            .unwrap()
            .preverify()
            .expect("Test transaction should have valid signatures")
    }

    fn create_channel(
        ledger_state: LedgerState,
        config: &Config,
        id: ChannelId,
        signing_key: &Ed25519Key,
        verifying_key: UnverifiedEd25519PublicKey,
    ) -> LedgerState {
        let tx = create_signed_tx(
            Op::ChannelInscribe(InscriptionOp {
                channel_id: id,
                inscription: [1, 2, 3, 4].into(),
                parent: MsgId::root(),
                signer: verifying_key,
                steps: ChannelSteps::default(),
            }),
            &Key::Ed25519(signing_key.clone()),
        );
        ledger_state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(config, tx)
            .unwrap()
            .0
    }

    // A channel that does not exist yet accredits no key, so its configuration
    // is verified against a threshold of 0 and must carry an empty proof.
    fn create_genesis_config_tx(
        config_op: ChannelConfigOp,
    ) -> SignedOps<Preverified, StandardMode> {
        create_signed_tx(
            Op::ChannelConfig(config_op),
            &Key::MultiSequencer(ChannelMultiSigProof::empty()),
        )
    }

    fn create_config_tx(
        config_op: ChannelConfigOp,
        signing_key: &Ed25519Key,
    ) -> SignedOps<Preverified, StandardMode> {
        let ops = Ops::from([Op::ChannelConfig(config_op.clone())]);
        let config_tx_hash = ops.hash();
        let config_proof = ChannelMultiSigProof::new(IndexedSignatures::from((
            0,
            signing_key.sign_payload(config_tx_hash.as_signing_bytes()),
        )));

        create_signed_tx(
            Op::ChannelConfig(config_op),
            &Key::MultiSequencer(config_proof),
        )
    }

    #[expect(clippy::too_many_arguments, reason = "test fn")]
    fn apply_and_add_utxo_and_activity(
        ledger: &mut Ledger<HeaderId>,
        parent: HeaderId,
        slot: impl Into<Slot>,
        target_epoch_state: &EpochState,
        utxo_proof: notes::Note,
        utxo_add: notes::Note,
        declaration_id: DeclarationId,
        zk_key: &ZkKey,
        nonce: Nonce,
    ) -> HeaderId {
        let id = apply_and_add_utxo(ledger, parent, slot, utxo_proof, utxo_add);
        let current_epoch_state = ledger
            .states
            .get(&id)
            .unwrap()
            .cryptarchia_ledger
            .epoch_state
            .clone();

        let config = ledger.config().clone();
        let active_op = SDPActiveOp {
            declaration_id,
            nonce,
            metadata: ActivityMetadata::Blend(Box::new(generate_activity_proof(
                zk_key,
                target_epoch_state,
                &current_epoch_state,
                &config.sdp_config.service_rewards_params.blend,
            ))),
        };
        let signed_operation =
            SignedOperation::new(active_op, Ed25519Signature::zero()).into_state_trusted();

        let block_ledger = ledger.states.get_mut(&id).unwrap();
        block_ledger.mantle_ledger = block_ledger
            .mantle_ledger
            .clone()
            .try_apply_sdp_active(signed_operation, &config)
            .unwrap()
            .0;
        id
    }

    #[test]
    fn test_ledger_creation() {
        let (ledger, genesis_id, (_, genesis_note)) = create_test_ledger();
        let state = ledger.state(&genesis_id).unwrap();
        let expected = CommitmentMmr::new()
            .push(genesis_note.commitment())
            .unwrap()
            .frontier_root();
        assert_eq!(ledger_root(state), expected);
        assert_eq!(state.slot(), 0.into());
    }

    #[test]
    fn test_ledger_try_update_with_transaction() {
        let (mut ledger, genesis_id, (secret_key, genesis_note)) = create_test_ledger();
        let root = ledger_root(ledger.state(&genesis_id).unwrap());
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(genesis_note.commitment(), &mut [])
            .unwrap();
        assert_eq!(mmr.frontier_root(), root);
        let nullifier = derive_note_nullifier(&genesis_note.commitment(), &secret_key);

        // The fee depends on the encoded size only, so a draft with placeholder values
        // prices it.
        let draft = Ops::from([Op::Transfer(TransferOp::new(
            [nullifier].into(),
            [NoteCommitment(Fr::ZERO)].into(),
            root,
            0,
        ))]);
        let gas_context = OpsGasContext::from_channels(&Channels::new(), GasPrices::new(1, 1));
        let fees = draft
            .by_ref()
            .total_gas_cost::<MainnetGasProfile>(&gas_context)
            .unwrap()
            .into_inner();
        let output = notes::Note::new(
            genesis_note.value.checked_sub(fees).unwrap(),
            Fr::from(5u64),
            ZkPublicKey::new(Fr::from(1u64)),
        );
        let ops = Ops::from([Op::Transfer(TransferOp::new(
            [nullifier].into(),
            [output.commitment()].into(),
            root,
            fees,
        ))]);
        let msg = *TxHashView::from(ops.hash()).as_fr();
        let proof = prove_transfer(&[(genesis_note, secret_key, path)], &[output], root, msg);
        let tx = SignedOps::from_parts(
            ops,
            OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(proof))]),
        )
        .unwrap()
        .preverify()
        .unwrap();
        let leader_proof = generate_proof(
            &ledger.state(&genesis_id).unwrap().cryptarchia_ledger,
            &note(),
            Slot::from(1u64),
        );

        let new_id = [1; 32];
        let update = ledger
            .prepare_update::<_, _, MainnetGasProfile>(
                new_id,
                genesis_id,
                Slot::from(1u64),
                &leader_proof,
                &UncleSlots::default(),
                std::iter::once(tx),
            )
            .unwrap()
            .verify_batch_proofs()
            .unwrap();
        // A transfer emits no transaction event, the block events remain.
        assert!(
            !ledger
                .commit_update(update)
                .iter()
                .any(|event| matches!(event, lb_core::events::Event::Tx(_)))
        );

        let ledger_set = ledger
            .state(&new_id)
            .unwrap()
            .latest_note_sets()
            .get(LEDGER_SET)
            .unwrap();
        assert!(ledger_set.contains_nullifier(&nullifier));
        assert_eq!(
            ledger_set.commitment_root(),
            mmr.push(output.commitment())
                .unwrap()
                .push(notes::Note::new(38_051_750_380, Fr::ZERO, ZkPublicKey::zero()).commitment())
                .unwrap()
                .frontier_root()
        );
    }

    #[test]
    fn test_channel_inscribe_operation() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([2; 32]);

        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3, 4].into(),
            parent: MsgId::root(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let tx = create_signed_tx(Op::ChannelInscribe(inscribe_op), &Key::Ed25519(signing_key));
        let result =
            state.try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _, _, events, deferred_proofs) = result.unwrap();
        deferred_proofs.verify().unwrap();

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
                .channel_state(&channel_id)
                .unwrap()
                .note_set,
            Some(2)
        );
        assert_eq!(new_state.latest_note_sets().get(2), Some(&NoteSet::empty()));
        assert_eq!(events, []);
    }

    #[test]
    fn test_channel_config_operation() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (_, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([3; 32]);

        let config_op = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };

        let tx = create_genesis_config_tx(config_op);
        let result =
            state.try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _, _, events, deferred_proofs) = result.unwrap();
        deferred_proofs.verify().unwrap();

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
            verifying_key.into_unverified().into()
        );
        assert_eq!(events, []);
    }

    #[test]
    fn test_jit_config_requires_zero_parent() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (_, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // The channel doesn't exist yet, so any parent but ZERO is dangling
        let dangling_config = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::from([1; 32]),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };
        let dangling_tx = create_genesis_config_tx(dangling_config.clone());
        let result = state
            .clone()
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, dangling_tx);
        assert!(matches!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::InvalidParent { .. }
                )
            ))
        ));

        // The same configuration rooted at ZERO creates the channel
        let rooted_config = ChannelConfigOp {
            parent: MsgId::root(),
            ..dangling_config
        };
        let rooted_tx = create_genesis_config_tx(rooted_config.clone());
        let new_state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, rooted_tx)
            .unwrap()
            .0;
        let channel = new_state
            .mantle_ledger
            .channels()
            .channels
            .get(&channel_id)
            .unwrap();
        assert_eq!(channel.config_tip_hash, rooted_config.id());
        assert_eq!(channel.tip_message, MsgId::root());
    }

    #[test]
    fn test_channel_config_valid_after_inscriptions() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // Create the channel, its config tip stays ZERO
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };
        let mut tip = first_inscribe.id();
        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
            .unwrap()
            .0;

        // Sign a configuration against the current config tip
        let config_op = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };
        let config_tx = create_config_tx(config_op.clone(), &signing_key);

        // Inscriptions keep extending the channel, none of them moves the config tip
        for byte in [4u8, 5, 6] {
            let inscribe = InscriptionOp {
                channel_id,
                inscription: [byte].into(),
                parent: tip,
                signer: verifying_key.into_unverified(),
                steps: ChannelSteps::default(),
            };
            tip = inscribe.id();
            let tx = create_signed_tx(
                Op::ChannelInscribe(inscribe),
                &Key::Ed25519(signing_key.clone()),
            );
            state = state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
                .unwrap()
                .0;
        }

        // The configuration is still valid and leaves the message tip untouched
        let new_state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, config_tx)
            .unwrap()
            .0;
        let channel = new_state
            .mantle_ledger
            .channels()
            .channels
            .get(&channel_id)
            .unwrap();
        assert_eq!(channel.config_tip_hash, config_op.id());
        assert_eq!(channel.tip_message, tip);
    }

    #[test]
    fn test_channel_config_rejected_after_later_config() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // C1 creates the channel, C2 supersedes it
        let config1 = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };
        let config1_tx = create_genesis_config_tx(config1.clone());
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                config1_tx.clone(),
            )
            .unwrap()
            .0;
        let config2 = ChannelConfigOp {
            parent: config1.id(),
            ..config1.clone()
        };
        let config2_tx = create_config_tx(config2, &signing_key);
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, config2_tx)
            .unwrap()
            .0;

        // The superseded configuration cannot be re-included after a
        // reorganization abandons its block
        let result = state
            .clone()
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, config1_tx);
        assert!(matches!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::InvalidParent { .. }
                )
            ))
        ));

        // Neither can a sibling extending the same superseded parent
        let sibling = ChannelConfigOp {
            parent: config1.id(),
            posting_timeframe: 1.into(),
            ..config1
        };
        let sibling_tx = create_config_tx(sibling, &signing_key);
        let result = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, sibling_tx);
        assert!(matches!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::InvalidParent { .. }
                )
            ))
        ));
    }

    #[test]
    fn test_inscription_valid_after_config() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // Create the channel, the message tip is now I1
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };
        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe.clone()),
            &Key::Ed25519(signing_key.clone()),
        );
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
            .unwrap()
            .0;

        // A configuration keeping the signer accredited executes
        let config_op = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };
        let config_tx = create_config_tx(config_op.clone(), &signing_key);
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, config_tx)
            .unwrap()
            .0;

        // An inscription signed before the configuration still extends I1
        let second_inscribe = InscriptionOp {
            channel_id,
            inscription: [4, 5, 6].into(),
            parent: first_inscribe.id(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };
        let second_tx = create_signed_tx(
            Op::ChannelInscribe(second_inscribe.clone()),
            &Key::Ed25519(signing_key),
        );
        let new_state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, second_tx)
            .unwrap()
            .0;
        let channel = new_state
            .mantle_ledger
            .channels()
            .channels
            .get(&channel_id)
            .unwrap();
        assert_eq!(channel.tip_message, second_inscribe.id());
        assert_eq!(channel.config_tip_hash, config_op.id());
    }

    fn deposit_tx(deposit: DepositOp) -> SignedOps<Preverified, StandardMode> {
        SignedOps::from_parts(
            Ops::from([Op::ChannelDeposit(deposit)]),
            OpProofs::from([OpProof::ZkTransfer(zero_proof())]),
        )
        .unwrap()
        .preverify()
        .unwrap()
    }

    fn zero_proof() -> ZkTransferProof {
        ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128]))
    }

    #[test]
    fn deposit_moves_ledger_notes_into_the_channel_set_and_pays_with_its_excess() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let root = ledger_root(&state);
        let deposit = DepositOp {
            channel_id,
            inputs: [NoteNullifier(Fr::ONE)].into(),
            commitment_merkle_root: root,
            outputs: [NoteCommitment(Fr::from(10u64))].into(),
            excess_value: 6,
            metadata: [5, 6, 7, 8].into(),
        };

        let (state, balance, _, events, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                deposit_tx(deposit.clone()),
            )
            .unwrap();

        assert_eq!(balance, 6);
        assert_eq!(deferred.zk_transfers().len(), 1);
        assert!(
            state
                .latest_note_sets()
                .get(LEDGER_SET)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::ONE))
        );
        assert_eq!(
            state.latest_note_sets().get(2).unwrap().commitment_root(),
            CommitmentMmr::new()
                .push(NoteCommitment(Fr::from(10u64)))
                .unwrap()
                .frontier_root()
        );
        assert!(matches!(
            events.as_slice(),
            [TxEvent { payload: TxEventPayload::Deposit { outputs, .. }, .. }] if *outputs == deposit.outputs
        ));
    }

    fn inscribe_tx(
        ops: Vec<Op>,
        signing_key: &Ed25519Key,
        step_proofs: usize,
    ) -> SignedOps<Preverified, StandardMode> {
        let ops = Ops::new_unchecked(ops);
        let tx_hash = ops.hash();
        let proofs = ops
            .iter()
            .map(|op| match op {
                Op::ChannelInscribe(_) => OpProof::ChannelInscribe(ChannelInscribeProof::new(
                    signing_key.sign_payload(tx_hash.as_signing_bytes()),
                    std::iter::repeat_with(|| StepProof::User(zero_proof()))
                        .take(step_proofs)
                        .collect(),
                )),
                _ => OpProof::ZkTransfer(zero_proof()),
            })
            .collect();
        SignedOps::from_parts(ops, OpProofs::new_unchecked(proofs))
            .unwrap()
            .preverify()
            .unwrap()
    }

    fn one_step(
        input: u64,
        output: u64,
        commitment_merkle_root: Fr,
        buffered: u16,
    ) -> ChannelSteps {
        [Step::User(ChannelStep {
            inputs: [NoteNullifier(Fr::from(input))].into(),
            outputs: [NoteCommitment(Fr::from(output))].into(),
            commitment_merkle_root,
            buffered,
        })]
        .into()
    }

    #[test]
    fn deposit_then_step_in_one_transaction_covers_the_deposit_with_buffered() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let channel = state
            .mantle_ledger()
            .channels()
            .channel_state(&channel_id)
            .unwrap()
            .clone();
        let channel_root = state.latest_note_sets().get(2).unwrap().commitment_root();
        let deposit = DepositOp {
            channel_id,
            inputs: [NoteNullifier(Fr::ONE)].into(),
            commitment_merkle_root: ledger_root(&state),
            outputs: [NoteCommitment(Fr::from(10u64))].into(),
            excess_value: 0,
            metadata: Metadata::empty(),
        };
        let inscribe = InscriptionOp {
            channel_id,
            inscription: [1].into(),
            parent: channel.tip_message,
            signer: verifying_key.into_unverified(),
            steps: one_step(11, 12, channel_root, 1),
        };

        let (state, _, _, _, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                inscribe_tx(
                    vec![Op::ChannelDeposit(deposit), Op::ChannelInscribe(inscribe)],
                    &signing_key,
                    1,
                ),
            )
            .unwrap();

        let covered = CommitmentMmr::new()
            .push(NoteCommitment(Fr::from(10u64)))
            .unwrap()
            .frontier_root();
        assert_eq!(
            deferred.zk_transfers()[1]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            covered
        );
        assert!(
            state
                .latest_note_sets()
                .get(2)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::from(11u64)))
        );
    }

    #[test]
    fn a_step_cannot_consume_a_note_an_earlier_inscription_of_the_transaction_consumed() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let channel = state
            .mantle_ledger()
            .channels()
            .channel_state(&channel_id)
            .unwrap()
            .clone();
        let channel_root = state.latest_note_sets().get(2).unwrap().commitment_root();
        let first = InscriptionOp {
            channel_id,
            inscription: [1].into(),
            parent: channel.tip_message,
            signer: verifying_key.into_unverified(),
            steps: one_step(11, 12, channel_root, 0),
        };
        let second = InscriptionOp {
            parent: first.id(),
            inscription: [2].into(),
            steps: one_step(11, 13, channel_root, 0),
            ..first
        };

        assert_eq!(
            state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                    &test_config,
                    inscribe_tx(
                        vec![Op::ChannelInscribe(first), Op::ChannelInscribe(second)],
                        &signing_key,
                        1
                    ),
                )
                .err(),
            Some(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(mantle::channel::Error::NoteSet(
                    NoteSetError::AlreadySpent(NoteNullifier(Fr::from(11u64)))
                ))
            ))
        );
    }

    // As `inscribe_tx`, with the sample proof of the shape of each step and no
    // proof for a program registration.
    fn program_step_tx(
        ops: Vec<Op>,
        signing_key: &Ed25519Key,
    ) -> SignedOps<Preverified, StandardMode> {
        use lb_core::mantle::ops::NoOpProof;

        let ops = Ops::new_unchecked(ops);
        let tx_hash = ops.hash();
        let proofs = ops
            .iter()
            .map(|op| match op {
                Op::ChannelInscribe(inscribe) => {
                    OpProof::ChannelInscribe(ChannelInscribeProof::new(
                        signing_key.sign_payload(tx_hash.as_signing_bytes()),
                        inscribe
                            .steps
                            .iter()
                            .map(|step| StepProof::sample_for(step.proof_shape()))
                            .collect(),
                    ))
                }
                Op::ProgramRegister(_) => OpProof::None(NoOpProof),
                _ => OpProof::ZkTransfer(zero_proof()),
            })
            .collect();
        SignedOps::from_parts(ops, OpProofs::new_unchecked(proofs))
            .unwrap()
            .preverify()
            .unwrap()
    }

    fn test_program_key() -> ProgramVerificationKey {
        ProgramVerificationKey::try_from(&lb_groth16::test_utils::three_input_setup(1).0).unwrap()
    }

    // A program step paying no opened note, so the sample proofs pass
    // preverification.
    fn program_steps(
        program_id: Fr,
        nullifier: u64,
        output: u64,
        commitment_merkle_root: Fr,
        buffered: u16,
    ) -> ChannelSteps {
        [Step::Program(ProgramStep {
            program_id,
            payments: [Payment {
                inputs: [NoteNullifier(Fr::from(nullifier))].into(),
                outputs: OutputCommitments::default(),
                commitment_merkle_root,
                buffered,
                paid_value: 5,
                intent: Fr::ONE,
            }]
            .into(),
            inputs: InputNullifiers::default(),
            outputs: [NoteCommitment(Fr::from(output))].into(),
            program_outputs: ProgramOutputs::default(),
            commitment_merkle_root,
            spend_notes_hash: Fr::ZERO,
        })]
        .into()
    }

    #[test]
    fn a_program_registered_earlier_in_the_transaction_runs_its_step() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let channel = state
            .mantle_ledger()
            .channels()
            .channel_state(&channel_id)
            .unwrap()
            .clone();
        let channel_root = state.latest_note_sets().get(2).unwrap().commitment_root();
        let key = test_program_key();
        let inscribe = InscriptionOp {
            channel_id,
            inscription: [1].into(),
            parent: channel.tip_message,
            signer: verifying_key.into_unverified(),
            steps: program_steps(derive_program_id(&key), 11, 12, channel_root, 0),
        };

        let (state, _, execution_gas, _, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                program_step_tx(
                    vec![
                        Op::ProgramRegister(ProgramRegisterOp {
                            verification_key: key.clone(),
                        }),
                        Op::ChannelInscribe(inscribe),
                    ],
                    &signing_key,
                ),
            )
            .unwrap();

        assert_eq!(deferred.program_proofs()[0].inputs().program.key(), &key);
        assert_eq!(
            deferred.program_spends()[0]
                .inputs()
                .paid_value
                .into_inner(),
            Fr::from(5u64)
        );
        let set = state.latest_note_sets().get(2).unwrap();
        assert!(set.contains_nullifier(&NoteNullifier(Fr::from(11u64))));
        assert_eq!(
            set.commitment_root(),
            CommitmentMmr::new()
                .push(NoteCommitment(Fr::from(12u64)))
                .unwrap()
                .frontier_root()
        );
        assert_eq!(execution_gas, Gas::new(1_896 + 59 + 619 + 619 + 2_451));
    }

    #[test]
    fn an_unregistered_program_rejects_the_transaction() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let channel = state
            .mantle_ledger()
            .channels()
            .channel_state(&channel_id)
            .unwrap()
            .clone();
        let channel_root = state.latest_note_sets().get(2).unwrap().commitment_root();
        let inscribe = InscriptionOp {
            channel_id,
            inscription: [1].into(),
            parent: channel.tip_message,
            signer: verifying_key.into_unverified(),
            steps: program_steps(
                derive_program_id(&test_program_key()),
                11,
                12,
                channel_root,
                0,
            ),
        };

        let result = state.try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
            &test_config,
            program_step_tx(vec![Op::ChannelInscribe(inscribe)], &signing_key),
        );

        assert_eq!(
            result.err(),
            Some(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::UnknownProgram(derive_program_id(&test_program_key()))
                )
            ))
        );
    }

    #[test]
    fn a_payment_covers_a_deposit_of_the_same_transaction_with_buffered() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let channel = state
            .mantle_ledger()
            .channels()
            .channel_state(&channel_id)
            .unwrap()
            .clone();
        let channel_root = state.latest_note_sets().get(2).unwrap().commitment_root();
        let key = test_program_key();
        let deposit = DepositOp {
            channel_id,
            inputs: [NoteNullifier(Fr::ONE)].into(),
            commitment_merkle_root: ledger_root(&state),
            outputs: [NoteCommitment(Fr::from(10u64))].into(),
            excess_value: 0,
            metadata: Metadata::empty(),
        };
        let inscribe = InscriptionOp {
            channel_id,
            inscription: [1].into(),
            parent: channel.tip_message,
            signer: verifying_key.into_unverified(),
            steps: program_steps(derive_program_id(&key), 11, 12, channel_root, 1),
        };

        let (_, _, _, _, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(
                &test_config,
                program_step_tx(
                    vec![
                        Op::ProgramRegister(ProgramRegisterOp {
                            verification_key: key,
                        }),
                        Op::ChannelDeposit(deposit),
                        Op::ChannelInscribe(inscribe),
                    ],
                    &signing_key,
                ),
            )
            .unwrap();

        let covered = CommitmentMmr::new()
            .push(NoteCommitment(Fr::from(10u64)))
            .unwrap()
            .frontier_root();
        assert_eq!(
            deferred.zk_transfers()[1]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            covered
        ); // [0] is the deposit
        assert_eq!(
            deferred.program_spends()[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            covered
        );
    }

    #[test]
    fn withdrawal_waits_for_its_due_slot_and_adds_nothing_to_the_balance() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1u8; 32]);
        let state = create_channel(
            state,
            &test_config,
            channel_id,
            &signing_key,
            verifying_key.into_unverified(),
        );
        let channel_root = state.latest_note_sets().get(2).unwrap().commitment_root();
        let ledger_before = ledger_root(&state);
        let withdraw = ChannelWithdrawOp {
            channel_id,
            inputs: [NoteNullifier(Fr::from(5u64))].into(),
            commitment_merkle_root: channel_root,
            outputs: [NoteCommitment(Fr::from(50u64))].into(),
        };
        let tx = SignedOps::from_parts(
            Ops::from([Op::ChannelWithdraw(withdraw)]),
            OpProofs::from([OpProof::ZkTransfer(zero_proof())]),
        )
        .unwrap()
        .preverify()
        .unwrap();

        let (state, balance, _, events, deferred) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
            .unwrap();

        assert_eq!(balance, 0);
        assert_eq!(events, []);
        assert_eq!(deferred.zk_transfers().len(), 1);
        assert_eq!(ledger_root(&state), ledger_before);
        assert!(
            !state
                .latest_note_sets()
                .get(2)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::from(5u64)))
        );
        assert_eq!(
            state
                .cryptarchia_ledger
                .pending_withdrawals
                .iter()
                .map(|w| (w.due, w.note_set))
                .collect::<Vec<_>>(),
            vec![(state.slot().strict_add(Slot::from(WITHDRAW_DELAY)), 2)]
        );
    }

    #[test]
    fn due_withdrawals_are_released_before_the_transactions_of_the_block() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let (note_sets, channel_set) = state.cryptarchia_ledger.note_sets.clone().push_empty();
        state.cryptarchia_ledger.note_sets = note_sets;
        let ledger_before = state.latest_note_sets().get(LEDGER_SET).unwrap().clone();
        let withdrawal = PendingWithdrawal {
            due: Slot::from(1u64),
            note_set: channel_set,
            inputs: [NoteNullifier(Fr::from(7u64))].into(),
            outputs: [NoteCommitment(Fr::from(70u64))].into(),
        };
        state.cryptarchia_ledger.pending_withdrawals =
            PendingWithdrawals::new().push(withdrawal.clone());
        let proof = generate_proof(&state.cryptarchia_ledger, &note(), Slot::from(1u64));

        let (state, events) = state
            .try_apply_header::<_, HeaderId>(
                Slot::from(1u64),
                &proof,
                &UncleSlots::default(),
                &test_config,
            )
            .unwrap();

        assert_eq!(
            state.cryptarchia_ledger.pending_withdrawals.iter().count(),
            0
        );
        assert!(
            state
                .latest_note_sets()
                .get(channel_set)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::from(7u64)))
        );
        assert_eq!(
            state
                .latest_note_sets()
                .get(LEDGER_SET)
                .unwrap()
                .commitment_root(),
            ledger_before
                .execute_adding(&[NoteCommitment(Fr::from(70u64))])
                .unwrap()
                .commitment_root()
        );
        assert!(events.iter().any(|event| matches!(
            event,
            HeaderEvent::ChannelWithdrawalReleased { note_set, outputs, .. }
                if *note_set == channel_set && *outputs == withdrawal.outputs
        )));
    }

    // An inscription creates the channel when it does not exist, so the config
    // following it is verified against a threshold of 1 rather than 0. Predict
    // 0 and the transaction is funded short and rejected.
    #[test]
    fn test_fee_estimate_covers_inscribe_then_config_in_one_tx() {
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };
        let config_op = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };
        let ops = vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelConfig(config_op),
        ];

        let tx_hash = Ops::new_unchecked(ops.clone()).hash();
        let inscribe_key = Key::Ed25519(signing_key.clone());
        let config_key = Key::MultiSequencer(ChannelMultiSigProof::new(IndexedSignatures::from((
            0,
            signing_key.sign_payload(tx_hash.as_signing_bytes()),
        ))));
        let tx = create_multi_signed_tx(ops, vec![&inscribe_key, &config_key]);

        let prices = GasPrices::default();
        let gas_context =
            OpsGasContext::from_channels(state.mantle_ledger().channels(), prices.clone());
        let predicted = tx
            .op_refs()
            .total_gas_cost::<MainnetGasProfile>(&gas_context)
            .unwrap();

        let (_, _, execution_gas, _, deferred_proofs) = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx.clone())
            .unwrap();
        deferred_proofs.verify().unwrap();

        // The config is priced against the channel the inscription created.
        assert_eq!(execution_gas.into_inner(), 59 + 56);

        let storage_gas = Gas::new(tx.storage_size() as u64);
        let charged = GasCost::calculate(execution_gas, prices.execution_base_gas_price)
            .unwrap()
            .checked_add(GasCost::calculate(storage_gas, prices.storage_gas_price).unwrap())
            .unwrap();

        assert_eq!(predicted, charged);
    }

    #[test]
    fn test_invalid_parent_error() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([5; 32]);

        // First, create a channel with one message
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
            .unwrap()
            .0;

        // Now try to add a message with wrong parent
        let wrong_parent = MsgId::from([99; 32]);
        let second_inscribe = InscriptionOp {
            channel_id,
            inscription: [4, 5, 6].into(),
            parent: wrong_parent,
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let second_tx = create_signed_tx(
            Op::ChannelInscribe(second_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
        let result = state
            .clone()
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, second_tx);
        assert!(matches!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::InvalidParent { .. }
                )
            ))
        ));

        // Writing into an empty channel with a parent != MsgId::root() should also fail
        let empty_channel_id = ChannelId::from([8; 32]);
        let empty_inscribe = InscriptionOp {
            channel_id: empty_channel_id,
            inscription: [7, 8, 9].into(),
            parent: MsgId::from([1; 32]), // non-root parent
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let empty_tx = create_signed_tx(
            Op::ChannelInscribe(empty_inscribe),
            &Key::Ed25519(signing_key),
        );
        let empty_result =
            state.try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, empty_tx);
        assert!(matches!(
            empty_result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::InvalidParent { .. }
                )
            ))
        ));
    }

    #[test]
    fn test_unauthorized_signer_error() {
        let test_config = config();
        let mut state = LedgerState::from_notes([note()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let (unauthorized_signing_key, unauthorized_verifying_key) = create_test_keys_with_seed(3);
        let channel_id = ChannelId::from([6; 32]);

        // First, create a channel with authorized signer
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let correct_parent = first_inscribe.id();
        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key),
        );
        state = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
            .unwrap()
            .0;

        // Now try to add a message with unauthorized signer
        let second_inscribe = InscriptionOp {
            channel_id,
            inscription: [4, 5, 6].into(),
            parent: correct_parent,
            signer: unauthorized_verifying_key.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let second_tx = create_signed_tx(
            Op::ChannelInscribe(second_inscribe),
            &Key::Ed25519(unauthorized_signing_key),
        );
        let result = state
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, second_tx);
        assert!(matches!(
            result,
            Err(LedgerError::VerificationError(
                VerificationError::ChannelVerificationError(
                    mantle::channel::Error::UnauthorizedSigner { .. }
                )
            ))
        ));
    }

    #[test]
    fn test_multiple_operations_in_transaction() {
        // Create channel 1 by posting an inscription
        // Create channel 2 by posting an inscription
        // Change the keys for channel 1
        // Post another inscription in channel 1
        let test_config = config();
        let state = LedgerState::from_notes([note()], &test_config);
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
            signer: vk1.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let inscribe_op2 = InscriptionOp {
            channel_id: channel2,
            inscription: [4, 5, 6].into(),
            parent: MsgId::root(),
            signer: vk2.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let config_op = ChannelConfigOp {
            channel: channel1,
            parent: MsgId::root(),
            keys: [vk3, vk4].into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
        };

        let inscribe_op3 = InscriptionOp {
            channel_id: channel1,
            inscription: [7, 8, 9].into(),
            parent: inscribe_op1.id(),
            signer: vk3.into_unverified(),
            steps: ChannelSteps::default(),
        };

        let ops = vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
            Op::ChannelConfig(config_op.clone()),
            Op::ChannelInscribe(inscribe_op3.clone()),
        ];
        let config_tx = Ops::new_unchecked(ops.clone());
        let config_tx_hash = config_tx.hash();
        let config_proof = ChannelMultiSigProof::new(IndexedSignatures::from((
            0,
            sk1.sign_payload(config_tx_hash.as_signing_bytes()),
        )));

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
            .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
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
        assert_eq!(
            result
                .mantle_ledger
                .channels()
                .channels
                .get(&channel1)
                .unwrap()
                .config_tip_hash,
            config_op.id()
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
        let leader_utxo = note();
        let (sdp_utxo_key, sdp_utxo) = note_with_sk();
        let new_utxo_1 = note();
        let new_utxo_2 = note();
        let config = config();
        let epoch_length = config.epoch_length();
        let (mut ledger, genesis) = ledger(&[leader_utxo, sdp_utxo], config);

        // Declare at the first slot of epoch 3 — `active = 3 + 2 = 5`.
        let h_1 = update_ledger(&mut ledger, genesis, epoch_length, leader_utxo).unwrap();
        let h_2 = update_ledger(&mut ledger, h_1, 2 * epoch_length, leader_utxo).unwrap();
        let (h_3, declare, zk_key) = apply_and_add_utxo_and_declaration(
            &mut ledger,
            h_2,
            3 * epoch_length,
            leader_utxo,
            new_utxo_1,
            (sdp_utxo_key, sdp_utxo),
        );

        // Advance to the first slot of epoch 6 and submit an Active message.
        // This sets the live SDP's `active` to 6.
        let h_4 = update_ledger(&mut ledger, h_3, 4 * epoch_length, leader_utxo).unwrap();
        let h_5 = update_ledger(&mut ledger, h_4, 5 * epoch_length, leader_utxo).unwrap();
        let epoch5 = ledger.states[&h_5].cryptarchia_ledger.epoch_state.clone();
        let h_6 = apply_and_add_utxo_and_activity(
            &mut ledger,
            h_5,
            6 * epoch_length,
            &epoch5,
            leader_utxo,
            new_utxo_2,
            declare.id(),
            &zk_key,
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
    fn test_fee_rejection() {
        let config = config();
        let mut ledger = LedgerState::from_notes([note()], &config);
        update_ledger_prices(&mut ledger, 1, 1);
        let root = ledger_root(&ledger);

        let tx = create_tx(
            vec![NoteNullifier(Fr::ONE)],
            vec![NoteCommitment(Fr::from(2u64))],
            root,
            0,
        );
        let gas_context = OpsGasContext::from_channels(&Channels::new(), ledger.get_gas_prices());
        let fees = tx
            .op_refs()
            .total_gas_cost::<MainnetGasProfile>(&gas_context)
            .unwrap();
        let tx = create_tx(
            vec![NoteNullifier(Fr::ONE)],
            vec![NoteCommitment(Fr::from(2u64))],
            root,
            fees.into_inner(),
        )
        .preverify()
        .unwrap();

        let result = ledger
            .clone()
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                ZkPublicKey::zero(),
                std::iter::once(tx.clone()),
            );
        // The `unwrap` should succeed because the user pays at least the base fee of
        // 2705
        let _apply_result = result.unwrap();

        ledger.cryptarchia_ledger = ledger.cryptarchia_ledger.set_execution_base_fee(10.into());

        let err = ledger
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                ZkPublicKey::zero(),
                std::iter::once(tx),
            )
            .err()
            .unwrap();
        // The transaction should be rejected because the price indicated for execution
        // doesn't cover the base fee that cost 27 050
        assert_eq!(err, LedgerError::InsufficientBalance);
    }

    #[test]
    fn test_priority_fees_go_to_leader() {
        let config = config();
        let mut ledger = LedgerState::from_notes([note()], &config);
        let root = ledger_root(&ledger);

        let tx = create_tx(
            vec![NoteNullifier(Fr::ONE)],
            vec![NoteCommitment(Fr::from(2u64))],
            root,
            0,
        );
        update_ledger_prices(&mut ledger, 1, 1);
        let ledger_set_before = ledger.latest_note_sets().get(LEDGER_SET).unwrap().clone();
        let gas_context = OpsGasContext::from_channels(&Channels::new(), ledger.get_gas_prices());
        let fees = tx
            .op_refs()
            .total_gas_cost::<MainnetGasProfile>(&gas_context)
            .unwrap();
        let tx = create_tx(
            vec![NoteNullifier(Fr::ONE)],
            vec![NoteCommitment(Fr::from(2u64))],
            root,
            fees.into_inner(),
        )
        .preverify()
        .unwrap();

        let result = ledger
            .clone()
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                ZkPublicKey::zero(),
                std::iter::once(tx),
            );
        // The `unwrap` should succeed because the user pays at least the base fee of
        // 823
        let (no_priority_fee_ledger, events, _, _) = result.unwrap();
        assert_eq!(events, []);

        // The tx ays 1823 fees = 619 execution base fee + 1000 execution tip + 204
        // storage
        let tx = create_tx(
            vec![NoteNullifier(Fr::ONE)],
            vec![NoteCommitment(Fr::from(2u64))],
            root,
            fees.into_inner() + 1000,
        )
        .preverify()
        .unwrap();

        let result = ledger.try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
            &config,
            ZkPublicKey::zero(),
            std::iter::once(tx),
        );
        // The `unwrap` should succeed because the user pays at least the base fee of
        // 823
        let (priority_fee_ledger, events, _, _) = result.unwrap();

        // Stake 10_000 is far below target, so the block reward is the maximum release
        // whatever the fees; the leader note carries two fifths of it plus the tips.
        let with_leader_note = |value: Value| {
            ledger_set_before
                .clone()
                .execute_adding(&[
                    NoteCommitment(Fr::from(2u64)),
                    notes::Note::new(value, Fr::ZERO, ZkPublicKey::zero()).commitment(),
                ])
                .unwrap()
                .commitment_root()
        };
        assert_eq!(
            ledger_root(&no_priority_fee_ledger),
            with_leader_note(38_051_750_380)
        );
        assert_eq!(
            ledger_root(&priority_fee_ledger),
            with_leader_note(38_051_751_380)
        );
        assert_eq!(events, []);
    }

    #[test]
    fn test_apply_contents_accumulates_storage_gas() {
        let config = config();
        let mut ledger = LedgerState::from_notes([note()], &config);
        update_ledger_prices(&mut ledger, 1, 1);
        let root = ledger_root(&ledger);

        // No outputs: the whole input covers the gas cost and the remainder is
        // tipped. We only assert on the storage counter, not the tip, so the
        // exact balance is irrelevant.
        let tx = create_tx(vec![NoteNullifier(Fr::ONE)], vec![], root, 10_000)
            .preverify()
            .unwrap();

        // The tx must consume a non-zero amount of storage gas for the check to
        // be meaningful.
        let storage_gas = Gas::new(tx.storage_size() as u64);
        assert!(storage_gas.into_inner() > 0);

        let (applied, _, _, _) = ledger
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                ZkPublicKey::zero(),
                std::iter::once(tx),
            )
            .unwrap();

        // Storage gas consumed by the tx should be accumulated in the ledger
        assert_eq!(
            applied.cryptarchia_ledger.storage_gas_consumed_in_epoch(),
            storage_gas
        );
    }

    mod pow {
        use std::num::NonZeroU64;

        use lb_core::mantle::ops::{
            NoOpProof,
            pow::{ClaimPowRewardError, ClaimPowRewardOp, PowTarget},
        };

        use super::*;

        /// A reward config with claiming disabled (`rate_num = 0`), standing in
        /// for a real deployment config.
        fn disabled_reward_config() -> RewardPoWConfig {
            RewardPoWConfig {
                reward_pool_genesis: 1_000_000_000,
                epoch_reward_genesis: 1_000_000,
                minimum_difficulty: ModulusShift::new::<26>(),
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZeroU64::new(10).expect("10 is non-zero"),
                target_claims_per_block: 100,
                rate_num: 0,
                rate_den: NonZeroU64::MIN,
                target_claim_per_block: NonZeroU64::MIN,
                pow_share: 0,
                share_den: NonZeroU64::MIN,
                slot_window: NonZeroU64::new(100).expect("100 is non-zero"),
            }
        }

        /// A payout rate of `1/100`: `sigma_e = pool / 100`, used to give the
        /// `PoW` state a nonzero per-claim reward in tests. The denominator is
        /// `rate_den (1) * target_claim_per_block (10) *
        /// expected_blocks_per_epoch (10, derived from the test consensus
        /// schedule)`.
        fn test_pool_config() -> Config {
            let mut config = config();
            config.pow_config.reward = RewardPoWConfig {
                rate_num: 1,
                rate_den: NonZeroU64::MIN,
                target_claim_per_block: NonZeroU64::new(10).expect("10 is non-zero"),
                ..disabled_reward_config()
            };
            config
        }

        /// A ledger state with a funded `PoW` pool (1000, `sigma_e` = 10) and
        /// a seeded reward difficulty.
        fn pow_ledger_state(reward_difficulty: u64) -> (LedgerState, Config) {
            let config = config();
            let mut state = LedgerState::from_notes([note()], &config);
            state
                .mantle_ledger
                .pow
                .add_reward_refill_rewards(1_000)
                .unwrap();
            state
                .mantle_ledger
                .pow
                .add_rewards_to_pool(&test_pool_config())
                .unwrap();
            state
                .mantle_ledger
                .pow
                .set_reward_difficulty(PowTarget::from(reward_difficulty));
            (state, config)
        }

        fn claim_op() -> ClaimPowRewardOp {
            ClaimPowRewardOp {
                // The current epoch's randomness nonce, which for a fresh test
                // ledger built from genesis is the genesis nonce (`Fr::ZERO`).
                epoch_nonce: Fr::ZERO,
                block_hash: [1u8; 32],
                public_key: ZkPublicKey::new(Fr::from(42u64)),
                nonce: Fr::ZERO,
            }
        }

        /// Read the reward difficulty of a block's committed state.
        fn difficulty_at(ledger: &Ledger<HeaderId>, id: HeaderId) -> PowTarget {
            ledger
                .state(&id)
                .expect("block state should exist")
                .mantle_ledger
                .pow
                .reward_difficulty()
        }

        #[test]
        fn difficulty_eases_on_each_applied_block_without_claims() {
            // The retarget runs in `try_update`, on the canonical block-apply
            // path, once the block's contents have applied successfully.
            // `PoWDifficultySettings`: q = 9/10, T = 100. An empty block is
            // the largest easing step, a factor of P/F = 10/9 per block:
            //   900 -> 10·100·900/(9·100) = 1000 -> 1000000/900 = 1111.
            let test_utxo = note();
            let (mut test_ledger, genesis) = ledger(&[test_utxo], config());
            test_ledger
                .states
                .get_mut(&genesis)
                .expect("genesis state should exist")
                .mantle_ledger
                .pow
                .set_reward_difficulty(PowTarget::from(900u64));

            let block_1 = update_ledger(&mut test_ledger, genesis, 1, test_utxo)
                .expect("empty block should apply");
            assert_eq!(
                difficulty_at(&test_ledger, block_1),
                PowTarget::from(1_000u64)
            );

            let block_2 = update_ledger(&mut test_ledger, block_1, 2, test_utxo)
                .expect("empty block should apply");
            assert_eq!(
                difficulty_at(&test_ledger, block_2),
                PowTarget::from(1_111u64)
            );
        }

        #[test]
        fn difficulty_hardens_when_claims_exceed_the_target() {
            // Exercises the `LedgerState` plumbing directly with a claim
            // count: 2T claims shrink the target,
            // 1000 -> 10·100·1000/(1·200 + 9·100) = 909.
            let (mut state, config) = pow_ledger_state(1_000);

            state.update_pow_reward_difficulty(200, &config);

            assert_eq!(
                state.mantle_ledger.pow.reward_difficulty(),
                PowTarget::from(909u64)
            );
        }

        #[test]
        fn difficulty_is_seeded_at_genesis_and_the_controller_can_move_it() {
            // Genesis seeds a nonzero initial difficulty (zero would be an
            // absorbing state for the controller, with no ticket ever able
            // to satisfy it). It is also the minimum difficulty: an empty
            // block (no claims) cannot ease the target past it. The per-block
            // retarget still moves it the other way: excess claims harden it.
            let test_utxo = note();
            let config = config();
            let (mut test_ledger, genesis) = ledger(&[test_utxo], config.clone());
            let genesis_difficulty = difficulty_at(&test_ledger, genesis);
            assert_ne!(genesis_difficulty, Fr::ZERO);

            let block_1 = update_ledger(&mut test_ledger, genesis, 1, test_utxo)
                .expect("empty block should apply");
            assert_eq!(difficulty_at(&test_ledger, block_1), genesis_difficulty);

            let mut state = test_ledger
                .state(&block_1)
                .expect("block state should exist")
                .clone();
            state.update_pow_reward_difficulty(u64::MAX, &config);
            let hardened = state.mantle_ledger.pow.reward_difficulty();
            assert!(
                BigUint::from_bytes_le(&lb_groth16::fr_to_bytes(&hardened))
                    < BigUint::from_bytes_le(&lb_groth16::fr_to_bytes(&genesis_difficulty))
            );
        }

        fn claim_tx() -> SignedOps<Preverified, StandardMode> {
            let mantle_tx = Ops::from([Op::ClaimPowReward(claim_op())]);
            let op_proofs = OpProofs::from([OpProof::None(NoOpProof)]);
            SignedOps::from_parts(mantle_tx, op_proofs)
                .unwrap()
                .preverify()
                .expect("claim op with OpProof::None should pass preverification")
        }

        #[test]
        fn claim_tx_validation_rejects_disabled_rewards() {
            // End-to-end through `try_apply_tx_operations`: with `sigma_e` forced to
            // zero the claim fails the §5.6 safety cutoff. This exercises
            // the full wiring: preverification, the stateful
            // `ClaimPowReward` arm and the helper-built context.
            let config = config();
            let mut state = LedgerState::from_notes([note()], &config);
            // The default reward config disables claiming (`rate_num = 0`).
            state
                .mantle_ledger
                .pow
                .add_rewards_to_pool(&config)
                .unwrap();
            assert_eq!(state.mantle_ledger.pow.epoch_reward(), 0);

            let err = state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .err()
                .expect("claim should fail validation");

            assert!(matches!(
                err,
                LedgerError::VerificationError(VerificationError::ClaimPowRewardError(
                    ClaimPowRewardError::EmptyRewards
                ))
            ));
        }

        #[test]
        fn claim_tx_validation_rejects_unknown_anchor_block() {
            // With a funded pool the claim advances to the window-of-
            // acceptance check, which fails because the anchor block is not
            // among the ledger's recently seen blocks.
            let (state, config) = pow_ledger_state(1_000);

            let err = state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .err()
                .expect("claim should fail validation");

            assert!(matches!(
                err,
                LedgerError::VerificationError(VerificationError::ClaimPowRewardError(
                    ClaimPowRewardError::MissingBlock { block_id }
                )) if block_id == claim_op().block_hash
            ));
        }

        /// A funded state where `claim_tx()` passes full validation: max
        /// difficulty (every ticket wins) and the anchor block recorded as
        /// seen at the current slot.
        fn claim_accepting_state() -> (LedgerState, Config) {
            let (mut state, config) = pow_ledger_state(1_000);
            state
                .mantle_ledger
                .pow
                .set_reward_difficulty(-PowTarget::ONE);
            state
                .mantle_ledger
                .pow
                .add_seen_block_slots(claim_op().block_hash, Slot::from(0u64));
            (state, config)
        }

        #[test]
        fn claim_tx_end_to_end_pays_the_reward() {
            // The full §5.3 pipeline through `try_apply_tx_operations`: preverification,
            // helper-built validation context (pool, window, epoch nonce,
            // difficulty, double-claim) and execution.
            let (state, config) = claim_accepting_state();
            let pool_before = state.mantle_ledger.pow.reward_pool();
            let epoch_reward = state.mantle_ledger.pow.epoch_reward();
            let root_before = ledger_root(&state);

            let (state, _balance, _, events, deferred_proofs) = state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .expect("claim should validate and execute");
            deferred_proofs.verify().unwrap();

            assert_eq!(
                state.mantle_ledger.pow.reward_pool(),
                pool_before - epoch_reward
            );
            assert!(
                state
                    .mantle_ledger
                    .pow
                    .nullifiers()
                    .contains_key(&claim_op().get_puzzle_ticket())
            );
            let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
            assert_eq!(
                ledger_set.tx_commitment_root(&root_before, 0),
                Ok(ledger_set.commitment_root())
            );
            assert_ne!(ledger_set.commitment_root(), root_before);
            assert_eq!(events.len(), 1);
            let TxEventPayload::PoWRewardClaimed { note, .. } = &events[0].payload else {
                panic!("expected PoWRewardClaimed tx event");
            };
            assert_eq!(
                *note,
                notes::Note::new(
                    epoch_reward,
                    derive_note_nonce(&claim_op().op_id(), 0, epoch_reward, &claim_op().public_key),
                    claim_op().public_key
                )
            );
        }

        #[test]
        fn claim_tx_double_claim_is_rejected() {
            // Replaying the same solution is caught by the nullifier check
            // during tx-level validation.
            let (state, config) = claim_accepting_state();
            let (state, _, _, _, deferred_proofs) = state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .expect("first claim should succeed");
            deferred_proofs.verify().unwrap();

            let err = state
                .try_apply_tx_operations::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .err()
                .expect("second claim should be rejected");

            assert!(matches!(
                err,
                LedgerError::VerificationError(VerificationError::ClaimPowRewardError(
                    ClaimPowRewardError::DoubleClaimed
                ))
            ));
        }

        #[test]
        fn claim_execution_pays_the_reward_and_updates_the_ledger() {
            // The execution path (`try_apply_op`): the claim drains sigma_e
            // from the pool, records the nullifier, adds the reward note to
            // the ledger set and emits the claim event.
            let (mut state, config) = pow_ledger_state(1_000);
            // Execution reads the anchor block's slot from the seen-block
            // map (validation, skipped here, guarantees its presence), so
            // record it directly.
            state
                .mantle_ledger
                .pow
                .add_seen_block_slots(claim_op().block_hash, Slot::from(0u64));
            let pool_before = state.mantle_ledger.pow.reward_pool();
            let epoch_reward = state.mantle_ledger.pow.epoch_reward();
            let root_before = ledger_root(&state);
            let op = claim_op();
            let signed_operation =
                SignedOperation::<_, _, StandardMode>::new(op.clone(), NoOpProof)
                    .into_state_trusted();
            let tx_hash = TxHash::from([9u8; 32]);

            let (state, _balance, events) = state
                .try_apply_op::<HeaderId, MainnetGasProfile>(
                    signed_operation.into(),
                    &config,
                    &tx_hash,
                    0,
                    Vec::new(),
                )
                .expect("claim execution should succeed");

            assert_eq!(
                state.mantle_ledger.pow.reward_pool(),
                pool_before - epoch_reward
            );
            assert!(
                state
                    .mantle_ledger
                    .pow
                    .nullifiers()
                    .contains_key(&op.get_puzzle_ticket())
            );

            let ledger_set = state.latest_note_sets().get(LEDGER_SET).unwrap();
            assert_eq!(
                ledger_set.tx_commitment_root(&root_before, 0),
                Ok(ledger_set.commitment_root())
            );
            assert_ne!(ledger_set.commitment_root(), root_before);

            assert_eq!(events.len(), 1);
            assert!(matches!(
                &events[0],
                TxEvent {
                    tx_hash: event_tx_hash,
                    payload: TxEventPayload::PoWRewardClaimed { .. },
                    ..
                } if *event_tx_hash == tx_hash
            ));
            let TxEventPayload::PoWRewardClaimed { note, .. } = &events[0].payload else {
                panic!("expected PoWRewardClaimed tx event");
            };
            assert_eq!(
                *note,
                notes::Note::new(
                    epoch_reward,
                    derive_note_nonce(&claim_op().op_id(), 0, epoch_reward, &claim_op().public_key),
                    claim_op().public_key
                )
            );
        }

        #[test]
        fn claim_execution_alone_has_no_double_claim_guard() {
            // `try_apply_op` is execute-only by design (like the other op
            // arms): the double-claim check lives in
            // `ClaimPowRewardOp::validate`, which `try_apply_tx_operations` runs via
            // `verify_stateful_op` before execution (see
            // `claim_tx_double_claim_is_rejected`). Calling the execution
            // path directly therefore pays the same solution twice — pinned
            // here to document that the guard lives in validation, not
            // execution.
            let (mut state, config) = pow_ledger_state(1_000);
            // Execution reads the anchor block's slot from the seen-block
            // map (validation, skipped here, guarantees its presence), so
            // record it directly.
            state
                .mantle_ledger
                .pow
                .add_seen_block_slots(claim_op().block_hash, Slot::from(0u64));
            let pool_before = state.mantle_ledger.pow.reward_pool();
            let epoch_reward = state.mantle_ledger.pow.epoch_reward();
            let op = claim_op();
            let signed_operation =
                SignedOperation::<_, _, StandardMode>::new(op, NoOpProof).into_state_trusted();

            let tx_hash = TxHash::from([9u8; 32]);

            let (state, _, _) = state
                .try_apply_op::<HeaderId, MainnetGasProfile>(
                    signed_operation.clone().into(),
                    &config,
                    &tx_hash,
                    0,
                    Vec::new(),
                )
                .expect("first claim should succeed");
            let (state, _, _) = state
                .try_apply_op::<HeaderId, MainnetGasProfile>(
                    signed_operation.into(),
                    &config,
                    &tx_hash,
                    0,
                    Vec::new(),
                )
                .expect("second claim currently also succeeds (no validation)");

            assert_eq!(
                state.mantle_ledger.pow.reward_pool(),
                pool_before - 2 * epoch_reward
            );
        }

        #[test]
        fn block_fees_refill_the_pow_pool_by_the_configured_share() {
            // A tenth of the collected fees is diverted to the PoW refill and
            // the rest is pooled for block rewards.
            let mut config = config();
            config.pow_config.reward.pow_share = 10;
            config.pow_config.reward.share_den = NonZeroU64::new(100).unwrap();
            let mut state = LedgerState::from_notes([note()], &config);
            let pool_before = state.mantle_ledger.pow.reward_pool();

            (state, _) = state
                .compute_block_rewards::<HeaderId>(
                    1_000.into(),
                    0.into(),
                    ZkPublicKey::zero(),
                    &config.pow_config.reward,
                )
                .expect("reward computation should succeed");

            // The pooled fees of the block are the last entry of the window.
            assert_eq!(
                state.cryptarchia_ledger.block_rewards.pooled_fees_window()[POOLED_FEES_WINDOW - 1],
                900
            );
            state
                .mantle_ledger
                .pow
                .add_rewards_to_pool(&test_pool_config())
                .unwrap();
            assert_eq!(state.mantle_ledger.pow.reward_pool(), pool_before + 100);
        }

        #[test]
        fn block_fees_do_not_refill_the_pow_pool_while_the_share_is_zero() {
            // With `pow_share = 0` every collected fee is pooled for block
            // rewards and nothing accrues to the PoW refill.
            let config = config();
            let mut state = LedgerState::from_notes([note()], &config);
            let pool_before = state.mantle_ledger.pow.reward_pool();

            (state, _) = state
                .compute_block_rewards::<HeaderId>(
                    1_000.into(),
                    0.into(),
                    ZkPublicKey::zero(),
                    &config.pow_config.reward,
                )
                .expect("reward computation should succeed");

            assert_eq!(
                state.cryptarchia_ledger.block_rewards.pooled_fees_window()[POOLED_FEES_WINDOW - 1],
                1_000
            );
            state
                .mantle_ledger
                .pow
                .add_rewards_to_pool(&test_pool_config())
                .unwrap();
            assert_eq!(state.mantle_ledger.pow.reward_pool(), pool_before);
        }
    }
}
