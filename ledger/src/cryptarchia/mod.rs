mod block_density;
mod stake;
#[cfg(test)]
mod test;

use std::sync::{Arc, LazyLock};

use derivative::Derivative;
use lb_core::{
    crypto::{ZkDigest, ZkHasher},
    events::TxEvent,
    mantle::{
        NoteId, Utxo, Value,
        gas::{Gas, GasCost, GasOverflow, GasPrice, GasProfile},
        ledger::verification_mode::{GenesisMode, StandardMode},
        ops::{SignedOperation, pow::PowTarget, transfer::TransferOp},
        transactions::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE, states::Verified},
    },
    proofs::leader_proof::{self, LeaderPublic},
    sdp::Declarations,
};
use lb_cryptarchia_engine::{Epoch, Slot, UncleSlots};
use lb_groth16::{Fr, fr_from_bytes};
use lb_utxotree::MerklePath;

use crate::{
    cryptarchia::{
        block_density::BlockDensity,
        stake::{PRECISION, StakeInference},
    },
    mantle::{
        pow::{PowState, blend_difficulty::compute_epoch_blend_difficulty},
        sdp::SdpLedger,
    },
};

// corresponds to the denominator of q
const EXECUTION_MARKET_EMA_DENOMINATOR: u128 = 10;
// Corresponds to the numerator of q
const EXECUTION_MARKET_EMA_PREV_WEIGHT: u128 = 9;
// Corresponds to 7 * G_target because the numerator is 1 + phi (G_avg -
// G_target)
const EXECUTION_MARKET_BASE_FEE_NUMERATOR: u128 = 11_177_110;
// Corresponds to 8 * G_target because the denominator is 1 + phi (G_avg -
// // G_target)
const EXECUTION_MARKET_BASE_FEE_DENOMINATOR: u128 = 12_773_840;

// Corresponds to the denominator of 1/beta
const STORAGE_MARKET_EMA_DENOMINATOR: u128 = 2;
// Corresponds to the denominator of 1+ alpha and 1-alpha
const STORAGE_MARKET_CLAMP_DENOMINATOR: u128 = 8;
// Corresponds to the numerator of 1-alpha
const STORAGE_MARKET_CLAMP_DOWN_NUMERATOR: u128 = 7;
// Corresponds to the numerator of 1+alpha
const STORAGE_MARKET_CLAMP_UP_NUMERATOR: u128 = 9;

pub type UtxoTree = lb_utxotree::UtxoTree<NoteId, Utxo, ZkHasher>;
use super::{Balance, Config, LedgerError, mantle};
use crate::WINDOW_SIZE;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpochState {
    /// The epoch this snapshot is for
    pub epoch: Epoch,
    /// value of the ledger nonce after `epoch_period_nonce_buffer` slots from
    /// the beginning of the epoch
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub nonce: Fr,
    /// `d_blend`: the Blend `PoW` difficulty for this epoch.
    ///
    /// Set once per epoch from the transaction load of the last epoch that
    /// closed before the snapshot — the baseline divided down by the load, so
    /// a busier chain admits fewer `PoW`-backed Blend messages — and frozen at
    /// the same slot as [`Self::nonce`]. See
    /// [`compute_epoch_blend_difficulty`] for the controller.
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub blend_pow_difficulty: PowTarget,
    /// stake distribution snapshot taken at the beginning of the epoch
    /// (in practice, this is equivalent to the utxos the are spendable at the
    /// beginning of the epoch)
    pub utxos: UtxoTree,
    pub total_stake: Value,
    /// Lottery values computed based on `total_stake`
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub lottery_0: Fr,
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub lottery_1: Fr,
    /// Snapshot of the declarations that are active at the start of
    /// `self.epoch`, frozen at the same slot as the stake distribution
    /// (`stake_distribution_snapshot`).
    ///
    /// Held behind `Arc` because `EpochState` is cloned for every block in
    /// cryptarchia's per-branch state; the underlying map is immutable once
    /// frozen, so all clones can share it.
    ///
    /// TODO: This field reaches "up" into the mantle layer (`SdpLedger`), which
    /// is why the `&SdpLedger` is threaded through `update_from_ledger`,
    /// `update_epoch_state`, `try_apply_header`, and `epoch_state_for_slot`.
    /// That threading is scaffolding: long-term, `EpochState` should be lifted
    /// out of `cryptarchia` to sit alongside `cryptarchia_ledger` and
    /// `mantle_ledger` in the outer `LedgerState`, where the freeze can read
    /// both sub-ledgers directly and these `&SdpLedger` parameters disappear.
    pub active_declarations: Arc<Declarations>,
}

impl EpochState {
    fn update_from_ledger(
        self,
        ledger: &LedgerState,
        sdp: &SdpLedger,
        pow: &PowState,
        config: &Config,
    ) -> Self {
        let nonce_snapshot_slot = config.nonce_snapshot(self.epoch);
        let (nonce, blend_pow_difficulty) = if ledger.slot < nonce_snapshot_slot {
            // The Blend difficulty is snapshotted together with the nonce, and
            // reads the load of the last epoch to have closed — for a snapshot
            // taken during epoch `N - 1`, that is epoch `N - 2`. Both inputs
            // are fixed for the whole time the snapshot is open, so
            // recomputing on every block converges on one value instead of
            // stepping once per block.
            let blend_pow_difficulty = pow.last_closed_epoch_load().map_or(
                // No epoch has closed yet, so the schedule has not started:
                // epochs 0 and 1 sit at the baseline the chain started from.
                ledger.epoch_state.blend_pow_difficulty,
                |load| {
                    compute_epoch_blend_difficulty(
                        load,
                        ledger.epoch_state.blend_pow_difficulty,
                        &config.pow_config.blend,
                    )
                },
            );
            (ledger.nonce, blend_pow_difficulty)
        } else {
            (self.nonce, self.blend_pow_difficulty)
        };

        // The active-declarations snapshot is frozen at the same slot as the
        // stake distribution, so the two halves of the epoch's public info
        // stay consistent.
        let stake_snapshot_slot = config.stake_distribution_snapshot(self.epoch);
        let (utxos, active_declarations) = if ledger.slot < stake_snapshot_slot {
            (
                ledger.utxos.clone(),
                // Filter declarations active at the `self.epoch` from `SdpLedger`
                // regardless of when it was built.
                Arc::new(sdp.active_declarations(self.epoch, &config.sdp_config.service_params)),
            )
        } else {
            (self.utxos, self.active_declarations)
        };
        Self {
            epoch: self.epoch,
            nonce,
            blend_pow_difficulty,
            utxos,
            total_stake: self.total_stake,
            lottery_0: self.lottery_0,
            lottery_1: self.lottery_1,
            active_declarations,
        }
    }

    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    #[must_use]
    pub const fn nonce(&self) -> &Fr {
        &self.nonce
    }

    /// `d_blend`: the Blend `PoW` difficulty in effect for this epoch.
    #[must_use]
    pub const fn blend_pow_difficulty(&self) -> PowTarget {
        self.blend_pow_difficulty
    }

    #[must_use]
    pub const fn total_stake(&self) -> Value {
        self.total_stake
    }

    #[must_use]
    pub fn utxo_merkle_root(&self) -> Fr {
        self.utxos.root()
    }

    /// Computes the Merkle path for the utxo.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the utxo does not exist or has been removed.
    #[must_use]
    pub fn utxo_merkle_path(&self, utxo: &Utxo) -> Option<MerklePath<Fr>> {
        self.utxos.path(&utxo.id())
    }
}

/// Tracks bedrock transactions and minimal the state needed for consensus to
/// work.
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, PartialEq)]
pub struct LedgerState {
    // All available Unspent Transaction Outputs (UTXOs) at the current slot
    // TODO: move UTXOs in the mantle ledger. There is no reason to keep them here
    pub utxos: UtxoTree,
    // randomness contribution
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub nonce: Fr,
    pub slot: Slot,
    // rolling snapshot of the state for the next epoch, used for epoch transitions
    pub next_epoch_state: EpochState,
    pub epoch_state: EpochState,
    #[derivative(PartialEq = "ignore")]
    block_density: BlockDensity,
    // Using an Arc wrapper here as this can be completely shared among instances of LedgerState
    #[derivative(PartialEq = "ignore")]
    stake_inference: Arc<StakeInference>,
    // rolling fee window of 120 blocks, used to derive block rewards
    #[serde(with = "serde_arrays")]
    fee_window: [GasCost; WINDOW_SIZE],
    // Smoothed Average Execution Gas used up to the last block
    average_execution_gas: Gas,
    // Execution Base Fee that are burned and minimum required to pay.
    execution_base_fee: GasPrice,
    // Exponential Moving Average Storage Gas used in the currect epoch
    storage_gas_ema: Gas,
    // Actual storage Gas price of the currect epoch
    storage_gas_price: GasPrice,
    // The amount of Storage Gas consumed in the current epoch
    storage_gas_consumed_in_epoch: Gas,
}

impl LedgerState {
    /// Synthesizes the epoch state for the given slot.
    ///
    /// This function must be called before any other function that updates
    /// [`LedgerState`]. Otherwise, previously accumulated values (e.g. nonce
    /// and block density) will be lost.
    #[expect(
        clippy::too_many_lines,
        reason = "TODO: fix/refactor updating next_epoch_state"
    )]
    fn update_epoch_state<Id>(
        self,
        slot: Slot,
        sdp: &SdpLedger,
        pow: &PowState,
        config: &Config,
    ) -> Result<Self, LedgerError<Id>> {
        if slot <= self.slot {
            return Err(LedgerError::InvalidSlot {
                parent: self.slot,
                block: slot,
            });
        }

        let current_epoch = config.epoch(self.slot);
        let new_epoch = config.epoch(slot);

        // First, update the next epoch nonce using the ledger state
        // that was updated by the previous slot (block).
        // TODO: Refactor: Guarantee that `next_epoch_state` is always updated
        // whenever `LedgerState` is updated before Lottery Constants Finalization
        // period starts.
        let next_epoch_state = self
            .next_epoch_state
            .clone()
            .update_from_ledger(&self, sdp, pow, config);

        // There are 3 cases to consider:
        // 1. We are in the same epoch as the parent state: Update the next epoch state
        // 2. We are in the next epoch: Use the next epoch state as the current epoch
        //    state and reset next epoch state
        // 3. We are in the next-next or later epoch (which mean that some epochs had no
        //    block): Use the parent state as the epoch state and reset next epoch
        //    state. Total stake should be adjusted with zero block density for skipped
        //    epochs. Storage Market is updated with 0 storage gas used for skipped
        //    epochs.
        if current_epoch == new_epoch {
            // case 1)
            Ok(Self {
                slot,
                next_epoch_state,
                ..self
            })
        } else if new_epoch == current_epoch.strict_add(1.into()) {
            // case 2)

            // infer new total stake
            let total_stake = self.stake_inference.total_stake_inference::<PRECISION>(
                self.epoch_state.total_stake,
                self.block_density.current_block_density(),
            );
            let (lottery_0, lottery_1) = config
                .lottery_constants()
                .compute_lottery_values(total_stake);

            tracing::info!(
                old_epoch = ?current_epoch,
                new_epoch = ?new_epoch,
                old_total_stake = self.epoch_state.total_stake,
                new_total_stake = total_stake,
                slot = ?slot,
                "epoch transition"
            );
            let block_density = BlockDensity::new(new_epoch, config);
            // TODO: Refactor: Have the unified update logic for all fields in `EpochState`.
            // `epoch` and `utxos` are updated by `EpochState::update_from_ledger`,
            // but `total_stake` and lottery values are updated here.
            // This can be error-prone.
            let epoch_state = EpochState {
                total_stake,
                lottery_0,
                lottery_1,
                ..next_epoch_state
            };
            let next_epoch_state_epoch = new_epoch.strict_add(1.into());
            let next_epoch_state = EpochState {
                epoch: next_epoch_state_epoch,
                nonce: self.nonce,
                // Carries the difficulty in effect until the retarget for the
                // next epoch is computed and frozen at its nonce snapshot.
                blend_pow_difficulty: epoch_state.blend_pow_difficulty,
                utxos: self.utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
                // Filter declarations active at the `next_epoch_state_epoch`
                // from `SdpLedger` regardless of when it was built.
                active_declarations: Arc::new(sdp.active_declarations(
                    next_epoch_state_epoch,
                    &config.sdp_config.service_params,
                )),
            };
            let (new_price, new_ema) = update_storage_market(
                self.storage_gas_price,
                self.storage_gas_consumed_in_epoch,
                self.storage_gas_ema,
            );
            Ok(Self {
                slot,
                next_epoch_state,
                epoch_state,
                block_density,
                storage_gas_consumed_in_epoch: 0.into(),
                storage_gas_ema: new_ema,
                storage_gas_price: new_price,
                ..self
            })
        } else {
            // case 3)

            // First, infer total stake using block density of the current epoch
            let mut total_stake = self.stake_inference.total_stake_inference::<PRECISION>(
                self.epoch_state.total_stake,
                self.block_density.current_block_density(),
            );
            // Adjust total stake with zero block density for skipped epochs
            for _ in u32::from(next_epoch_state.epoch())..u32::from(new_epoch) {
                total_stake = self
                    .stake_inference
                    .total_stake_inference::<PRECISION>(total_stake, 0);
            }
            let (lottery_0, lottery_1) = config
                .lottery_constants()
                .compute_lottery_values(total_stake);

            // Update Storage Market
            // First, using the current epoch
            let (mut new_price, mut new_ema) = update_storage_market(
                self.storage_gas_price,
                self.storage_gas_consumed_in_epoch,
                self.storage_gas_ema,
            );
            // Then for the empty epochs
            for _ in u32::from(next_epoch_state.epoch())..u32::from(new_epoch) {
                (new_price, new_ema) = update_storage_market(new_price, 0.into(), new_ema);
            }

            tracing::warn!(
                old_epoch = ?current_epoch,
                new_epoch = ?new_epoch,
                epochs_skipped = new_epoch.strict_sub(current_epoch).strict_sub(1.into()).into_inner(),
                old_total_stake = self.epoch_state.total_stake,
                new_total_stake = total_stake,
                slot = ?slot,
                "skipped epochs"
            );
            let block_density = BlockDensity::new(new_epoch, config);
            let epoch_state = EpochState {
                epoch: new_epoch,
                nonce: self.nonce,
                // Too many epochs were skipped for a snapshot to have been
                // taken for `new_epoch`, so the last known difficulty carries
                // over unretargeted, like the rest of the state does here.
                blend_pow_difficulty: self.epoch_state.blend_pow_difficulty,
                utxos: self.utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
                // Filter declarations active at the `new_epoch`
                // from `SdpLedger` regardless of when it was built.
                active_declarations: Arc::new(
                    sdp.active_declarations(new_epoch, &config.sdp_config.service_params),
                ),
            };
            let next_epoch_state_epoch = new_epoch.strict_add(1.into());
            let next_epoch_state = EpochState {
                epoch: next_epoch_state_epoch,
                nonce: self.nonce,
                blend_pow_difficulty: epoch_state.blend_pow_difficulty,
                utxos: self.utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
                // Filter declarations active at the `next_epoch_state_epoch`
                // from `SdpLedger` regardless of when it was built.
                active_declarations: Arc::new(sdp.active_declarations(
                    next_epoch_state_epoch,
                    &config.sdp_config.service_params,
                )),
            };
            Ok(Self {
                slot,
                next_epoch_state,
                epoch_state,
                block_density,
                storage_gas_consumed_in_epoch: 0.into(),
                storage_gas_ema: new_ema,
                storage_gas_price: new_price,
                ..self
            })
        }
    }

    #[must_use]
    pub fn update_execution_market(self, block_execution_gas_consumed: Gas) -> Self {
        // First update the `average_execution_gas`
        let avg_numerator = u128::from(block_execution_gas_consumed.into_inner())
            + EXECUTION_MARKET_EMA_PREV_WEIGHT
                * u128::from(self.average_execution_gas.into_inner());
        let new_average_execution_gas: Gas =
            ((avg_numerator / EXECUTION_MARKET_EMA_DENOMINATOR) as Value).into();

        // Then update the `execution_base_fee` using the new average
        let fee_numerator = u128::from(self.execution_base_fee.into_inner())
            * (EXECUTION_MARKET_BASE_FEE_NUMERATOR
                + u128::from(new_average_execution_gas.into_inner()));
        let new_base_fee =
            (fee_numerator.div_ceil(EXECUTION_MARKET_BASE_FEE_DENOMINATOR) as Value).into();

        Self {
            average_execution_gas: new_average_execution_gas,
            execution_base_fee: new_base_fee,
            ..self
        }
    }

    /// Accumulates the storage gas consumed by an applied block into the
    /// current epoch's counter, which drives the storage price update at the
    /// next epoch rotation.
    pub fn add_storage_gas_consumed(self, storage_gas: Gas) -> Result<Self, GasOverflow> {
        Ok(Self {
            storage_gas_consumed_in_epoch: self
                .storage_gas_consumed_in_epoch
                .checked_add(storage_gas)?,
            ..self
        })
    }

    fn try_apply_proof<LeaderProof, Id>(
        self,
        slot: Slot,
        proof: &LeaderProof,
        config: &Config,
    ) -> Result<Self, LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        assert_eq!(config.epoch(slot), self.epoch_state.epoch);
        let public_inputs = LeaderPublic::new(
            self.aged_utxos().root(),
            self.latest_utxos().root(),
            self.epoch_state.nonce,
            slot.into(),
            self.epoch_state.lottery_0,
            self.epoch_state.lottery_1,
        );
        if !proof.verify(&public_inputs) {
            return Err(LedgerError::InvalidProof);
        }

        Ok(self)
    }

    pub fn try_apply_header<LeaderProof, Id>(
        self,
        slot: Slot,
        proof: &LeaderProof,
        uncle_slots: &UncleSlots,
        sdp: &SdpLedger,
        pow: &PowState,
        config: &Config,
    ) -> Result<Self, LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        // First, synthesize epoch state for `slot` before update the ledger state.
        // Then, apply the proof and update the nonce. Finally, mark the occupied
        // slots since this function is called for a new block.
        Ok(self
            .update_epoch_state_and_apply_proof(slot, proof, sdp, pow, config)?
            .update_nonce(&proof.entropy(), slot)
            .mark_occupied_slots(slot, uncle_slots))
    }

    /// Synthesizes the epoch state for `slot` and apply the proof.
    fn update_epoch_state_and_apply_proof<LeaderProof, Id>(
        self,
        slot: Slot,
        proof: &LeaderProof,
        sdp: &SdpLedger,
        pow: &PowState,
        config: &Config,
    ) -> Result<Self, LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        self.update_epoch_state(slot, sdp, pow, config)?
            .try_apply_proof(slot, proof, config)
    }

    /// Verifies a leadership proof for a block at `slot` whose parent is the
    /// block this state belongs to, leaving the state untouched.
    pub fn verify_proof_of_leadership<LeaderProof, Id>(
        &self,
        slot: Slot,
        proof: &LeaderProof,
        sdp: &SdpLedger,
        pow: &PowState,
        config: &Config,
    ) -> Result<(), LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        self.clone()
            .update_epoch_state_and_apply_proof(slot, proof, sdp, pow, config)?;
        Ok(())
    }

    pub fn try_apply_transfer<Id, Profile: GasProfile>(
        mut self,
        signed_operation: SignedOperation<TransferOp, Verified, StandardMode>,
    ) -> Result<(Self, Balance, Vec<TxEvent>), LedgerError<Id>> {
        let operation = signed_operation.operation();
        // Compute the balance
        let balance = operation
            .balance(&self.utxos)
            .map_err(mantle::Error::Transfer)?;

        //execute the transfer
        let (result, events) = signed_operation
            .execute(self.utxos)
            .map_err(|(_signed_operation, error)| mantle::Error::Transfer(error))?;
        self.utxos = result;
        Ok((self, balance, events))
    }

    fn update_nonce(self, contrib: &Fr, slot: Slot) -> Self {
        // constants and structure as defined in the Mantle spec:
        // https://lip.logos.co/blockchain/raw/cryptarchia-v1-protocol.html
        static EPOCH_NONCE_V1: LazyLock<Fr> =
            LazyLock::new(|| fr_from_bytes(b"EPOCH_NONCE_V1").unwrap());
        let mut hasher = ZkHasher::new();
        <ZkHasher as ZkDigest>::update(&mut hasher, &EPOCH_NONCE_V1);
        <ZkHasher as ZkDigest>::update(&mut hasher, &self.nonce);
        <ZkHasher as ZkDigest>::update(&mut hasher, contrib);
        <ZkHasher as ZkDigest>::update(&mut hasher, &Fr::from(u64::from(slot)));

        let nonce: Fr = hasher.finalize();
        Self { nonce, ..self }
    }

    fn mark_occupied_slots(self, slot: Slot, uncle_slots: &UncleSlots) -> Self {
        let mut block_density = self.block_density.clone();
        block_density.mark_occupied_slots(slot, uncle_slots);
        Self {
            block_density,
            ..self
        }
    }

    pub const fn update_fee_window(&mut self, index: usize, total_fee: GasCost) {
        self.fee_window[index] = total_fee;
    }

    #[must_use]
    pub const fn get_fee_from_index(&self, index: usize) -> GasCost {
        self.fee_window[index]
    }

    #[must_use]
    pub fn get_summed_fees(&self) -> u128 {
        self.fee_window
            .iter()
            .map(|x| u128::from(x.into_inner()))
            .sum()
    }

    #[must_use]
    pub const fn slot(&self) -> Slot {
        self.slot
    }

    #[must_use]
    pub const fn epoch_state(&self) -> &EpochState {
        &self.epoch_state
    }

    #[must_use]
    pub const fn next_epoch_state(&self) -> &EpochState {
        &self.next_epoch_state
    }

    /// Seeds the genesis epoch-state snapshots with the genesis SDP ledger.
    ///
    /// At genesis the cryptarchia ledger is built before the mantle `SdpLedger`
    /// exists (the mantle ledger is derived from the cryptarchia epoch state),
    /// so the initial epoch states start with an empty active-declarations
    /// snapshot. Once the genesis `SdpLedger` is available, this seeds the
    /// active-declarations snapshot for epochs 0 and 1.
    #[must_use]
    pub fn with_genesis_sdp(mut self, sdp: &SdpLedger, config: &Config) -> Self {
        let service_params = &config.sdp_config.service_params;
        self.epoch_state.active_declarations =
            Arc::new(sdp.active_declarations(self.epoch_state.epoch, service_params));
        self.next_epoch_state.active_declarations =
            Arc::new(sdp.active_declarations(self.next_epoch_state.epoch, service_params));
        self
    }

    #[must_use]
    pub const fn latest_utxos(&self) -> &UtxoTree {
        &self.utxos
    }

    #[must_use]
    pub fn update_utxos(self, utxos: UtxoTree) -> Self {
        Self { utxos, ..self }
    }

    #[must_use]
    pub const fn execution_base_fee(&self) -> &GasPrice {
        &self.execution_base_fee
    }

    #[must_use]
    pub const fn storage_gas_price(&self) -> &GasPrice {
        &self.storage_gas_price
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn storage_gas_consumed_in_epoch(&self) -> Gas {
        self.storage_gas_consumed_in_epoch
    }

    #[must_use]
    pub const fn aged_utxos(&self) -> &UtxoTree {
        &self.epoch_state.utxos
    }

    /// Synthesizes the epoch state for a given slot.
    ///
    /// This handles the case where epochs have been skipped (no blocks
    /// produced). Details can be found in [`Self::update_epoch_state`].
    ///
    /// Returns [`LedgerError::InvalidSlot`] if the slot is in the past before
    /// the current ledger state.
    pub fn epoch_state_for_slot<Id>(
        &self,
        slot: Slot,
        sdp: &SdpLedger,
        pow: &PowState,
        config: &Config,
    ) -> Result<EpochState, LedgerError<Id>> {
        Ok(self
            .clone()
            .update_epoch_state(slot, sdp, pow, config)?
            .epoch_state()
            .clone())
    }

    pub fn from_genesis_tx<Id>(
        transfer: &SignedOperation<TransferOp, Verified, GenesisMode>,
        config: &Config,
        epoch_nonce: Fr,
    ) -> Result<Self, LedgerError<Id>> {
        let operation = transfer.operation();

        // This transfer has not yet been verified despite the state saying so.
        // This is its verification.
        if !operation.inputs.is_empty() {
            let first_input = operation
                .inputs
                .iter()
                .next()
                .copied()
                .expect("is not empty");
            return Err(LedgerError::InputInGenesis(first_input));
        }

        Ok(Self::from_utxos(operation.utxos(), config, epoch_nonce))
    }

    pub fn from_utxos(utxos: impl IntoIterator<Item = Utxo>, config: &Config, nonce: Fr) -> Self {
        let utxos = utxos
            .into_iter()
            .map(|utxo| (utxo.id(), utxo))
            .collect::<UtxoTree>();
        let total_stake = utxos
            .utxos()
            .iter()
            .filter(|(_, (utxo, _))| config.faucet_pk.is_none_or(|fpk| utxo.note.pk != fpk))
            .map(|(_, (utxo, _))| utxo.note.value)
            .sum::<Value>()
            .max(1); // TODO: Change total_stake to NonZeroU64: https://github.com/logos-blockchain/logos-blockchain/issues/2166
        let (lottery_0, lottery_1) = config
            .lottery_constants()
            .compute_lottery_values(total_stake);
        let slot: Slot = 0.into();
        let stake_inference = Arc::new(StakeInference::new(
            config.consensus_config.stake_inference_learning_rate(),
            config.consensus_config.slot_activation_coeff().as_f64(),
            config.total_stake_inference_period(),
        ));
        let block_density = BlockDensity::new(config.epoch(slot), config);
        Self {
            utxos: utxos.clone(),
            nonce,
            slot,
            next_epoch_state: EpochState {
                epoch: 1.into(),
                nonce,
                blend_pow_difficulty: config.pow_config.blend.base_difficulty.into(),
                utxos: utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
                active_declarations: Arc::new(Declarations::default()),
            },
            epoch_state: EpochState {
                epoch: 0.into(),
                nonce,
                blend_pow_difficulty: config.pow_config.blend.base_difficulty.into(),
                utxos,
                total_stake,
                lottery_0,
                lottery_1,
                active_declarations: Arc::new(Declarations::default()),
            },
            block_density,
            stake_inference,
            fee_window: [0.into(); 120],
            average_execution_gas: 0.into(),
            execution_base_fee: GENESIS_EXECUTION_GAS_PRICE,
            storage_gas_ema: 0.into(),
            storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
            storage_gas_consumed_in_epoch: 0.into(),
        }
    }
}

// This function upgrade the storage Gas price when a new epoch starts assuming
// the structure contains how much storage gas was consumed in the previous
// epoch according to <https://lip.logos.co/blockchain/raw/storage-markets.html>
fn update_storage_market(
    storage_gas_price: GasPrice,
    storage_gas_consumed_in_epoch: Gas,
    storage_gas_ema: Gas,
) -> (GasPrice, Gas) {
    let previous_price = u128::from(storage_gas_price.into_inner());
    let total_storage_gas = u128::from(storage_gas_consumed_in_epoch.into_inner());
    let previous_ema = u128::from(storage_gas_ema.into_inner());

    let new_ema: Gas =
        (((total_storage_gas + previous_ema) / STORAGE_MARKET_EMA_DENOMINATOR) as Value).into();
    let new_ema_unsigned = u128::from(new_ema.into_inner());
    // Hold the price while the effective target is zero (genesis / sustained
    // zero usage). Without this guard `comparator <= 7*ema` is `0 <= 0` (true),
    // wrongly taking the clamp-down branch and ratcheting the price down 12.5%
    // every zero-usage epoch instead of holding it at P_STR(0).
    if new_ema_unsigned == 0 {
        return (storage_gas_price, new_ema);
    }
    let comparator = STORAGE_MARKET_CLAMP_DENOMINATOR * total_storage_gas;
    let new_price = if comparator <= STORAGE_MARKET_CLAMP_DOWN_NUMERATOR * new_ema_unsigned {
        ((previous_price * STORAGE_MARKET_CLAMP_DOWN_NUMERATOR)
            .div_ceil(STORAGE_MARKET_CLAMP_DENOMINATOR) as Value)
            .into()
    } else if comparator >= STORAGE_MARKET_CLAMP_UP_NUMERATOR * new_ema_unsigned {
        ((previous_price * STORAGE_MARKET_CLAMP_UP_NUMERATOR)
            .div_ceil(STORAGE_MARKET_CLAMP_DENOMINATOR) as Value)
            .into()
    } else {
        ((previous_price * total_storage_gas).div_ceil(new_ema_unsigned) as Value).into()
    };

    (new_price, new_ema)
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "No epoch info in debug output."
)]
impl core::fmt::Debug for LedgerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LedgerState")
            .field("utxos root", &self.utxos.root())
            .field("nonce", &self.nonce)
            .field("slot", &self.slot)
            .finish()
    }
}

#[cfg(test)]
pub mod tests {
    use std::num::{NonZero, NonZeroU32, NonZeroU64};

    use lb_core::{
        crypto::{Digest as _, Hasher},
        mantle::{
            Note, Op,
            OpProof::ZkSig,
            SignedOps, TxGasCalculator as _,
            gas::MainnetGasProfile,
            ledger::{Inputs, Outputs},
            ops::{ZkAndEd25519Proof, leader_claim::VoucherCm, sdp::SDPDeclareOp},
            traits::Hashable as _,
            transactions::{
                GasPrices, OpProofs, Ops,
                states::{Preverified, Unverified},
            },
        },
        sdp::{Declaration, DeclarationId, Locator, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof, ModulusShift};
    use lb_key_management_system_keys::keys::{
        Ed25519Key, Ed25519PublicKey, Ed25519Signature, ZkKey, ZkSignature,
    };
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};

    use super::*;
    use crate::{
        Ledger,
        leader_proof::LeaderProof,
        mantle::{
            pow::tx_density::ClosedEpochLoad,
            sdp::{
                ServiceRewardsParameters,
                rewards::{self},
            },
        },
    };

    type HeaderId = [u8; 32];

    #[must_use]
    pub fn utxo() -> Utxo {
        utxo_with_sk().1
    }

    #[must_use]
    pub fn utxo_with_sk() -> (ZkKey, Utxo) {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(BigUint::from(0u64));
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note: Note::new(10000, zk_sk.to_public_key()),
        };

        (zk_sk, utxo)
    }

    pub struct DummyProof {
        pub public: LeaderPublic,
        pub leader_key: Ed25519PublicKey,
        pub voucher_cm: VoucherCm,
    }

    impl LeaderProof for DummyProof {
        fn verify(&self, public_inputs: &LeaderPublic) -> bool {
            &self.public == public_inputs
        }

        fn verify_genesis(&self) -> bool {
            true
        }

        fn entropy(&self) -> Fr {
            // For dummy proof, return zero entropy
            Fr::from(0u8)
        }

        fn leader_key(&self) -> &Ed25519PublicKey {
            &self.leader_key
        }

        fn voucher_cm(&self) -> &VoucherCm {
            &self.voucher_cm
        }
    }

    impl LedgerState {
        #[cfg(test)]
        #[must_use]
        pub fn set_execution_base_fee(self, new_execution_fee: GasPrice) -> Self {
            Self {
                execution_base_fee: new_execution_fee,
                ..self
            }
        }

        #[cfg(test)]
        #[must_use]
        pub fn set_storage_price(self, new_storage_price: GasPrice) -> Self {
            Self {
                storage_gas_price: new_storage_price,
                ..self
            }
        }
    }

    /// Advance both halves of the state the way applying a block does: the
    /// epoch state is synthesised first, reading the `PoW` state as it stands,
    /// then the `PoW` state closes the epoch just left and counts the block's
    /// transactions.
    fn apply_block(
        state: LedgerState,
        pow: &mut PowState,
        slot: u64,
        txs_in_block: u64,
        sdp: &SdpLedger,
        config: &Config,
    ) -> LedgerState {
        let previous_epoch_state = state.epoch_state().clone();
        let state = state
            .update_epoch_state::<HeaderId>(slot.into(), sdp, pow, config)
            .unwrap();
        *pow = pow.try_apply_header(&previous_epoch_state, state.epoch_state());
        pow.record_block_txs(txs_in_block);
        state
    }

    pub fn update_ledger(
        ledger: &mut Ledger<HeaderId>,
        parent: HeaderId,
        slot: impl Into<Slot>,
        utxo: Utxo,
    ) -> Result<HeaderId, LedgerError<HeaderId>> {
        let slot = slot.into();
        let ledger_state = ledger
            .state(&parent)
            .unwrap()
            .clone()
            .cryptarchia_ledger
            .update_epoch_state::<HeaderId>(
                slot,
                &SdpLedger::new(0.into()),
                &ledger.state(&parent).unwrap().mantle_ledger.pow,
                ledger.config(),
            )?;
        let id = make_id(parent, slot, utxo);
        let proof = generate_proof(&ledger_state, &utxo, slot);
        let (_, state, _) = ledger.prepare_update::<_, _, MainnetGasProfile>(
            id,
            parent,
            slot,
            &proof,
            &UncleSlots::default(),
            std::iter::empty::<SignedOps<Preverified, StandardMode>>(),
        )?;
        ledger.commit_update(id, state);
        Ok(id)
    }

    fn make_id(parent: HeaderId, slot: impl Into<Slot>, utxo: Utxo) -> HeaderId {
        Hasher::new()
            .chain_update(parent)
            .chain_update(slot.into().to_le_bytes())
            .chain_update(utxo.id().as_bytes())
            .finalize()
            .into()
    }

    // produce a proof for a note
    #[must_use]
    pub fn generate_proof(ledger_state: &LedgerState, utxo: &Utxo, slot: Slot) -> DummyProof {
        let latest_tree = ledger_state.latest_utxos();
        let aged_tree = ledger_state.aged_utxos();
        DummyProof {
            public: LeaderPublic::new(
                if aged_tree.contains(&utxo.id()) {
                    aged_tree.root()
                } else {
                    println!("Note not found in aged utxos, using zero root");
                    Fr::from(0u8)
                },
                if latest_tree.contains(&utxo.id()) {
                    latest_tree.root()
                } else {
                    println!("Note not found in latest utxos, using zero root");
                    Fr::from(0u8)
                },
                ledger_state.epoch_state.nonce,
                slot.into(),
                ledger_state.epoch_state.lottery_0,
                ledger_state.epoch_state.lottery_1,
            ),
            leader_key: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            voucher_cm: VoucherCm::default(),
        }
    }

    #[must_use]
    pub fn config() -> Config {
        let mut service_params = std::collections::HashMap::new();
        service_params.insert(
            ServiceType::BlendNetwork,
            ServiceParameters {
                inactivity_period: 2.try_into().unwrap(),
                epoch: 0.into(),
            },
        );

        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(1).unwrap(),
            NonNegativeRatio::new(1, 10.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
            NonZero::new(12).unwrap(),
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        Config {
            epoch_config,
            consensus_config,
            sdp_config: mantle::sdp::Config {
                service_params: Arc::new(service_params),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: lb_core::sdp::MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
            // `alpha = 1/2`, `T_tx = 10`, `k = 4`, with a baseline that is a
            // perfect square so the reference load lands on it exactly.
            pow_config: crate::config::PoWConfig {
                blend: crate::config::BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<234>(),
                    target_transactions_per_block: NonZeroU64::new(10).unwrap(),
                    max_step: NonZeroU64::new(4).unwrap(),
                    damping_num: NonZeroU32::new(1).unwrap(),
                    damping_den_offset: 1,
                },
            },
        }
    }

    #[must_use]
    pub fn genesis_state(utxos: &[Utxo]) -> LedgerState {
        let config = config();
        let total_stake = utxos.iter().map(|u| u.note.value).sum();
        let (lottery_0, lottery_1) = config
            .lottery_constants()
            .compute_lottery_values(total_stake);
        let utxos = utxos
            .iter()
            .map(|utxo| (utxo.id(), *utxo))
            .collect::<UtxoTree>();
        let slot = 0.into();
        let stake_inference = Arc::new(StakeInference::new(
            config.consensus_config.stake_inference_learning_rate(),
            config.consensus_config.slot_activation_coeff().as_f64(),
            config.total_stake_inference_period(),
        ));
        let block_density = BlockDensity::new(config.epoch(slot), &config);

        let epoch_state = EpochState {
            epoch: 0.into(),
            nonce: Fr::ZERO,
            blend_pow_difficulty: config.pow_config.blend.base_difficulty.into(),
            utxos: utxos.clone(),
            total_stake,
            lottery_0,
            lottery_1,
            active_declarations: Arc::new(Declarations::default()),
        };
        let next_epoch_state = EpochState {
            epoch: 1.into(),
            nonce: Fr::ZERO,
            blend_pow_difficulty: config.pow_config.blend.base_difficulty.into(),
            utxos: utxos.clone(),
            total_stake,
            lottery_0,
            lottery_1,
            active_declarations: Arc::new(Declarations::default()),
        };

        LedgerState {
            utxos,
            nonce: Fr::ZERO,
            slot,
            next_epoch_state,
            epoch_state,
            stake_inference,
            fee_window: [0.into(); 120],
            average_execution_gas: 0.into(),
            block_density,
            execution_base_fee: GENESIS_EXECUTION_GAS_PRICE,
            storage_gas_ema: 0.into(),
            storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
            storage_gas_consumed_in_epoch: 0.into(),
        }
    }

    fn full_ledger_state(cryptarchia_ledger: LedgerState, config: &Config) -> crate::LedgerState {
        let mantle_ledger = mantle::LedgerState::new(config, cryptarchia_ledger.epoch_state());
        crate::LedgerState {
            block_number: 0,
            cryptarchia_ledger,
            mantle_ledger,
        }
    }

    #[must_use]
    pub fn ledger(utxos: &[Utxo], config: Config) -> (Ledger<HeaderId>, HeaderId) {
        let genesis_state = genesis_state(utxos);
        (
            Ledger::new([0; 32], full_ledger_state(genesis_state, &config), config),
            [0; 32],
        )
    }

    pub fn apply_and_add_utxo(
        ledger: &mut Ledger<HeaderId>,
        parent: HeaderId,
        slot: impl Into<Slot>,
        utxo_proof: Utxo,
        utxo_add: Utxo,
    ) -> HeaderId {
        let id = update_ledger(ledger, parent, slot, utxo_proof).unwrap();
        // we still don't have transactions, so the only way to add a commitment to
        // spendable utxos and test epoch snapshotting is by doing this
        // manually
        let block_ledger = ledger.states.get_mut(&id).unwrap();
        let new_utxos = block_ledger
            .cryptarchia_ledger
            .utxos
            .insert(utxo_add.id(), utxo_add)
            .0;
        block_ledger.cryptarchia_ledger.utxos = new_utxos;
        id
    }

    pub fn apply_and_add_utxo_and_declaration(
        ledger: &mut Ledger<HeaderId>,
        parent: HeaderId,
        slot: impl Into<Slot>,
        utxo_proof: Utxo,
        utxo_add: Utxo,
        sdp_utxo: Utxo,
    ) -> (HeaderId, SDPDeclareOp, ZkKey) {
        let id = apply_and_add_utxo(ledger, parent, slot, utxo_proof, utxo_add);

        let mut zk_key = [0u8; 16];
        thread_rng().fill_bytes(&mut zk_key);
        let zk_key: ZkKey = fr_from_bytes(&zk_key).unwrap().into();
        let mut signing_key_bytes = [0u8; 32];
        thread_rng().fill_bytes(&mut signing_key_bytes);
        let signing_key = Ed25519Key::from_bytes(&signing_key_bytes);
        let declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
            provider_id: signing_key.public_key().into(),
            zk_id: zk_key.to_public_key(),
            locked_note_id: sdp_utxo.id(),
        };

        let proof = ZkAndEd25519Proof {
            zk_sig: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation =
            SignedOperation::<_, _, StandardMode>::new(declare_op.clone(), proof)
                .into_state_trusted();
        let config = ledger.config().clone();

        let block_ledger = ledger.states.get_mut(&id).unwrap();
        block_ledger.mantle_ledger = block_ledger
            .mantle_ledger
            .clone()
            .try_apply_sdp_declaration(
                signed_operation,
                block_ledger.cryptarchia_ledger.latest_utxos(),
                &config,
            )
            .unwrap()
            .0;

        (id, declare_op, zk_key)
    }

    fn assert_sdp_snapshot(
        ledger: &Ledger<HeaderId>,
        header_id: &HeaderId,
        snapshot_header_id: &HeaderId,
    ) {
        let epoch = ledger.states[header_id]
            .cryptarchia_ledger
            .epoch_state
            .epoch;
        let expected = ledger.states[snapshot_header_id]
            .mantle_ledger
            .sdp
            .active_declarations(epoch, &config().sdp_config.service_params);
        assert_eq!(
            *ledger.states[header_id]
                .cryptarchia_ledger
                .epoch_state
                .active_declarations,
            expected,
        );
    }

    fn assert_declaration_exists(
        ledger: &Ledger<HeaderId>,
        header_id: &HeaderId,
        declaration_id: &DeclarationId,
    ) {
        assert!(
            ledger.states[header_id]
                .mantle_ledger
                .sdp
                .get_declaration(declaration_id)
                .is_some()
        );
    }

    #[must_use]
    pub fn declaration_in_snapshot<'l>(
        ledger: &'l Ledger<HeaderId>,
        header_id: &HeaderId,
        declaration_id: &DeclarationId,
    ) -> Option<&'l Declaration> {
        ledger.states[header_id]
            .cryptarchia_ledger
            .epoch_state
            .active_declarations
            .for_service(&ServiceType::BlendNetwork)
            .and_then(|m| m.get(declaration_id))
    }

    #[test]
    fn storage_price_held_when_effective_target_is_zero() {
        // Failure case for the missing guard: with zero usage and a zero EMA the
        // effective target is 0, so the price must be HELD at P_STR(0). Without
        // the `effective_target == 0` guard, `8*0 <= 7*0` takes the clamp-down
        // branch and ratchets the price down 12.5% (1000 -> 875) every
        // zero-usage epoch.
        let price: GasPrice = 1000.into();
        let (new_price, new_ema) = update_storage_market(price, 0.into(), 0.into());
        assert_eq!(
            new_price, price,
            "price must hold while the effective target (EMA) is zero"
        );
        assert_eq!(new_ema, Gas::from(0));

        // Sanity: once there is a real (non-zero) EMA, the price still adjusts —
        // the guard only fires at a zero target. Zero usage against a non-zero
        // EMA clamps down as before.
        let (clamped, _) = update_storage_market(price, 0.into(), 800.into());
        assert_eq!(clamped, GasPrice::from(875), "non-zero target still clamps");
    }

    #[test]
    fn test_ledger_state_allow_leadership_utxo_reuse() {
        let utxo = utxo();
        let (mut ledger, genesis) = ledger(&[utxo], config());

        let h = update_ledger(&mut ledger, genesis, 1, utxo).unwrap();

        // reusing the same utxo for leadersip should be allowed
        update_ledger(&mut ledger, h, 2, utxo).unwrap();
    }

    #[test]
    fn test_ledger_state_uncommited_utxo() {
        let utxo_1 = utxo();
        let (mut ledger, genesis) = ledger(&[utxo()], config());
        assert!(matches!(
            update_ledger(&mut ledger, genesis, 1, utxo_1),
            Err(LedgerError::InvalidProof),
        ));
    }

    #[test]
    fn test_epoch_transition() {
        let leader_utxos = std::iter::repeat_with(utxo).take(4).collect::<Vec<_>>();
        let (_sdp_utxo_key_1, sdp_utxo_1) = utxo_with_sk();
        let (_sdp_utxo_key_2, sdp_utxo_2) = utxo_with_sk();
        let genesis_utxos = leader_utxos
            .iter()
            .copied()
            .chain(std::iter::once(sdp_utxo_1))
            .chain(std::iter::once(sdp_utxo_2))
            .collect::<Vec<_>>();
        let new_utxo_1 = utxo();
        let new_utxo_2 = utxo();

        let config = config();
        assert_eq!(config.epoch_length(), 100);
        let (mut ledger, genesis) = ledger(&genesis_utxos, config);
        // block density slot range should be [0, 59]
        assert_eq!(
            ledger.states[&genesis]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(0.into()..=59.into())
        );

        let h_1 = update_ledger(&mut ledger, genesis, 10, leader_utxos[0]).unwrap();
        assert_eq!(ledger.states[&h_1].cryptarchia_ledger.epoch_state.epoch, 0);

        let h_2 = update_ledger(&mut ledger, h_1, 60, leader_utxos[1]).unwrap();

        let (h_3, declare_1, _) = apply_and_add_utxo_and_declaration(
            &mut ledger,
            h_2,
            90,
            leader_utxos[2],
            new_utxo_1,
            sdp_utxo_1,
        );
        assert_declaration_exists(&ledger, &h_3, &declare_1.id());

        // Epoch jump: epoch 0 -> 2
        // Jump to the slot that is not the 1st slot of epoch 2
        let h_4 = update_ledger(&mut ledger, h_3, 222, leader_utxos[3]).unwrap();
        // nonce for epoch 2 should be taken at the end of slot 160, but in our case
        // the last block is at slot 90 because of epoch jumps
        assert_eq!(
            ledger.states[&h_4].cryptarchia_ledger.epoch_state.nonce,
            ledger.states[&h_3].cryptarchia_ledger.nonce,
        );
        // stake distribution snapshot should be taken at the end of slot 90
        assert_eq!(
            ledger.states[&h_4].cryptarchia_ledger.epoch_state.utxos,
            ledger.states[&h_3].cryptarchia_ledger.utxos,
        );
        // SDP snapshot should be taken at the end of slot 90
        assert_sdp_snapshot(&ledger, &h_4, &h_3);
        assert!(declaration_in_snapshot(&ledger, &h_4, &declare_1.id()).is_some());
        // block density slot range should be [200, 259]
        assert_eq!(
            ledger.states[&h_4]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(200.into()..=259.into())
        );

        // Epoch transition: 0 -> 1
        let (h_5, declare_2, _) = apply_and_add_utxo_and_declaration(
            &mut ledger,
            h_3,
            100,
            leader_utxos[3],
            new_utxo_2,
            sdp_utxo_2,
        );
        assert_declaration_exists(&ledger, &h_5, &declare_2.id());
        // nonce for epoch 1 should be taken at the end of slot 10,
        // ignoring updates (`h_2` and `h_3`) after slot 59.
        assert_eq!(
            ledger.states[&h_5].cryptarchia_ledger.epoch_state.nonce,
            ledger.states[&h_1].cryptarchia_ledger.nonce,
        );
        // stake distribution snapshot should be the same as the one in genesis
        assert_eq!(
            ledger.states[&h_5].cryptarchia_ledger.epoch_state.utxos,
            ledger.states[&genesis].cryptarchia_ledger.utxos,
        );
        // SDP snapshot should be the same as the one in genesis
        assert_sdp_snapshot(&ledger, &h_5, &genesis);
        assert!(declaration_in_snapshot(&ledger, &h_5, &declare_1.id()).is_none());
        assert!(declaration_in_snapshot(&ledger, &h_5, &declare_2.id()).is_none());
        // block density slot range should be [100, 159]
        assert_eq!(
            ledger.states[&h_5]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(100.into()..=159.into())
        );

        // Epoch transition: 1 -> 2
        let h_6 = update_ledger(&mut ledger, h_5, 200, leader_utxos[3]).unwrap();
        // nonce should be taken at the end of slot 100,
        // which was the only one update in the previous epoch.
        assert_eq!(
            ledger.states[&h_6].cryptarchia_ledger.epoch_state.nonce,
            ledger.states[&h_5].cryptarchia_ledger.nonce,
        );
        // stake distribution snapshot should be taken before the slot 100
        assert_eq!(
            ledger.states[&h_6].cryptarchia_ledger.epoch_state.utxos,
            ledger.states[&h_3].cryptarchia_ledger.utxos,
        );
        // SDP snapshot should be taken before the slot 100
        assert_sdp_snapshot(&ledger, &h_6, &h_3);
        assert!(declaration_in_snapshot(&ledger, &h_6, &declare_1.id()).is_some());
        assert!(declaration_in_snapshot(&ledger, &h_6, &declare_2.id()).is_none());
        // block density slot range should be [200, 259]
        assert_eq!(
            ledger.states[&h_6]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(200.into()..=259.into())
        );
    }

    #[test]
    fn blend_difficulty_retargets_from_the_closed_epoch_average() {
        let config = config();
        assert_eq!(config.epoch_length(), 100);
        // The Blend difficulty is snapshotted with the nonce: at slot 60 for
        // epoch 1, at slot 160 for epoch 2.
        assert_eq!(config.nonce_snapshot(1.into()), 60.into());
        assert_eq!(config.nonce_snapshot(2.into()), 160.into());

        let sdp = SdpLedger::new(0.into());
        let mut state = genesis_state(&[utxo()]);
        let mut pow = PowState::new();
        let blend_config = &config.pow_config.blend;
        let genesis_difficulty = state.epoch_state.blend_pow_difficulty;
        assert_eq!(
            genesis_difficulty,
            PowTarget::from(blend_config.base_difficulty)
        );

        // Epoch 0: three blocks carrying 12 transactions in total.
        for (slot, txs) in [(10u64, 3u64), (20, 5), (70, 4)] {
            state = apply_block(state, &mut pow, slot, txs, &sdp, &config);
        }

        // Epoch 0 -> 1. No epoch had closed while epoch 1's difficulty was open
        // for snapshotting, so the schedule has not started yet and epoch 1
        // sits at the baseline, as the spec requires for epochs 0 and 1.
        state = apply_block(state, &mut pow, 100, 2, &sdp, &config);
        assert_eq!(state.epoch_state.epoch, 1);
        let epoch_1_difficulty = state.epoch_state.blend_pow_difficulty;
        assert_eq!(epoch_1_difficulty, genesis_difficulty);

        // Epoch 1, with blocks on both sides of epoch 2's snapshot slot.
        for (slot, txs) in [(110u64, 100u64), (170, 100)] {
            state = apply_block(state, &mut pow, slot, txs, &sdp, &config);
        }

        // Epoch 1 -> 2: the schedule starts here. Epoch 2's difficulty comes
        // from the load of the epoch that closed before its snapshot opened —
        // epoch 0's 12 transactions over 3 blocks, four tenths of the
        // reference load, so the threshold eases towards BASE / sqrt(0.4) and
        // is held to the factor-2 clamp. Epoch 1's own, much busier, blocks do
        // not enter it: its totals only close when it does, a full epoch after
        // the snapshot was taken.
        state = apply_block(state, &mut pow, 200, 0, &sdp, &config);
        assert_eq!(state.epoch_state.epoch, 2);
        assert_eq!(
            state.epoch_state.blend_pow_difficulty,
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(12, 3),
                epoch_1_difficulty,
                blend_config
            )
        );
        // Below the reference load, so admission loosens — up to the clamp.
        assert!(state.epoch_state.blend_pow_difficulty > epoch_1_difficulty);
    }

    #[test]
    fn blend_difficulty_counts_only_the_branch_it_is_read_from() {
        // The block and transaction counters live in the per-block
        // `LedgerState`, which `Ledger::prepare_update` derives from a clone of
        // the parent's. Competing branches therefore accumulate independently,
        // and reading the difficulty at a tip counts exactly the blocks on that
        // tip's ancestry — no unwinding is needed when a re-org picks a
        // different tip.
        let config = config();
        let blend_config = &config.pow_config.blend;
        let sdp = SdpLedger::new(0.into());

        // A branch is both halves of the state together, since a block clones
        // and advances them as a pair.
        let mut ancestor = (genesis_state(&[utxo()]), PowState::new());

        // A common ancestor carrying 4 transactions, in epoch 0.
        ancestor.0 = apply_block(ancestor.0, &mut ancestor.1, 10, 4, &sdp, &config);

        // Two competing blocks for the same slot, each built on its own clone
        // of the ancestor's state.
        let mut busy_branch = ancestor.clone();
        busy_branch.0 = apply_block(busy_branch.0, &mut busy_branch.1, 20, 96, &sdp, &config);

        let mut quiet_branch = ancestor;
        quiet_branch.0 = apply_block(quiet_branch.0, &mut quiet_branch.1, 20, 6, &sdp, &config);

        // Carry both branches past the epoch-0 close and the epoch-2 snapshot.
        let advance = |(mut state, mut pow): (LedgerState, PowState)| {
            for slot in [100u64, 110, 200] {
                state = apply_block(state, &mut pow, slot, 0, &sdp, &config);
            }
            (state, pow)
        };
        let (busy_branch, _) = advance(busy_branch);
        let (quiet_branch, _) = advance(quiet_branch);

        // Both branches enter epoch 1 at the baseline: no epoch had closed
        // when epoch 1's value was snapshotted, so the schedule had not
        // started for either of them.
        let epoch_1_difficulty = PowTarget::from(blend_config.base_difficulty);

        // Each tip's epoch 2 then reads its own epoch 0: 100 transactions over
        // two blocks on one branch, 10 over two on the other. The shared
        // ancestor's 4 transactions are counted once on each — never twice, and
        // never leaked from the branch that lost.
        assert_eq!(
            busy_branch.epoch_state.blend_pow_difficulty,
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(100, 2),
                epoch_1_difficulty,
                blend_config
            )
        );
        assert_eq!(
            quiet_branch.epoch_state.blend_pow_difficulty,
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(10, 2),
                epoch_1_difficulty,
                blend_config
            )
        );
        // The busier branch admits fewer `PoW`-backed messages.
        assert!(
            busy_branch.epoch_state.blend_pow_difficulty
                < quiet_branch.epoch_state.blend_pow_difficulty
        );
    }

    #[test]
    fn blend_difficulty_reads_a_skipped_epoch_as_no_load() {
        let config = config();
        let blend_config = &config.pow_config.blend;
        let sdp = SdpLedger::new(0.into());
        let mut state = genesis_state(&[utxo()]);
        let mut pow = PowState::new();

        // A single busy block in epoch 0, then no block at all in epoch 1.
        state = apply_block(state, &mut pow, 10, 1_000, &sdp, &config);

        // Epoch 0 -> 2. Epoch 2's snapshot slot (160) has already passed
        // unused, so its difficulty carries over unretargeted.
        state = apply_block(state, &mut pow, 222, 7, &sdp, &config);
        assert_eq!(state.epoch_state.epoch, 2);
        let epoch_2_difficulty = state.epoch_state.blend_pow_difficulty;
        assert_eq!(
            epoch_2_difficulty,
            PowTarget::from(blend_config.base_difficulty)
        );

        // Epoch 2 -> 3: the epoch that closed last is the skipped epoch 1,
        // which produced no block at all, so epoch 3 sees no load and eases
        // by the full clamp step — epoch 0's 1000 transactions were closed
        // one epoch too early to be read.
        state = apply_block(state, &mut pow, 230, 7, &sdp, &config);
        state = apply_block(state, &mut pow, 300, 0, &sdp, &config);
        assert_eq!(state.epoch_state.epoch, 3);
        assert_eq!(
            state.epoch_state.blend_pow_difficulty,
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(0, 0),
                epoch_2_difficulty,
                blend_config
            )
        );
    }

    /// A declaration that lapses past `inactivity_period` but has not yet
    /// been withdrawn must be filtered out of the `EpochState` snapshot
    /// built at a later epoch.
    #[test]
    fn epoch_state_snapshot_excludes_inactive_declaration() {
        let leader_utxo = utxo();
        let (_sdp_utxo_key, sdp_utxo) = utxo_with_sk();
        let new_utxo = utxo();
        let config = config();
        let epoch_length = config.epoch_length();
        let (mut ledger0, genesis) = ledger(&[leader_utxo, sdp_utxo], config);

        // Declare at slot 1 (epoch 0). The declaration's `active` field is
        // initialized to `created + 2 = 2`.
        let (head0, declare, _) = apply_and_add_utxo_and_declaration(
            &mut ledger0,
            genesis,
            1,
            leader_utxo,
            new_utxo,
            sdp_utxo,
        );

        // Advance to epoch 5 (one-by-one).
        // With inactivity_period=2, the declaration goes inactive at epoch 5.
        // It shouldn't be in the snapshot, but should still exist in the live SDP
        // ledger because the user has not yet withdrawn it.
        let mut ledger = ledger0.clone();
        let mut head = head0;
        for epoch in 1..=5u64 {
            head = update_ledger(&mut ledger, head, epoch * epoch_length, leader_utxo).unwrap();
        }
        assert_eq!(
            ledger.states[&head].cryptarchia_ledger.epoch_state.epoch,
            Epoch::new(5)
        );
        assert!(
            ledger.states[&head]
                .mantle_ledger
                .sdp
                .get_declaration(&declare.id())
                .is_some(),
            "declaration must still be in the live SDP ledger because it is not yet withdrawn"
        );
        assert!(
            declaration_in_snapshot(&ledger, &head, &declare.id()).is_none(),
            "inactive declaration must be filtered out of the EpochState snapshot"
        );

        // Jump from epoch 0 to 5, and check the same conditions
        let mut ledger = ledger0;
        head = update_ledger(&mut ledger, head0, 5 * epoch_length, leader_utxo).unwrap();
        assert_eq!(
            ledger.states[&head].cryptarchia_ledger.epoch_state.epoch,
            Epoch::new(5)
        );
        assert!(
            ledger.states[&head]
                .mantle_ledger
                .sdp
                .get_declaration(&declare.id())
                .is_some(),
            "declaration must still be in the live SDP ledger before GC removes it"
        );
        assert!(
            declaration_in_snapshot(&ledger, &head, &declare.id()).is_none(),
            "inactive declaration must be filtered out of the EpochState snapshot"
        );
    }

    #[test]
    fn test_new_utxos_becoming_eligible_after_stake_distribution_stabilizes() {
        let utxo_1 = utxo();
        let utxo = utxo();
        let config = config();
        let epoch_length = config.epoch_length();

        let (mut ledger, genesis) = ledger(&[utxo], config);

        // EPOCH 0
        // mint a new utxo to be used for leader elections in upcoming epochs
        let h_0_1 = apply_and_add_utxo(&mut ledger, genesis, 1, utxo, utxo_1);

        // the new utxo is not yet eligible for leader elections
        assert!(matches!(
            update_ledger(&mut ledger, h_0_1, 2, utxo_1),
            Err(LedgerError::InvalidProof),
        ));

        // EPOCH 1
        for i in epoch_length..(2 * epoch_length) {
            // the newly minted utxo is still not eligible in the following epoch since the
            // stake distribution snapshot is taken at the beginning of the previous epoch
            assert!(matches!(
                update_ledger(&mut ledger, h_0_1, i, utxo_1),
                Err(LedgerError::InvalidProof),
            ));
        }

        // EPOCH 2
        // the utxo is finally eligible 2 epochs after it was first minted
        //
        // First, advance to epoch 1 using the `utxo` in genesis,
        // because SDP ledger doesn't support epoch jumps yet.
        let h_1_1 = update_ledger(&mut ledger, h_0_1, epoch_length, utxo).unwrap();
        // Then, try to advance to epoch 2 using `utxo_1`
        update_ledger(&mut ledger, h_1_1, 2 * epoch_length, utxo_1).unwrap();
    }

    /// Verifies that the TSI chain is computed correctly across epoch
    /// transitions.
    #[test]
    fn test_total_stake_inference_chain_across_epoch_transitions() {
        let utxo = utxo();
        let config = config();
        assert_eq!(config.epoch_length(), 100);
        let (mut ledger, genesis) = ledger(&[utxo], config.clone());
        let inference = stake_inference_from_config(&config);

        let ts_genesis = ledger.states[&genesis]
            .cryptarchia_ledger
            .epoch_state
            .total_stake;
        assert_eq!(ts_genesis, 10_000);

        // Epoch 0 ----------------------------------
        // Produce 3 blocks in the slot window [0, 59]
        let h1 = update_ledger(&mut ledger, genesis, 1, utxo).unwrap();
        let h2 = update_ledger(&mut ledger, h1, 2, utxo).unwrap();
        let h3 = update_ledger(&mut ledger, h2, 3, utxo).unwrap();
        assert_eq!(
            ledger.states[&h3]
                .cryptarchia_ledger
                .block_density
                .current_block_density(),
            3
        );
        // A block outside the slot window is not counted
        let h4 = update_ledger(&mut ledger, h3, 60, utxo).unwrap();
        assert_eq!(
            ledger.states[&h3]
                .cryptarchia_ledger
                .block_density
                .current_block_density(),
            3
        );

        // Epoch 0 -> 1 transition --------------------
        // slot 100 triggers the transition and also counts in epoch 1's window [100,
        // 159]
        let h5 = update_ledger(&mut ledger, h4, 100, utxo).unwrap();
        let ts1 = inference.total_stake_inference::<PRECISION>(ts_genesis, 3);
        assert_eq!(ledger.states[&h5].cryptarchia_ledger.epoch_state.epoch, 1);
        assert_eq!(
            ledger.states[&h5]
                .cryptarchia_ledger
                .epoch_state
                .total_stake,
            ts1,
        );

        // Epoch 1 ----------------------------------
        // 1 more block in [100, 159] (slot 100 already counted → total 2)
        let h6 = update_ledger(&mut ledger, h5, 101, utxo).unwrap();
        assert_eq!(
            ledger.states[&h6]
                .cryptarchia_ledger
                .block_density
                .current_block_density(),
            2
        );

        // Epoch 1 -> 2 transition --------------------
        let h7 = update_ledger(&mut ledger, h6, 200, utxo).unwrap();
        let ts2 = inference.total_stake_inference::<PRECISION>(ts1, 2);
        assert_eq!(ledger.states[&h7].cryptarchia_ledger.epoch_state.epoch, 2);
        assert_eq!(
            ledger.states[&h7]
                .cryptarchia_ledger
                .epoch_state
                .total_stake,
            ts2,
        );
    }

    #[test]
    fn test_update_epoch_state_with_outdated_slot_error() {
        let utxo = utxo();
        let (ledger, genesis) = ledger(&[utxo], config());

        let ledger_state = ledger.state(&genesis).unwrap().clone();
        let ledger_config = ledger.config();

        let slot = Slot::genesis().strict_add(10.into());
        let ledger_state2 = ledger_state
            .cryptarchia_ledger
            .update_epoch_state::<HeaderId>(
                slot,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                ledger_config,
            )
            .expect("Ledger needs to move forward");

        let slot2 = Slot::genesis().strict_add(1.into());
        let update_epoch_err = ledger_state2
            .update_epoch_state::<HeaderId>(
                slot2,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                ledger_config,
            )
            .err();

        // Time cannot flow backwards
        match update_epoch_err {
            Some(LedgerError::InvalidSlot { parent, block })
                if parent == slot && block == slot2 => {}
            _ => panic!("error does not match the LedgerError::InvalidSlot pattern"),
        }
    }

    #[test]
    fn test_invalid_aged_root_rejected() {
        let utxo = utxo();
        let (ledger, genesis) = ledger(&[utxo], config());
        let ledger_state = ledger.state(&genesis).unwrap().clone().cryptarchia_ledger;
        let slot = Slot::genesis().strict_add(1.into());
        let proof = DummyProof {
            public: LeaderPublic {
                aged_root: Fr::from(0u8), // Invalid aged root
                latest_root: ledger_state.latest_utxos().root(),
                epoch_nonce: ledger_state.epoch_state.nonce,
                slot: slot.into(),
                lottery_0: ledger_state.epoch_state.lottery_0,
                lottery_1: ledger_state.epoch_state.lottery_1,
            },
            leader_key: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            voucher_cm: VoucherCm::default(),
        };
        let update_err = ledger_state
            .try_apply_proof::<_, ()>(slot, &proof, ledger.config())
            .err();

        assert_eq!(Some(LedgerError::InvalidProof), update_err);
    }

    #[test]
    fn test_invalid_latest_root_rejected() {
        let utxo = utxo();
        let (ledger, genesis) = ledger(&[utxo], config());
        let ledger_state = ledger.state(&genesis).unwrap().clone().cryptarchia_ledger;
        let slot = Slot::genesis().strict_add(1.into());
        let proof = DummyProof {
            public: LeaderPublic {
                aged_root: ledger_state.aged_utxos().root(),
                latest_root: BigUint::from(1u8).into(), // Invalid latest root
                epoch_nonce: ledger_state.epoch_state.nonce,
                slot: slot.into(),
                lottery_0: ledger_state.epoch_state.lottery_0,
                lottery_1: ledger_state.epoch_state.lottery_1,
            },
            leader_key: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            voucher_cm: VoucherCm::default(),
        };
        let update_err = ledger_state
            .try_apply_proof::<_, ()>(slot, &proof, ledger.config())
            .err();

        assert_eq!(Some(LedgerError::InvalidProof), update_err);
    }

    fn create_tx_with_transfer(
        inputs: &[(&ZkKey, &Utxo)],
        outputs: Vec<Note>,
    ) -> (SignedOps<Unverified, StandardMode>, TransferOp, ZkSignature) {
        let sks = inputs
            .iter()
            .map(|(sk, _)| (*sk).clone())
            .collect::<Vec<_>>();
        let inputs = inputs.iter().map(|(_, utxo)| utxo.id()).collect::<Vec<_>>();
        let transfer_op = TransferOp::new(
            Inputs::try_new(inputs).expect("Invalid inputs size"),
            Outputs::try_new(outputs).expect("Invalid outputs size"),
        );
        let mantle_tx = Ops::from([Op::Transfer(transfer_op.clone())]);
        let transfer_sig = ZkKey::multi_sign(&sks, &mantle_tx.hash().to_fr()).unwrap();
        let op_proofs = OpProofs::from([ZkSig(transfer_sig.clone())]);
        let tx = SignedOps::from_parts(mantle_tx, op_proofs).unwrap();
        (tx, transfer_op, transfer_sig)
    }

    #[test]
    fn test_invalid_double_spend_transfer() {
        let note_sk = ZkKey::from(BigUint::from(1u8));
        let output_note_sk = ZkKey::from(BigUint::from(2u8));
        let input_note = Note::new(100, note_sk.to_public_key());
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: input_note,
        };

        let output_note = Note::new(200, output_note_sk.to_public_key());

        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_proof) = create_tx_with_transfer(
            &[(&note_sk, &input_utxo), (&note_sk, &input_utxo)],
            vec![output_note],
        );
        let signed_operation =
            SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
        let _fees = tx.total_gas_cost::<MainnetGasProfile>(&GasPrices::new(0, 0));

        let result = ledger_state.try_apply_transfer::<(), MainnetGasProfile>(signed_operation);

        assert!(result.is_err());
    }

    #[test]
    fn test_tx_processing_valid_transaction() {
        let note_sk = ZkKey::from(BigUint::from(1u8));
        let output_note1_sk = ZkKey::from(BigUint::from(2u8));
        let output_note2_sk = ZkKey::from(BigUint::from(3u8));
        let input_note = Note::new(11000, note_sk.to_public_key());
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: input_note,
        };

        let output_note1 = Note::new(4000, output_note1_sk.to_public_key());
        let output_note2 = Note::new(3000, output_note2_sk.to_public_key());

        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_proof) =
            create_tx_with_transfer(&[(&note_sk, &input_utxo)], vec![output_note1, output_note2]);

        let signed_operation =
            SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
        let _fees = tx.total_gas_cost::<MainnetGasProfile>(&GasPrices::new(0, 0));
        let (new_state, balance, events) = ledger_state
            .try_apply_transfer::<(), MainnetGasProfile>(signed_operation)
            .unwrap();

        assert_eq!(
            balance,
            i128::from(input_note.value - output_note1.value - output_note2.value)
        );
        assert!(events.is_empty());

        // Verify input was consumed
        assert!(!new_state.utxos.contains(&input_utxo.id()));

        // Verify outputs were created
        let (_, transfer_op, _) =
            create_tx_with_transfer(&[(&note_sk, &input_utxo)], vec![output_note1, output_note2]);
        let output_utxo1 = transfer_op.outputs.utxo_by_index(0, &transfer_op).unwrap();
        let output_utxo2 = transfer_op.outputs.utxo_by_index(1, &transfer_op).unwrap();

        assert!(new_state.utxos.contains(&output_utxo1.id()));
        assert!(new_state.utxos.contains(&output_utxo2.id()));

        // The new outputs can be spent in future transactions
        let (tx, transfer_op, transfer_proof) = create_tx_with_transfer(
            &[
                (&output_note1_sk, &output_utxo1),
                (&output_note2_sk, &output_utxo2),
            ],
            vec![],
        );

        let signed_operation =
            SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
        let _fees = tx.total_gas_cost::<MainnetGasProfile>(&GasPrices::new(0, 0));
        let (final_state, final_balance, events) = new_state
            .try_apply_transfer::<(), MainnetGasProfile>(signed_operation)
            .unwrap();

        assert_eq!(
            final_balance,
            i128::from(output_note1.value + output_note2.value)
        );
        assert!(!final_state.utxos.contains(&output_utxo1.id()));
        assert!(!final_state.utxos.contains(&output_utxo2.id()));
        assert!(events.is_empty());
    }

    #[test]
    fn test_tx_processing_invalid_input() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_note = Note::new(1000, input_sk.to_public_key());
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: input_note,
        };

        let non_existent_utxo_1 = Utxo {
            op_id: [1u8; 32],
            output_index: 1,
            note: input_note,
        };

        let non_existent_utxo_2 = Utxo {
            op_id: [2u8; 32],
            output_index: 0,
            note: input_note,
        };

        let non_existent_utxo_3 = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(999, Fr::from(BigUint::from(1u8)).into()),
        };

        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);

        let invalid_utxos = [
            non_existent_utxo_1,
            non_existent_utxo_2,
            non_existent_utxo_3,
        ];

        for non_existent_utxo in invalid_utxos {
            let (_tx, transfer_op, transfer_proof) =
                create_tx_with_transfer(&[(&ZkKey::zero(), &non_existent_utxo)], vec![]);

            let signed_operation =
                SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
            let result = ledger_state
                .clone()
                .try_apply_transfer::<(), MainnetGasProfile>(signed_operation);
            assert!(matches!(result, Err(LedgerError::Mantle(_))));
        }
    }

    #[test]
    fn test_tx_processing_insufficient_balance() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_note = Note::new(1, input_sk.to_public_key());
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: input_note,
        };

        let output_note = Note::new(1, Fr::from(BigUint::from(2u8)).into());

        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (_tx, transfer_op, transfer_proof) =
            create_tx_with_transfer(&[(&input_sk, &input_utxo)], vec![output_note, output_note]);

        let signed_operation =
            SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
        let (_, balance, events) = ledger_state
            .clone()
            .try_apply_transfer::<(), MainnetGasProfile>(signed_operation)
            .unwrap();
        assert_eq!(balance, -1);
        assert!(events.is_empty());

        let (_tx, transfer_op, transfer_proof) =
            create_tx_with_transfer(&[(&input_sk, &input_utxo)], vec![output_note]);
        let signed_operation =
            SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
        assert_eq!(
            ledger_state
                .try_apply_transfer::<(), MainnetGasProfile>(signed_operation)
                .unwrap()
                .1,
            0
        );
    }

    #[test]
    fn test_tx_processing_no_outputs() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_note = Note::new(10000, input_sk.to_public_key());
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: input_note,
        };

        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_proof) =
            create_tx_with_transfer(&[(&input_sk, &input_utxo)], vec![]);

        let signed_operation =
            SignedOperation::new(transfer_op, transfer_proof).into_state_trusted();
        let _fees = tx.total_gas_cost::<MainnetGasProfile>(&GasPrices::new(0, 0));
        let result = ledger_state.try_apply_transfer::<(), MainnetGasProfile>(signed_operation);
        assert!(result.is_ok());

        let (new_state, balance, events) = result.unwrap();
        assert_eq!(balance, 10000);
        assert!(events.is_empty());

        // Verify input was consumed
        assert!(!new_state.utxos.contains(&input_utxo.id()));
    }

    #[test]
    fn test_epoch_state_for_slot_with_empty_epochs() {
        let utxo = utxo();
        let config = config();
        let epoch_length = config.epoch_length();
        let ledger_state = genesis_state(&[utxo]);

        // Genesis state is at epoch 0, with epoch_state for epoch 0 and
        // next_epoch_state for epoch 1
        assert_eq!(ledger_state.epoch_state.epoch, 0);
        assert_eq!(ledger_state.next_epoch_state.epoch, 1);
        let initial_total_stake = ledger_state.epoch_state.total_stake;

        // Query for epoch 0 (current epoch) - should return epoch_state
        let epoch_0_slot: Slot = (epoch_length - 1).into();
        let epoch_0_state = ledger_state
            .epoch_state_for_slot::<HeaderId>(
                epoch_0_slot,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .expect("Should return epoch state for current epoch");
        assert_eq!(epoch_0_state.epoch, 0);
        assert_eq!(epoch_0_state.total_stake, initial_total_stake);

        // Query for epoch 1
        // Since epoch 0 has no block, total stake should be reduced
        let epoch_1_slot: Slot = (epoch_length + 1).into();
        let epoch_1_state = ledger_state
            .epoch_state_for_slot::<HeaderId>(
                epoch_1_slot,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .expect("Should return epoch state for next epoch");
        assert_eq!(epoch_1_state.epoch, 1);
        // With 0 density and LEARNING_RATE=1, total stake drops to minimum (1)
        assert_eq!(
            epoch_1_state.total_stake, 1,
            "Total stake should drop to minimum for empty epochs"
        );

        // Query for epoch 3 (multiple skipped epochs) - stake stays at minimum
        let epoch_2_slot: Slot = (2 * epoch_length + 1).into();
        let epoch_2_state = ledger_state
            .epoch_state_for_slot::<HeaderId>(
                epoch_2_slot,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .expect("Should synthesize epoch state for skipped epoch");
        assert_eq!(epoch_2_state.epoch, 2);
        assert_eq!(
            epoch_2_state.total_stake, 1,
            "Total stake should remain at minimum"
        );

        // Verify nonce and utxos are preserved from current state
        assert_eq!(epoch_2_state.nonce, ledger_state.nonce);
        assert_eq!(epoch_2_state.utxos, ledger_state.utxos);
    }

    /// Test that a proof built from the jumped (synthesized) epoch state can be
    /// applied successfully
    #[test]
    fn test_try_apply_header_with_proof_from_jumped_epoch() {
        let utxo = utxo();
        let config = config();
        let genesis_state = genesis_state(&[utxo]);

        // First, apply a header from epoch 0 to increase block density
        let slot = Slot::from(1);
        assert_eq!(config.epoch(slot), 0);
        let proof = generate_proof(&genesis_state, &utxo, slot);
        let ledger_state_1 = genesis_state
            .try_apply_header::<DummyProof, HeaderId>(
                slot,
                &proof,
                &UncleSlots::default(),
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .unwrap();

        // Now, apply a header from the 2nd slot of epoch 2
        let slot = Slot::from(config.epoch_length() * 2 + 1);
        assert_eq!(config.epoch(slot), 2);

        // First, synthesize epoch state for epoch 2
        let synthesized_ledger_state = ledger_state_1
            .clone()
            .update_epoch_state::<HeaderId>(
                slot,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .unwrap();
        assert_eq!(synthesized_ledger_state.slot, slot);

        // Build a proof with the synthesized epoch state
        let proof = generate_proof(&synthesized_ledger_state, &utxo, slot);

        // Apply it to `ledger_state_1`.
        // Must succeed if epoch state in `ledger_state_1` is jumped
        // correctly to epoch 2 as the same as `synthesized_ledger_state`.
        let ledger_state_2 = ledger_state_1
            .clone()
            .try_apply_header::<DummyProof, HeaderId>(
                slot,
                &proof,
                &UncleSlots::default(),
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .unwrap();
        assert_eq!(ledger_state_2.slot, slot);
        assert_ne!(ledger_state_2.nonce, ledger_state_1.nonce); // advanced
        assert_eq!(ledger_state_2.epoch_state.epoch, 2);
    }

    fn stake_inference_from_config(config: &Config) -> StakeInference {
        StakeInference::new(
            config.consensus_config.stake_inference_learning_rate(),
            config.consensus_config.slot_activation_coeff().as_f64(),
            config.total_stake_inference_period(),
        )
    }

    /// If the network is constantly full, execution gas must get more
    /// expensive.
    #[test]
    fn execution_price_rises_under_sustained_maximum_load() {
        let mut state = genesis_state(&[utxo()]);
        let genesis_price = state.execution_base_fee().into_inner();

        for _ in 0..10_000 {
            state = state.update_execution_market(crate::EXECUTION_GAS_LIMIT);
        }

        assert!(
            state.execution_base_fee().into_inner() > genesis_price,
            "the execution base fee never rose above {genesis_price} under sustained \
             blocks at the gas limit; the market cannot price demand from its genesis \
             state"
        );
    }

    /// After a quiet stretch, returning demand must be able to push the
    /// execution price back up.
    #[test]
    fn execution_price_recovers_when_demand_returns_after_quiet_blocks() {
        let mut state = genesis_state(&[utxo()]);

        for _ in 0..100 {
            state = state.update_execution_market(0.into());
        }

        let quiet_price = state.execution_base_fee().into_inner();

        for _ in 0..10_000 {
            state = state.update_execution_market(crate::EXECUTION_GAS_LIMIT);
        }

        assert!(
            state.execution_base_fee().into_inner() > quiet_price,
            "the execution base fee is stuck at {quiet_price} after quiet blocks and does \
             not respond to returning demand; the market is dead from this state on"
        );
    }

    /// If storage is constantly in heavy use, storage gas must get more
    /// expensive.
    #[test]
    fn storage_price_rises_under_sustained_heavy_usage() {
        let genesis_price = GENESIS_STORAGE_GAS_PRICE.into_inner();
        let mut price = GENESIS_STORAGE_GAS_PRICE;
        let mut ema: Gas = 0.into();
        let heavy_usage: Gas = 1_000_000.into();

        for _ in 0..1_000 {
            (price, ema) = update_storage_market(price, heavy_usage, ema);
        }

        assert!(
            price.into_inner() > genesis_price,
            "the storage gas price never rose above {genesis_price} under sustained \
             heavy usage; the market cannot price demand from its genesis state"
        );
    }

    /// After quiet epochs, returning usage must be able to push the storage
    /// price back up.
    #[test]
    fn storage_price_recovers_when_usage_returns_after_quiet_epochs() {
        let mut price = GENESIS_STORAGE_GAS_PRICE;
        let mut ema: Gas = 0.into();
        let heavy_usage: Gas = 1_000_000.into();

        for _ in 0..4 {
            (price, ema) = update_storage_market(price, heavy_usage, ema);
        }
        for _ in 0..4 {
            (price, ema) = update_storage_market(price, 0.into(), ema);
        }

        let quiet_price = price.into_inner();

        for _ in 0..1_000 {
            (price, ema) = update_storage_market(price, heavy_usage, ema);
        }

        assert!(
            price.into_inner() > quiet_price,
            "the storage gas price is stuck at {quiet_price} after quiet epochs and does \
             not respond to returning usage; the market is dead from this state on"
        );
    }

    #[test]
    fn test_storage_market_update() {
        // empty epoch
        assert_eq!(
            (438.into(), 340.into()),
            update_storage_market(500.into(), 0.into(), 681.into())
        );

        // Some random values
        // 1) raw = 113 * 1.125 = 127.125 -> 128
        assert_eq!(
            (128.into(), 450.into()),
            update_storage_market(113.into(), 600.into(), 300.into())
        );

        // 2) raw = 113 * 0.875 = 98.875 -> 99
        assert_eq!(
            (99.into(), 500.into()),
            update_storage_market(113.into(), 200.into(), 800.into())
        );

        // 3) raw = 221 * 1.125 = 248.625 -> 249
        assert_eq!(
            (249.into(), 550.into()),
            update_storage_market(221.into(), 900.into(), 200.into())
        );

        // 4) raw = 221 * 0.875 = 193.375 -> 194
        assert_eq!(
            (194.into(), 500.into()),
            update_storage_market(221.into(), 100.into(), 900.into())
        );

        // 5) raw = 345 * 1.125 = 388.125 -> 389
        assert_eq!(
            (389.into(), 165.into()),
            update_storage_market(345.into(), 250.into(), 80.into())
        );

        // 6) raw = 345 * 0.875 = 301.875 -> 302
        assert_eq!(
            (302.into(), 400.into()),
            update_storage_market(345.into(), 50.into(), 750.into())
        );

        // 7) raw = 517 * 1.125 = 581.625 -> 582
        assert_eq!(
            (582.into(), 160.into()),
            update_storage_market(517.into(), 220.into(), 100.into())
        );

        // 8) raw = 517 * 0.875 = 452.375 -> 453
        assert_eq!(
            (453.into(), 485.into()),
            update_storage_market(517.into(), 120.into(), 850.into())
        );

        // 9) raw = 999 * 1.125 = 1123.875 -> 1124
        assert_eq!(
            (1124.into(), 650.into()),
            update_storage_market(999.into(), 1000.into(), 300.into())
        );

        // 10) raw = 999 * 0.875 = 874.125 -> 875
        assert_eq!(
            (875.into(), 650.into()),
            update_storage_market(999.into(), 300.into(), 1000.into())
        );
    }

    #[test]
    fn test_execution_market_update() {
        // Create a base ledger first
        let mut ledger = LedgerState::from_utxos([], &config(), Fr::ZERO);

        // 1) G_avg = (1_700_000 + 9*1_596_730)/10 = 1_607_057
        // price = ceil(10_000 * (11_177_110 + 1_607_057) / 12_773_840) = 10_009
        ledger.execution_base_fee = 10_000.into();
        ledger.average_execution_gas = 1_596_730.into();
        ledger = ledger.update_execution_market(1_700_000.into());
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (10_009.into(), 1_607_057.into())
        );

        // 2) G_avg = (1_400_000 + 9*1_596_730)/10 = 1_577_057
        // price = ceil(10_000 * (11_177_110 + 1_577_057) / 12_773_840) = 9_985
        ledger.execution_base_fee = 10_000.into();
        ledger.average_execution_gas = 1_596_730.into();
        ledger = ledger.update_execution_market(1_400_000.into());
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (9_985.into(), 1_577_057.into())
        );

        // 3) G_avg = (2_500_000 + 9*1_000_000)/10 = 1_150_000
        // price = ceil(20_000 * (11_177_110 + 1_150_000) / 12_773_840) = 19_301
        ledger.execution_base_fee = 20_000.into();
        ledger.average_execution_gas = 1_000_000.into();
        ledger = ledger.update_execution_market(2_500_000.into());
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (19_301.into(), 1_150_000.into())
        );

        // 4) G_avg = (500_000 + 9*2_000_000)/10 = 1_850_000
        // price = ceil(15_000 * (11_177_110 + 1_850_000) / 12_773_840) = 15_298
        ledger.execution_base_fee = 15_000.into();
        ledger.average_execution_gas = 2_000_000.into();
        ledger = ledger.update_execution_market(500_000.into());
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (15_298.into(), 1_850_000.into())
        );

        // 5) G_avg = (1_000_000 + 9*1_800_000)/10 = 1_720_000
        // price = ceil(30_000 * (11_177_110 + 1_720_000) / 12_773_840) = 30_290
        ledger.execution_base_fee = 30_000.into();
        ledger.average_execution_gas = 1_800_000.into();
        ledger = ledger.update_execution_market(1_000_000.into());
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (30_290.into(), 1_720_000.into())
        );
    }

    #[test]
    fn test_accumulated_storage_gas_drives_next_epoch_price() {
        let config = config();
        let mut ledger = genesis_state(&[utxo()]);

        // Seed a known storage-market state, then accumulate the storage gas
        // that applied transactions consume during the epoch.
        ledger.storage_gas_price = 113.into();
        ledger.storage_gas_ema = 300.into();
        let ledger = ledger.add_storage_gas_consumed(600.into()).unwrap();

        // Cross a single epoch boundary so the storage price is recomputed.
        let slot: Slot = (config.epoch_length() + 1).into();
        assert_eq!(config.epoch(slot), 1);
        let rotated = ledger
            .update_epoch_state::<HeaderId>(
                slot,
                &SdpLedger::new(0.into()),
                &PowState::new(),
                &config,
            )
            .unwrap();

        // The accumulated 600 must reach the price update: with a starting price
        // of 113 and EMA 300 that yields (128, 450) after ceiling division.
        assert_eq!(rotated.storage_gas_price, 128.into());
        assert_eq!(rotated.storage_gas_ema, 450.into());
        // The counter resets for the new epoch.
        assert_eq!(rotated.storage_gas_consumed_in_epoch, 0.into());
    }
}
