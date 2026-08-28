pub mod config;
// The ledger is split into two modules:
// - `cryptarchia`: the base functionalities needed by the Cryptarchia consensus
//   algorithm, including a minimal UTxO model.
// - `mantle_ops`: our extensions in the form of Mantle operations, e.g. SDP.
pub mod cryptarchia;
mod intent;
pub mod mantle;
mod update;

use std::hash::Hash;

pub use config::Config;
use cryptarchia::LedgerState as CryptarchiaLedger;
pub use cryptarchia::{EpochState, UtxoTree};
pub use intent::{Intent, IntentStatus};
use lb_core::{
    block::BlockNumber,
    crypto::Hash as BlockHash,
    events::{Events, HeaderEvent, TxEvent, TxEventPayload},
    mantle::{
        NoteId, TxGasCalculator, Utxo, Value, VerificationError,
        batch::DeferredZkpVerifications,
        gas::{Gas, GasCost, GasOverflow, GasProfile},
        ledger::verification_mode::StandardMode,
        ops::{
            SignedOp,
            channel::{
                channel_transfer::ChannelTransferExecutionContext,
                deposit::DepositExecutionContext, withdraw::WithdrawExecutionContext,
            },
            leader_claim::LeaderClaimExecutionContext,
            pow::{ClaimPoWRewardExecutionContext, PowReward},
            sdp::{SDPActiveOp, SDPDeclareOp},
        },
        traits::{GenesisTx, PreverifiedMantleTransaction, genesis::GenesisOps},
        transactions::{
            GasPrices,
            hash::TxHash,
            states::Verified,
            tx_list::ops::{OpsContext, OpsGasContext},
        },
    },
    proofs::leader_proof,
};
use lb_cryptarchia_engine::{Slot, UncleSlots};
use lb_groth16::{AdditiveGroup as _, Fr};
use mantle::LedgerState as MantleLedger;
use rpds::HashTrieMapSync;
use thiserror::Error;

use crate::{
    mantle::helpers::MantleOperationVerificationHelper,
    update::{BatchVerifiedUpdate, PreparedUpdate},
};

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

// `POW` related rewards
// TODO: Activate this, currently is 0 based to keep original behaviour
// (blend+leadership)
const POW_REWARD_SHARE_NUMERATOR: u128 = 0;
const POW_REWARD_SHARE_DENOMINATOR: u128 = 4;
const EXECUTION_GAS_LIMIT: Gas = Gas::new(3_193_460);

// While individual notes are constrained to be `u64`, intermediate calculations
// may overflow, so we use `i128` to avoid that and to easily represent negative
// balances which may arise in special circumstances (e.g. rewards calculation).
pub type Balance = i128;

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
    #[error("Inputs error: {0}")]
    Inputs(#[from] lb_core::mantle::ledger::InputsError),
    #[error("Mantle error: {0}")]
    Outputs(#[from] lb_core::mantle::ledger::OutputsError),
    #[error("Input note in genesis block: {0:?}")]
    InputInGenesis(NoteId),
    #[error("The first Transfer Operation is missing in genesis tx")]
    MissingTransferGenesis(),
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
        Tx: PreverifiedMantleTransaction + TxGasCalculator<Context = GasPrices> + Clone,
        LeaderProof: leader_proof::LeaderProof,
        Profile: GasProfile,
        Id: Into<BlockHash>,
    {
        let parent_state = self
            .states
            .get(&parent_id)
            .ok_or(LedgerError::ParentNotFound(parent_id))?;

        let (new_state, events, deferred_zkps) = parent_state
            .clone()
            .try_update::<_, _, _, Profile>(id, slot, proof, uncle_slots, txs, &self.config)?;

        Ok(PreparedUpdate::new(id, new_state, events, deferred_zkps))
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
    ) -> Result<(Self, Events, DeferredZkpVerifications), LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction + TxGasCalculator<Context = GasPrices> + Clone,
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
        let (mut state, tx_events, deferred_zkp) = state.try_apply_contents::<_, _, Profile>(
            config,
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
            .collect::<Events>();
        Ok((state, events, deferred_zkp))
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
            *proof.voucher_cm(),
            config,
        )?;

        // Insert reward UTXOs into the cryptarchia ledger
        for utxo in effect.reward_utxos {
            cryptarchia_ledger.utxos = cryptarchia_ledger.utxos.insert(utxo.id(), utxo).0;
        }

        Ok((
            Self {
                block_number: self
                    .block_number
                    .checked_add(1)
                    .expect("Logos blockchain lived long and prospered"),
                cryptarchia_ledger,
                mantle_ledger,
            },
            effect.events,
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
    /// the last `BLOCK_REWARD_WINDOW_SIZE` blocks. See the block rewards
    /// specification: <https://lip.logos.co/blockchain/raw/block-rewards.html>
    fn compute_block_rewards(
        mut self,
        total_fee_burned: GasCost,
        total_fee_tip: GasCost,
    ) -> Result<Self, GasOverflow> {
        let window_index = self.block_number as usize % WINDOW_SIZE;

        // First update the fee burned in the block
        self.cryptarchia_ledger
            .update_fee_window(window_index, total_fee_burned);

        // Then compute the amount of block rewards

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

        // Blend gets 60% of block rewards while leaders get the 40% remaining + the
        // tips. Casting as Value truncates the floating points
        let blend_reward = (reward_numerator * BLEND_REWARD_SHARE_NUMERATOR
            / (reward_denominator * BLEND_REWARD_SHARE_DENOMINATOR))
            as Value;
        let leader_reward = GasCost::from(
            (reward_numerator * LEADER_REWARD_SHARE_NUMERATOR
                / (reward_denominator * LEADER_REWARD_SHARE_DENOMINATOR)) as Value,
        )
        .checked_add(total_fee_tip)?;

        let pow_reward: PowReward = ((reward_numerator * POW_REWARD_SHARE_NUMERATOR)
            / (reward_denominator * POW_REWARD_SHARE_DENOMINATOR))
            .try_into()
            .map_err(|_e| GasOverflow)?;

        self.mantle_ledger.leaders = self
            .mantle_ledger
            .leaders
            .add_pending_rewards(leader_reward.into_inner());

        self.mantle_ledger.sdp.add_blend_income(blend_reward);
        self.mantle_ledger.pow.add_reward_refill_rewards(pow_reward);

        Ok(self)
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

    /// Apply the contents of an update to the ledger state.
    pub fn try_apply_contents<Tx, Id, Profile: GasProfile>(
        mut self,
        config: &Config,
        txs: impl Iterator<Item = Tx>,
    ) -> Result<(Self, Vec<TxEvent>, DeferredZkpVerifications), LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction + TxGasCalculator<Context = GasPrices> + Clone,
    {
        let mut total_block_execution_gas: Gas = 0.into();
        let mut total_block_storage_gas: Gas = 0.into();
        let mut total_fee_burned: GasCost = 0.into();
        let mut total_fee_tip: GasCost = 0.into();
        let mut tx_events = Vec::new();
        let mut deferred_zkps = DeferredZkpVerifications::new();

        for tx in txs {
            let balance;
            let events;
            let deferred;
            (self, balance, events, deferred) =
                self.try_apply_tx::<_, _, Profile>(config, tx.clone())?;
            tx_events.extend(events);
            deferred_zkps.extend(deferred);

            let gas_prices = GasPrices {
                execution_base_gas_price: *self.cryptarchia_ledger.execution_base_fee(),
                storage_gas_price: *self.cryptarchia_ledger.storage_gas_price(),
            };
            // Check the transaction is balanced
            let total_gas_cost = tx.total_gas_cost::<Profile>(&gas_prices)?;
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
                tx.execution_gas_consumption::<Profile>(&gas_prices)?,
                gas_prices.execution_base_gas_price,
            )?
            .checked_add(tx.storage_gas_cost(&gas_prices)?)?;

            let tx_fee_tip = GasCost::from(balance as Value).checked_sub(tx_fee_burned)?;
            total_fee_burned = total_fee_burned.checked_add(tx_fee_burned)?;
            total_fee_tip = total_fee_tip.checked_add(tx_fee_tip)?;
            total_block_execution_gas = total_block_execution_gas
                .checked_add(tx.execution_gas_consumption::<Profile>(&gas_prices)?)?;
            total_block_storage_gas =
                total_block_storage_gas.checked_add(tx.storage_gas_consumption(&gas_prices)?)?;

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
        // Accumulate storage gas consumed so the storage market can update the
        // price at the next epoch rotation.
        self = self.add_storage_gas_consumed(total_block_storage_gas)?;
        Ok((self, tx_events, deferred_zkps))
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
    ) -> Result<(Self, Vec<TxEvent>), LedgerError<Id>> {
        let GenesisOps {
            transfer,
            inscription,
            declarations,
        } = tx.into_genesis_ops();
        let cryptarchia_ledger =
            CryptarchiaLedger::from_genesis_tx(&transfer, config, epoch_nonce)?;
        let (mantle_ledger, events) = MantleLedger::from_genesis_tx(
            inscription,
            declarations,
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
        self.cryptarchia_ledger.epoch_state_for_slot(
            slot,
            &self.mantle_ledger.sdp,
            &self.mantle_ledger.pow,
            config,
        )
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
    pub fn tx_context(&self) -> OpsContext {
        OpsContext {
            gas_context: OpsGasContext::from_channels(
                self.mantle_ledger().channels(),
                self.get_gas_prices(),
            ),
            leader_reward_amount: self.mantle_ledger().leader_reward_amount(),
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
            diagnostic = "blend_tsi_outage",
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
            diagnostic = "blend_tsi_outage",
            event = "sdp_activity_applied",
            evaluation_context = "candidate",
            canonical = false,
            proof_epoch = u32::from(op.metadata.origin_epoch()),
            ledger_epoch = u32::from(epoch),
            ledger_slot = u64::from(slot),
            tx_id = ?tx_hash,
            provider_id = ?new_declaration.provider_id,
            declaration_id = ?op.declaration_id,
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
                let (result, events) = self.mantle_ledger.try_apply_channel_inscription(
                    signed_operation,
                    self.cryptarchia_ledger.slot,
                )?;
                self.mantle_ledger = result;
                tx_events.extend(events);
            }
            SignedOp::ChannelConfig(signed_operation) => {
                let (result, events) = self
                    .mantle_ledger
                    .try_apply_channel_config(signed_operation, self.cryptarchia_ledger.slot)?;
                self.mantle_ledger = result;
                tx_events.extend(events);
            }
            SignedOp::ChannelDeposit(signed_operation) => {
                let channels = self.mantle_ledger.channels();
                let utxos = self.cryptarchia_ledger.latest_utxos();

                // Execute the Deposit
                let (result, events) = signed_operation
                    .execute(DepositExecutionContext {
                        channels: channels.clone(),
                        utxos: utxos.clone(),
                        tx_hash: *tx_hash,
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::Channel(error))?;
                self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
                tx_events.extend(events);
            }
            SignedOp::ChannelWithdraw(signed_operation) => {
                let channels = self.mantle_ledger.channels();

                let (result, events) = signed_operation
                    .execute(WithdrawExecutionContext {
                        channels: channels.clone(),
                        tx_hash: *tx_hash,
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::Channel(error))?;
                self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                tx_events.extend(events);
            }
            SignedOp::ChannelTransfer(signed_operation) => {
                let channels = self.mantle_ledger.channels();
                let utxos = self.cryptarchia_ledger.latest_utxos();

                let context = ChannelTransferExecutionContext {
                    channels: channels.clone(),
                    utxos: utxos.clone(),
                    tx_hash: *tx_hash,
                };
                let (result, events) = signed_operation
                    .execute(context)
                    .map_err(|(_signed_operation, error)| mantle::Error::Channel(error))?;
                self.mantle_ledger = self.mantle_ledger.update_channels(result.channels);
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
                tx_events.extend(events);
            }
            SignedOp::SDPDeclare(signed_operation) => {
                let operation = signed_operation.operation();
                let (mantle_ledger, events) = self.mantle_ledger.try_apply_sdp_declaration(
                    signed_operation,
                    self.cryptarchia_ledger.latest_utxos(),
                    config,
                )?;
                self.mantle_ledger = mantle_ledger;
                self.log_sdp_declaration_evaluation(operation, &self.mantle_ledger, config);
                tx_events.extend(events);
            }
            SignedOp::SDPActive(signed_operation) => {
                let operation = signed_operation.operation();
                let previous_active_epoch = tracing::enabled!(tracing::Level::TRACE).then(|| {
                    self.mantle_ledger
                        .sdp_ledger()
                        .get_declaration(&operation.declaration_id)
                        .map(|declaration| declaration.active)
                });
                let (mantle_ledger, events) =
                    self.mantle_ledger.try_apply_sdp_active(op, config)?;
                self.mantle_ledger = mantle_ledger;
                self.log_sdp_activity_evaluation(
                    operation,
                    &self.mantle_ledger,
                    config,
                    tx_hash,
                    previous_active_epoch.flatten(),
                );
                tx_events.extend(events);
            }
            SignedOp::SDPWithdraw(signed_operation) => {
                let (result, events) = self
                    .mantle_ledger
                    .try_apply_sdp_withdraw(signed_operation, config)?;
                self.mantle_ledger = result;
                tx_events.extend(events);
            }
            SignedOp::LeaderClaim(signed_operation) => {
                let (result, events) = signed_operation
                    .execute(LeaderClaimExecutionContext {
                        nullifiers: self.mantle_ledger.leaders.nullifiers_cloned(),
                        reward_amount: self.mantle_ledger.leaders.reward_amount(),
                        claimable_rewards: self.mantle_ledger.leaders.claimable_rewards(),
                        utxos: self.cryptarchia_ledger.latest_utxos().clone(),
                        tx_hash: *tx_hash,
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::LeaderClaim(error))?;
                self.mantle_ledger
                    .leaders
                    .update_nullifiers(result.nullifiers);
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);

                self.mantle_ledger
                    .leaders
                    .update_rewards(result.claimable_rewards);
                tx_events.extend(events);
            }
            SignedOp::Transfer(signed_operation) => {
                let transfer_balance;
                let events;
                (self.cryptarchia_ledger, transfer_balance, events) =
                    self.cryptarchia_ledger
                        .try_apply_transfer::<_, Profile>(signed_operation)?;
                balance = balance
                    .checked_add(transfer_balance)
                    .ok_or(LedgerError::BalanceOverflow)?;
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
                        utxos: self.cryptarchia_ledger.latest_utxos().clone(),
                        block_slots: self.mantle_ledger.pow.block_slots().clone(),
                    })
                    .map_err(|(_signed_operation, error)| mantle::Error::ClaimPow(error))?;
                self.mantle_ledger
                    .pow
                    .update_from_claim_execution_result(&result);
                self.cryptarchia_ledger = self.cryptarchia_ledger.update_utxos(result.utxos);
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
    fn try_apply_tx<Tx, Id, Profile: GasProfile>(
        mut self,
        config: &Config,
        tx: Tx,
    ) -> Result<(Self, Balance, Vec<TxEvent>, DeferredZkpVerifications), LedgerError<Id>>
    where
        Tx: PreverifiedMantleTransaction,
    {
        let mut verified_ops = tx.into_verified_operations();

        let mut balance: Balance = 0;
        let mut tx_events = Vec::new();
        let mut deferred_zkps = DeferredZkpVerifications::new();

        loop {
            let helper = MantleOperationVerificationHelper::new(
                &self.mantle_ledger,
                &self.cryptarchia_ledger,
                config,
            );

            // On Error (failed verification), the transaction is considered invalid and
            // rejected.
            let Some((op, deferred_zkp)) = verified_ops.next(&helper).transpose()? else {
                // All operations have been processed, exit the loop.
                break;
            };

            if let Some(deferred) = deferred_zkp {
                deferred_zkps.push(deferred);
            }

            (self, balance, tx_events) = self.try_apply_op::<_, Profile>(
                op,
                config,
                verified_ops.tx_hash_view().tx_hash(),
                balance,
                tx_events,
            )?;
        }

        Ok((self, balance, tx_events, deferred_zkps))
    }

    fn update_pow_reward_difficulty(&mut self, claims_in_block: u64, config: &Config) {
        self.mantle_ledger
            .pow
            .update_difficulty(claims_in_block, &config.pow_config.reward);
    }
}

#[cfg(test)]
mod tests {
    use cryptarchia::tests::{config, generate_proof, utxo};
    use lb_core::{
        events::DepositNote,
        mantle::{
            Note, Op, OpProof, SignedOps,
            gas::MainnetGasProfile,
            ledger::{Inputs, Outputs, Utxos, VerifiableOperation as _},
            ops::{
                OpId as _, OpRef, SignedOperation,
                channel::{
                    ChannelId, MsgId,
                    config::ChannelConfigOp,
                    deposit::{DepositOp, Metadata},
                    inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
                leader_claim::{LeaderClaimError, LeaderClaimOp, LeaderClaimVerificationContext},
                transfer::TransferOp,
            },
            traits::Hashable as _,
            transactions::{
                OpProofs, Ops,
                hash::TxHashView,
                states::{Preverified, Unverified},
            },
        },
        proofs::{
            channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
            leader_claim_proof::Groth16LeaderClaimProof,
        },
        sdp::{ActivityMetadata, DeclarationId, Nonce},
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{CompressedGroth16Proof, Field as _};
    use lb_key_management_system_keys::keys::{
        Ed25519Key, Ed25519PublicKey, ZkKey, ZkPublicKey, ZkSignature,
    };
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        cryptarchia::tests::{
            apply_and_add_utxo, apply_and_add_utxo_and_declaration, declaration_in_snapshot,
            ledger, update_ledger, utxo_with_sk,
        },
        mantle::{leader::LeaderState, sdp::test_utils::generate_activity_proof},
    };

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    type HeaderId = [u8; 32];

    fn create_tx(
        inputs: Vec<NoteId>,
        outputs: Vec<Note>,
        sks: &[ZkKey],
    ) -> SignedOps<Unverified, StandardMode> {
        let transfer_op = TransferOp::new(
            Inputs::try_new(inputs).expect("Invalid inputs size"),
            Outputs::try_new(outputs).expect("Invalid outputs size"),
        );
        let mantle_tx = Ops::from([Op::Transfer(transfer_op)]);
        let ops_proofs = OpProofs::from([OpProof::ZkSig(
            ZkKey::multi_sign(sks, &mantle_tx.hash().to_fr()).unwrap(),
        )]);
        SignedOps::from_parts(mantle_tx, ops_proofs).unwrap()
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
                Key::Ed25519(key) => {
                    OpProof::Ed25519Sig(key.sign_payload(tx_hash.as_signing_bytes().as_ref()))
                }
                Key::Zk(key) => OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(key), &tx_hash.to_fr()).unwrap(),
                ),
                Key::EmptyZk => OpProof::ZkSig(ZkKey::multi_sign(&[], &tx_hash.to_fr()).unwrap()),
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
        verifying_key: Ed25519PublicKey,
    ) -> LedgerState {
        let tx = create_signed_tx(
            Op::ChannelInscribe(InscriptionOp {
                channel_id: id,
                inscription: [1, 2, 3, 4].into(),
                parent: MsgId::root(),
                signer: verifying_key,
            }),
            &Key::Ed25519(signing_key.clone()),
        );
        ledger_state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(config, tx)
            .unwrap()
            .0
    }

    fn create_config_tx(
        config_op: ChannelConfigOp,
        signing_key: &Ed25519Key,
    ) -> SignedOps<Preverified, StandardMode> {
        let ops = Ops::from([Op::ChannelConfig(config_op.clone())]);
        let config_tx_hash = ops.hash();
        let config_proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                signing_key.sign_payload(config_tx_hash.as_signing_bytes().as_ref()),
            )]
            .into(),
        )
        .unwrap();

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
        utxo_proof: Utxo,
        utxo_add: Utxo,
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
        let signed_operation = SignedOperation::new(
            active_op,
            ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
        )
        .into_state_trusted();

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

        let default_gas_prices = GasPrices::default();
        let fees = tx
            .total_gas_cost::<MainnetGasProfile>(&default_gas_prices)
            .unwrap();
        output_note.value = utxo.note.value - fees.into_inner();

        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk])
            .preverify()
            .unwrap();
        let output_utxo = if let OpRef::Transfer(transfer_op) = tx.op_refs().get(0).unwrap() {
            transfer_op.outputs.utxo_by_index(0, transfer_op).unwrap()
        } else {
            panic!("first op must be a transfer")
        };

        // Create a dummy proof (using same structure as in cryptarchia tests)

        let proof = generate_proof(
            &ledger.state(&genesis_id).unwrap().cryptarchia_ledger,
            &utxo,
            Slot::from(1u64),
        );

        let new_id = [1; 32];
        let update = ledger
            .prepare_update::<_, _, MainnetGasProfile>(
                new_id,
                genesis_id,
                Slot::from(1u64),
                &proof,
                &UncleSlots::default(),
                std::iter::once(tx),
            )
            .unwrap()
            .verify_batch_proofs()
            .unwrap();
        let events = ledger.commit_update(update);
        assert!(events.is_empty());

        // Verify the transaction was applied
        let new_state = ledger.state(&new_id).unwrap();
        assert!(!new_state.latest_utxos().contains(&utxo.id()));

        // Verify output was created
        assert!(new_state.latest_utxos().contains(&output_utxo.id()));
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
        let result = state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _, events, deferred_zkps) = result.unwrap();
        deferred_zkps.verify().unwrap();

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
            parent: MsgId::root(),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };

        let config_tx = Ops::from([Op::ChannelConfig(config_op.clone())]);
        let config_tx_hash = config_tx.hash();
        let config_proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                signing_key.sign_payload(config_tx_hash.as_signing_bytes().as_ref()),
            )]
            .into(),
        )
        .unwrap();

        let tx = create_signed_tx(
            Op::ChannelConfig(config_op),
            &Key::MultiSequencer(config_proof),
        );
        let result = state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx);
        assert!(result.is_ok());

        let (new_state, _, events, deferred_zkps) = result.unwrap();
        deferred_zkps.verify().unwrap();

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
    fn test_jit_config_requires_zero_parent() {
        let test_config = config();
        let state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // The channel doesn't exist yet, so any parent but ZERO is dangling
        let dangling_config = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::from([1; 32]),
            keys: verifying_key.into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let dangling_tx = create_config_tx(dangling_config.clone(), &signing_key);
        let result = state
            .clone()
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, dangling_tx);
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
        let rooted_tx = create_config_tx(rooted_config.clone(), &signing_key);
        let new_state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, rooted_tx)
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
        let mut state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // Create the channel, its config tip stays ZERO
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key,
        };
        let mut tip = first_inscribe.id();
        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
        state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
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
            transfer_threshold: 1,
        };
        let config_tx = create_config_tx(config_op.clone(), &signing_key);

        // Inscriptions keep extending the channel, none of them moves the config tip
        for byte in [4u8, 5, 6] {
            let inscribe = InscriptionOp {
                channel_id,
                inscription: [byte].into(),
                parent: tip,
                signer: verifying_key,
            };
            tip = inscribe.id();
            let tx = create_signed_tx(
                Op::ChannelInscribe(inscribe),
                &Key::Ed25519(signing_key.clone()),
            );
            state = state
                .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
                .unwrap()
                .0;
        }

        // The configuration is still valid and leaves the message tip untouched
        let new_state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, config_tx)
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
        let mut state = LedgerState::from_utxos([utxo()], &test_config);
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
            transfer_threshold: 1,
        };
        let config1_tx = create_config_tx(config1.clone(), &signing_key);
        state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, config1_tx.clone())
            .unwrap()
            .0;
        let config2 = ChannelConfigOp {
            parent: config1.id(),
            ..config1.clone()
        };
        let config2_tx = create_config_tx(config2, &signing_key);
        state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, config2_tx)
            .unwrap()
            .0;

        // The superseded configuration cannot be re-included after a
        // reorganization abandons its block
        let result = state
            .clone()
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, config1_tx);
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
        let result = state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, sibling_tx);
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
        let mut state = LedgerState::from_utxos([utxo()], &test_config);
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([0u8; 32]);

        // Create the channel, the message tip is now I1
        let first_inscribe = InscriptionOp {
            channel_id,
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key,
        };
        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe.clone()),
            &Key::Ed25519(signing_key.clone()),
        );
        state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
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
            transfer_threshold: 1,
        };
        let config_tx = create_config_tx(config_op.clone(), &signing_key);
        state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, config_tx)
            .unwrap()
            .0;

        // An inscription signed before the configuration still extends I1
        let second_inscribe = InscriptionOp {
            channel_id,
            inscription: [4, 5, 6].into(),
            parent: first_inscribe.id(),
            signer: verifying_key,
        };
        let second_tx = create_signed_tx(
            Op::ChannelInscribe(second_inscribe.clone()),
            &Key::Ed25519(signing_key),
        );
        let new_state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, second_tx)
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
        let tx_hash = tx.hash();
        let result = ledger_state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx);
        let (new_state, balance, events, deferred_zkps) = result.unwrap();
        deferred_zkps.verify().unwrap();

        // The deposited note is consumed and re-created as a channel note under
        // a new NoteId.
        let deposited = Utxo::new(deposit.op_id(), 0, utxo.note).id();
        assert!(!new_state.latest_utxos().contains(&utxo.id()));
        assert!(
            !new_state
                .mantle_ledger()
                .channels()
                .is_channel_note(&utxo.id())
        );
        assert!(new_state.latest_utxos().contains(&deposited));
        assert!(
            new_state
                .mantle_ledger()
                .channels()
                .is_channel_note(&deposited)
        );
        assert_eq!(balance, Balance::from(0));

        assert_eq!(events.len(), 1);
        let Some(TxEvent {
            tx_hash: event_tx_hash,
            op_id,
            payload:
                TxEventPayload::Deposit {
                    channel_id: event_channel_id,
                    amount,
                    metadata,
                    notes,
                },
        }) = events.iter().find(|event| {
            matches!(
                event,
                TxEvent {
                    payload: TxEventPayload::Deposit { .. },
                    ..
                }
            )
        })
        else {
            panic!("events should include deposit event")
        };
        assert_eq!(*event_tx_hash, tx_hash);
        assert_eq!(*op_id, deposit.op_id());
        assert_eq!(*event_channel_id, deposit.channel_id);
        assert_eq!(*amount, utxo.note.value);
        assert_eq!(*metadata, deposit.metadata);
        assert_eq!(
            notes.as_slice(),
            &[DepositNote {
                note_id: deposited,
                value: utxo.note.value,
                pk: utxo.note.pk,
            }]
        );
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
        let deposited = Utxo::new(deposit.op_id(), 0, utxo.note).id();
        let deposit_ops = vec![Op::ChannelDeposit(deposit)];
        let tx = create_multi_signed_tx(deposit_ops, vec![&Key::Zk(sk)]);
        ledger_state = ledger_state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
            .unwrap()
            .0;

        assert!(
            ledger_state
                .mantle_ledger
                .channels()
                .is_channel_note(&deposited)
        );

        // Withdraw the channel note, releasing it back to a regular note. It
        // keeps the NoteId the deposit gave it.
        let withdraw = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([deposited]),
        };
        let withdraw_tx = Ops::from([Op::ChannelWithdraw(withdraw)]);
        let withdraw_tx_hash = withdraw_tx.hash();
        let withdraw_proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                signing_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
            )]
            .into(),
        )
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.to_vec(),
            vec![&Key::MultiSequencer(withdraw_proof)],
        );

        let result =
            ledger_state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, signed_tx);
        assert!(result.is_ok());

        let (new_state, tx_balance, events, deferred_zkps) = result.unwrap();
        deferred_zkps.verify().unwrap();

        assert_eq!(tx_balance, 0);
        // The note is released from the channel and remains spendable in the ledger.
        assert!(
            !new_state
                .mantle_ledger()
                .channels()
                .is_channel_note(&deposited)
        );
        assert!(new_state.latest_utxos().contains(&deposited));
        assert!(events.is_empty());
    }

    #[test]
    fn test_channel_deposit_is_not_replayable_after_withdraw() {
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

        let deposit = DepositOp {
            channel_id,
            inputs: Inputs::new([utxo.id()]),
            metadata: [5, 6, 7, 8].into(),
        };
        let deposited = Utxo::new(deposit.op_id(), 0, utxo.note).id();
        let deposit_tx =
            create_multi_signed_tx(vec![Op::ChannelDeposit(deposit)], vec![&Key::Zk(sk)]);
        ledger_state = ledger_state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, deposit_tx.clone())
            .unwrap()
            .0;

        // Withdraw releases the channel note under the NoteId the deposit gave
        // it, so the original input never comes back to the ledger.
        let withdraw_tx = Ops::from([Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([deposited]),
        })]);
        let withdraw_proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                signing_key.sign_payload(withdraw_tx.hash().as_signing_bytes().as_ref()),
            )]
            .into(),
        )
        .unwrap();
        let signed_withdraw = create_multi_signed_tx(
            withdraw_tx.to_vec(),
            vec![&Key::MultiSequencer(withdraw_proof)],
        );
        ledger_state = ledger_state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, signed_withdraw)
            .unwrap()
            .0;

        assert!(!ledger_state.latest_utxos().contains(&utxo.id()));

        // Replaying the signed deposit fails: its input no longer exists.
        let result =
            ledger_state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, deposit_tx);
        assert!(result.is_err());
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
        let deposited = Utxo::new(deposit.op_id(), 0, utxo.note).id();
        let deposit_ops = vec![Op::ChannelDeposit(deposit)];
        let tx = create_multi_signed_tx(deposit_ops, vec![&Key::Zk(sk)]);
        ledger_state = ledger_state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
            .unwrap()
            .0;
        // The deposit re-created the note as a channel note
        assert!(
            ledger_state
                .mantle_ledger()
                .channels()
                .is_channel_note(&deposited)
        );

        // Try to withdraw the channel note, but with an invalid proof
        let withdraw = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([deposited]),
        };
        let wrong_key = Ed25519Key::from_bytes(&[42; 32]);
        let withdraw_tx = Ops::from([Op::ChannelWithdraw(withdraw)]);
        let withdraw_tx_hash = withdraw_tx.hash();
        let invalid_proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                wrong_key.sign_payload(withdraw_tx_hash.as_signing_bytes().as_ref()),
            )]
            .into(),
        )
        .unwrap();

        let signed_tx = create_multi_signed_tx(
            withdraw_tx.to_vec(),
            vec![&Key::MultiSequencer(invalid_proof), &Key::EmptyZk],
        );

        let err = ledger_state
            .clone()
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, signed_tx)
            .err()
            .unwrap();
        assert_eq!(
            err,
            LedgerError::VerificationError(VerificationError::ChannelVerificationError(
                mantle::channel::Error::InvalidSignature
            ))
        );

        // The rejected withdraw left the note owned by the channel
        assert!(
            ledger_state
                .mantle_ledger()
                .channels()
                .is_channel_note(&deposited)
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
            inscription: [1, 2, 3].into(),
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let first_tx = create_signed_tx(
            Op::ChannelInscribe(first_inscribe),
            &Key::Ed25519(signing_key.clone()),
        );
        state = state
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
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
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, second_tx);
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
            signer: verifying_key,
        };

        let empty_tx = create_signed_tx(
            Op::ChannelInscribe(empty_inscribe),
            &Key::Ed25519(signing_key),
        );
        let empty_result =
            state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, empty_tx);
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
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, first_tx)
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
        let result = state.try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, second_tx);
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
            parent: MsgId::root(),
            keys: [vk3, vk4].into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };

        let inscribe_op3 = InscriptionOp {
            channel_id: channel1,
            inscription: [7, 8, 9].into(),
            parent: inscribe_op1.id(),
            signer: vk3,
        };

        let ops = vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
            Op::ChannelConfig(config_op.clone()),
            Op::ChannelInscribe(inscribe_op3.clone()),
        ];
        let config_tx = Ops::new_unchecked(ops.clone());
        let config_tx_hash = config_tx.hash();
        let config_proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                sk1.sign_payload(config_tx_hash.as_signing_bytes().as_ref()),
            )]
            .into(),
        )
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
            .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&test_config, tx)
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
        let leader_utxo = utxo();
        let (_sdp_utxo_key, sdp_utxo) = utxo_with_sk();
        let new_utxo_1 = utxo();
        let new_utxo_2 = utxo();
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
            sdp_utxo,
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
        let fees = tx
            .total_gas_cost::<MainnetGasProfile>(&ledger.get_gas_prices())
            .unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(vec![utxo.id()], vec![output_note], &[sk])
            .preverify()
            .unwrap();

        let result = ledger
            .clone()
            .try_apply_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                std::iter::once(tx.clone()),
            );
        // The `unwrap` should succeed because the user pays at least the base fee of
        // 2705
        result.unwrap();

        ledger.cryptarchia_ledger = ledger.cryptarchia_ledger.set_execution_base_fee(10.into());

        let err = ledger
            .try_apply_contents::<_, HeaderId, MainnetGasProfile>(&config, std::iter::once(tx))
            .err()
            .unwrap();
        // The transaction should be rejected because the price indicated for execution
        // doesn't cover the base fee that cost 27 050
        assert_eq!(err, LedgerError::InsufficientBalance);
    }

    #[test]
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
        let fees = tx
            .total_gas_cost::<MainnetGasProfile>(&ledger.get_gas_prices())
            .unwrap();
        output_note.value = utxo.note.value - fees.into_inner();
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
        )
        .preverify()
        .unwrap();

        let result = ledger
            .clone()
            .try_apply_contents::<_, HeaderId, MainnetGasProfile>(&config, std::iter::once(tx));
        // The `unwrap` should succeed because the user pays at least the base fee of
        // 794
        let (no_priority_fee_ledger, events, _) = result.unwrap();
        assert!(events.is_empty());

        // The tx ays 1794 fees = 590 execution base fee + 1000 execution tip + 204
        // storage
        output_note.value = utxo.note.value - fees.into_inner() - 1000;
        let tx = create_tx(
            vec![utxo.id()],
            vec![output_note],
            std::slice::from_ref(&sk),
        )
        .preverify()
        .unwrap();

        let result = ledger
            .try_apply_contents::<_, HeaderId, MainnetGasProfile>(&config, std::iter::once(tx));
        // The `unwrap` should succeed because the user pays at least the base fee of
        // 794
        let (priority_fee_ledger, events, _) = result.unwrap();

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
    fn test_apply_contents_accumulates_storage_gas() {
        let utxo = utxo();
        let config = config();
        let mut ledger = LedgerState::from_utxos([utxo], &config);
        update_ledger_prices(&mut ledger, 1, 1);

        // No outputs: the whole input covers the gas cost and the remainder is
        // tipped. We only assert on the storage counter, not the tip, so the
        // exact balance is irrelevant.
        let sk = ZkKey::from(BigUint::from(0u8));
        let tx = create_tx(vec![utxo.id()], vec![], std::slice::from_ref(&sk))
            .preverify()
            .unwrap();

        // The tx must consume a non-zero amount of storage gas for the check to
        // be meaningful.
        let storage_gas = tx
            .storage_gas_consumption(&ledger.get_gas_prices())
            .unwrap();
        assert!(storage_gas.into_inner() > 0);

        let (applied, _, _) = ledger
            .try_apply_contents::<_, HeaderId, MainnetGasProfile>(&config, std::iter::once(tx))
            .unwrap();

        // Storage gas consumed by the tx should be accumulated in the ledger
        assert_eq!(
            applied.cryptarchia_ledger.storage_gas_consumed_in_epoch(),
            storage_gas
        );
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
                rewards_root: *leaders.vouchers_snapshot_root(),
                voucher_nullifier: nf.into(),
                pk: ZkPublicKey::zero(),
            };
            let signed_operation = SignedOperation::<_, _, StandardMode>::new(
                op,
                Groth16LeaderClaimProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            )
            .into_state_trusted();
            // Skip `op.validate` in this test to avoid having to generate a valid proof
            let (result, _events) = signed_operation
                .execute(LeaderClaimExecutionContext {
                    nullifiers: leaders.nullifiers_cloned(),
                    reward_amount: leaders.reward_amount(),
                    claimable_rewards: leaders.claimable_rewards(),
                    utxos: Utxos::new(),
                    tx_hash: TxHash::from([0u8; 32]),
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
            rewards_root: *leaders.vouchers_snapshot_root(),
            voucher_nullifier: Fr::ZERO.into(), // nf of the 1st voucher
            pk: ZkPublicKey::zero(),
        };
        let signed_operation = SignedOperation::<_, _, StandardMode>::new(
            op,
            Groth16LeaderClaimProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
        );
        let preverified_signed_operation = signed_operation.into_state_trusted::<Preverified>();
        let executable_signed_operation = preverified_signed_operation.clone().into_state_trusted();

        // Skip `op.validate` in this test to avoid having to generate a valid proof
        let (result, _events) = executable_signed_operation
            .execute(LeaderClaimExecutionContext {
                nullifiers: leaders.nullifiers_cloned(),
                reward_amount: leaders.reward_amount(),
                claimable_rewards: leaders.claimable_rewards(),
                utxos: Utxos::new(),
                tx_hash: TxHash::from([0u8; 32]),
            })
            .unwrap();
        leaders.update_nullifiers(result.nullifiers);
        leaders.update_rewards(result.claimable_rewards);
        assert_eq!(result.utxos.size(), 1);
        let (_, (utxo, _)) = result.utxos.utxos().iter().next().unwrap();
        assert_eq!(utxo.note.value, 100);

        // Try to claim the reward using the same nullifier.
        let tx_hash = TxHash::from([0u8; 32]);
        let tx_hash_view = TxHashView::from(tx_hash);
        let err = preverified_signed_operation
            .verify(&LeaderClaimVerificationContext {
                nullifiers: leaders.nullifiers(),
                claimable_vouchers_root: leaders.vouchers_snapshot_root(),
                tx_hash_view: &tx_hash_view,
            })
            .err()
            .unwrap();
        assert_eq!(err, LeaderClaimError::DuplicatedVoucherNullifier);
    }

    mod pow {
        use std::num::NonZeroU64;

        use lb_core::mantle::ops::{
            NoOpProof,
            pow::{ClaimPowRewardError, ClaimPowRewardOp, PowTarget},
        };

        use super::*;
        use crate::config::RewardPoWConfig;

        /// A reward config with claiming disabled (`rate_num = 0`), standing in
        /// for a real deployment config.
        fn disabled_reward_config() -> RewardPoWConfig {
            RewardPoWConfig {
                reward_pool_genesis: 1_000_000_000,
                epoch_reward_genesis: 1_000_000,
                initial_difficulty_seed: 1_000,
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZeroU64::new(10).expect("10 is non-zero"),
                target_claims_per_block: 100,
                rate_num: 0,
                rate_den: NonZeroU64::MIN,
                target_claim_per_block: NonZeroU64::MIN,
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
            let mut state = LedgerState::from_utxos([utxo()], &config);
            state.mantle_ledger.pow.add_reward_refill_rewards(1_000);
            state
                .mantle_ledger
                .pow
                .add_rewards_to_pool(&test_pool_config());
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
            let test_utxo = utxo();
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
            // to satisfy it), and the per-block retarget moves it: an empty
            // block (no claims) eases the target upward.
            let test_utxo = utxo();
            let (mut test_ledger, genesis) = ledger(&[test_utxo], config());
            let genesis_difficulty = difficulty_at(&test_ledger, genesis);
            assert_ne!(genesis_difficulty, Fr::ZERO);

            let block_1 = update_ledger(&mut test_ledger, genesis, 1, test_utxo)
                .expect("empty block should apply");
            assert!(difficulty_at(&test_ledger, block_1) > genesis_difficulty);
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
            // End-to-end through `try_apply_tx`: with `sigma_e` forced to
            // zero the claim fails the §5.6 safety cutoff. This exercises
            // the full wiring: preverification, the stateful
            // `ClaimPowReward` arm and the helper-built context.
            let config = config();
            let mut state = LedgerState::from_utxos([utxo()], &config);
            // The default reward config disables claiming (`rate_num = 0`).
            state.mantle_ledger.pow.add_rewards_to_pool(&config);
            assert_eq!(state.mantle_ledger.pow.epoch_reward(), 0);

            let err = state
                .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
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
                .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
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
            // The full §5.3 pipeline through `try_apply_tx`: preverification,
            // helper-built validation context (pool, window, epoch nonce,
            // difficulty, double-claim) and execution.
            let (state, config) = claim_accepting_state();
            let pool_before = state.mantle_ledger.pow.reward_pool();
            let epoch_reward = state.mantle_ledger.pow.epoch_reward();

            let (state, _balance, events, deferred_zkps) = state
                .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .expect("claim should validate and execute");
            deferred_zkps.verify().unwrap();

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
            let expected_utxo = Utxo {
                op_id: claim_op().op_id(),
                output_index: 0,
                note: Note::new(epoch_reward, claim_op().public_key),
            };
            assert_eq!(
                state
                    .cryptarchia_ledger
                    .latest_utxos()
                    .get(&expected_utxo.id()),
                Some(expected_utxo)
            );
            assert_eq!(events.len(), 1);
            assert!(matches!(
                &events[0].payload,
                TxEventPayload::PoWRewardClaimed { .. }
            ));
        }

        #[test]
        fn claim_tx_double_claim_is_rejected() {
            // Replaying the same solution is caught by the nullifier check
            // during tx-level validation.
            let (state, config) = claim_accepting_state();
            let (state, _, _, deferred_zkps) = state
                .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
                .expect("first claim should succeed");
            deferred_zkps.verify().unwrap();

            let err = state
                .try_apply_tx::<_, HeaderId, MainnetGasProfile>(&config, claim_tx())
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
            // from the pool, records the nullifier, inserts the reward note
            // into the UTXO set and emits the claim event.
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

            let expected_utxo = Utxo {
                op_id: op.op_id(),
                output_index: 0,
                note: Note::new(epoch_reward, op.public_key),
            };
            assert_eq!(
                state
                    .cryptarchia_ledger
                    .latest_utxos()
                    .get(&expected_utxo.id()),
                Some(expected_utxo)
            );

            assert_eq!(events.len(), 1);
            assert!(matches!(
                &events[0],
                TxEvent {
                    tx_hash: event_tx_hash,
                    payload: TxEventPayload::PoWRewardClaimed { .. },
                    ..
                } if *event_tx_hash == tx_hash
            ));
        }

        #[test]
        fn claim_execution_alone_has_no_double_claim_guard() {
            // `try_apply_op` is execute-only by design (like the other op
            // arms): the double-claim check lives in
            // `ClaimPowRewardOp::validate`, which `try_apply_tx` runs via
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
        fn block_fees_do_not_refill_the_pow_pool_while_the_share_is_zero() {
            // Pins that `POW_REWARD_SHARE_NUMERATOR` is still 0: block fees
            // are split between leaders and blend only, so nothing accrues
            // to the PoW refill and the pool is unchanged after crediting.
            let config = config();
            let mut state = LedgerState::from_utxos([utxo()], &config);
            let pool_before = state.mantle_ledger.pow.reward_pool();

            state = state
                .compute_block_rewards(1_000.into(), 0.into())
                .expect("reward computation should succeed");

            state
                .mantle_ledger
                .pow
                .add_rewards_to_pool(&test_pool_config());
            assert_eq!(state.mantle_ledger.pow.reward_pool(), pool_before);
        }
    }
}
