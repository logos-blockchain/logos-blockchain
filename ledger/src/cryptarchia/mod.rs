mod block_density;
mod stake;

use std::sync::{Arc, LazyLock};

use derivative::Derivative;
use lb_core::{
    crypto::{ZkDigest, ZkHasher},
    mantle::{
        GenesisTx, NoteId, TxHash, Utxo, Value,
        gas::{Gas, GasConstants},
        ops::transfer::TransferOp,
    },
    proofs::leader_proof::{self, LeaderPublic},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_groth16::{Fr, fr_from_bytes};
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_utxotree::MerklePath;

use crate::cryptarchia::{
    block_density::BlockDensity,
    stake::{PRECISION, StakeInference},
};

// corresponds to the denominator of q
const EXECUTION_MARKET_EMA_DENOMINATOR: u128 = 10;
// Corresponds to the numerator of q
const EXECUTION_MARKET_EMA_PREV_WEIGHT: u128 = 9;
// Corresponds to 7 * G_target because the numerator is 1 + phi (G_avg -
// G_target)
const EXECUTION_MARKET_BASE_FEE_NUMERATOR: u128 = 11_176_760;
// Corresponds to 8 * G_target because the denominator is 1 + phi (G_avg -
// // G_target)
const EXECUTION_MARKET_BASE_FEE_DENOMINATOR: u128 = 12_773_440;

// Corresponds to the denominator of 1/beta
const STORAGE_MARKET_EMA_DENOMINATOR: u128 = 2;
// Corresponds to the denominator of 1+ alpha and 1-alpha
const STORAGE_MARKET_CLAMP_DENOMINATOR: u128 = 8;
// Corresponds to the numerator of 1-alpha
const STORAGE_MARKET_CLAMP_DOWN_NUMERATOR: u128 = 7;
// Corresponds to the numerator of 1+alpha
const STORAGE_MARKET_CLAMP_UP_NUMERATOR: u128 = 9;

pub type UtxoTree = lb_utxotree::UtxoTree<NoteId, Utxo, ZkHasher>;
use super::{Balance, Config, LedgerError};
use crate::{WINDOW_SIZE, mantle::sdp::locked_notes::LockedNotes};

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochState {
    /// The epoch this snapshot is for
    pub epoch: Epoch,
    /// value of the ledger nonce after `epoch_period_nonce_buffer` slots from
    /// the beginning of the epoch
    #[cfg_attr(feature = "serde", serde(with = "lb_groth16::serde::serde_fr"))]
    pub nonce: Fr,
    /// stake distribution snapshot taken at the beginning of the epoch
    /// (in practice, this is equivalent to the utxos the are spendable at the
    /// beginning of the epoch)
    pub utxos: UtxoTree,
    pub total_stake: Value,
    /// Lottery values computed based on `total_stake`
    #[cfg_attr(feature = "serde", serde(with = "lb_groth16::serde::serde_fr"))]
    pub lottery_0: Fr,
    #[cfg_attr(feature = "serde", serde(with = "lb_groth16::serde::serde_fr"))]
    pub lottery_1: Fr,
}

impl EpochState {
    fn update_from_ledger(self, ledger: &LedgerState, config: &Config) -> Self {
        let nonce_snapshot_slot = config.nonce_snapshot(self.epoch);
        let nonce = if ledger.slot < nonce_snapshot_slot {
            ledger.nonce
        } else {
            self.nonce
        };

        let stake_snapshot_slot = config.stake_distribution_snapshot(self.epoch);
        let utxos = if ledger.slot < stake_snapshot_slot {
            ledger.utxos.clone()
        } else {
            self.utxos
        };
        Self {
            epoch: self.epoch,
            nonce,
            utxos,
            total_stake: self.total_stake,
            lottery_0: self.lottery_0,
            lottery_1: self.lottery_1,
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

    #[must_use]
    pub const fn total_stake(&self) -> Value {
        self.total_stake
    }

    #[must_use]
    pub const fn lottery_values(&self) -> (Fr, Fr) {
        (self.lottery_0, self.lottery_1)
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Derivative)]
#[derivative(Clone, Eq, PartialEq)]
pub struct LedgerState {
    // All available Unspent Transtaction Outputs (UTXOs) at the current slot
    // TODO: move UTXOs in the mantle ledger. There is no reason to keep them here
    pub utxos: UtxoTree,
    // randomness contribution
    #[cfg_attr(feature = "serde", serde(with = "lb_groth16::serde::serde_fr"))]
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
    #[cfg_attr(feature = "serde", serde(with = "serde_arrays"))]
    fee_window: [Value; WINDOW_SIZE],
    // Smoothed Average Execution Gas used up to the last block
    average_execution_gas: Gas,
    // Execution Base Fee that are burned and minimum required to pay.
    execution_base_fee: Gas,
    // Exponential Moving Average Storage Gas used in the currect epoch
    storage_gas_ema: Gas,
    // Actual storage Gas price of the currect epoch
    storage_gas_price: Gas,
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
    fn update_epoch_state<Id>(self, slot: Slot, config: &Config) -> Result<Self, LedgerError<Id>> {
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
            .update_from_ledger(&self, config);

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
        } else if new_epoch == current_epoch + 1 {
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
            let next_epoch_state = EpochState {
                epoch: new_epoch + 1,
                nonce: self.nonce,
                utxos: self.utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
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
                storage_gas_consumed_in_epoch: 0u64,
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
                (new_price, new_ema) = update_storage_market(new_price, 0u64, new_ema);
            }

            tracing::warn!(
                old_epoch = ?current_epoch,
                new_epoch = ?new_epoch,
                epochs_skipped = u32::from(new_epoch) - u32::from(current_epoch) - 1,
                old_total_stake = self.epoch_state.total_stake,
                new_total_stake = total_stake,
                slot = ?slot,
                "skipped epochs"
            );
            let block_density = BlockDensity::new(new_epoch, config);
            let epoch_state = EpochState {
                epoch: new_epoch,
                nonce: self.nonce,
                utxos: self.utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
            };
            let next_epoch_state = EpochState {
                epoch: new_epoch + 1,
                nonce: self.nonce,
                utxos: self.utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
            };
            Ok(Self {
                slot,
                next_epoch_state,
                epoch_state,
                block_density,
                storage_gas_consumed_in_epoch: 0u64,
                storage_gas_ema: new_ema,
                storage_gas_price: new_price,
                ..self
            })
        }
    }

    #[must_use]
    pub fn update_execution_market(self, block_execution_gas_consumed: Gas) -> Self {
        // First update the `average_execution_gas`
        let avg_numerator = u128::from(block_execution_gas_consumed)
            + EXECUTION_MARKET_EMA_PREV_WEIGHT * u128::from(self.average_execution_gas);
        let new_average_execution_gas: Gas =
            (avg_numerator / EXECUTION_MARKET_EMA_DENOMINATOR) as Gas;

        // Then update the `execution_base_fee`
        let fee_numerator = u128::from(self.execution_base_fee)
            * (EXECUTION_MARKET_BASE_FEE_NUMERATOR + u128::from(self.average_execution_gas));
        let new_base_fee = (fee_numerator / EXECUTION_MARKET_BASE_FEE_DENOMINATOR) as Gas;

        Self {
            average_execution_gas: new_average_execution_gas,
            execution_base_fee: new_base_fee,
            ..self
        }
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
        config: &Config,
    ) -> Result<Self, LedgerError<Id>>
    where
        LeaderProof: leader_proof::LeaderProof,
    {
        // First, synthesize epoch state for `slot` before update the ledger state.
        // Then, apply the proof and update the nonce. Finally, increment block density
        // since this function is called for a new block.
        Ok(self
            .update_epoch_state(slot, config)?
            .try_apply_proof(slot, proof, config)?
            .update_nonce(&proof.entropy(), slot)
            .increment_block_density(slot))
    }

    pub fn try_apply_transfer<Id, Constants: GasConstants>(
        mut self,
        locked_notes: &LockedNotes,
        transfer_op: &TransferOp,
        transfer_sig: &ZkSignature,
        tx_hash: TxHash,
    ) -> Result<(Self, Balance), LedgerError<Id>> {
        let mut balance: i128 = 0;
        let mut pks: Vec<ZkPublicKey> = vec![];
        for input in &transfer_op.inputs {
            if locked_notes.contains(input) {
                return Err(LedgerError::LockedNote(*input));
            }
            let utxo;
            (self.utxos, utxo) = self
                .utxos
                .remove(input)
                .map_err(|_| LedgerError::InvalidNote(*input))?;
            balance = balance
                .checked_add(utxo.note.value.into())
                .ok_or(LedgerError::Overflow)?;
            pks.push(utxo.note.pk);
        }

        if !ZkPublicKey::verify_multi(&pks, &tx_hash.0, transfer_sig) {
            return Err(LedgerError::InvalidProof);
        }

        for utxo in transfer_op.utxos() {
            if utxo.note.value == 0 {
                return Err(LedgerError::ZeroValueNote);
            }
            balance = balance
                .checked_sub(utxo.note.value.into())
                .ok_or(LedgerError::Overflow)?;
            self.utxos = self.utxos.insert(utxo.id(), utxo).0;
        }
        Ok((self, balance))
    }

    fn update_nonce(self, contrib: &Fr, slot: Slot) -> Self {
        // constants and structure as defined in the Mantle spec:
        // https://www.notion.so/Cryptarchia-v1-Protocol-Specification-21c261aa09df810cb85eff1c76e5798c
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

    fn increment_block_density(self, slot: Slot) -> Self {
        let mut block_density = self.block_density.clone();
        block_density.increment_block_density(slot);
        Self {
            block_density,
            ..self
        }
    }

    pub const fn update_fee_window(&mut self, index: usize, total_fee: u64) {
        self.fee_window[index] = total_fee;
    }

    #[must_use]
    pub const fn get_fee_from_index(&self, index: usize) -> Value {
        self.fee_window[index]
    }

    #[must_use]
    pub fn get_summed_fees(&self) -> u128 {
        self.fee_window.iter().map(|x| u128::from(*x)).sum()
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

    #[must_use]
    pub const fn latest_utxos(&self) -> &UtxoTree {
        &self.utxos
    }

    #[must_use]
    pub const fn execution_base_fee(&self) -> &Gas {
        &self.execution_base_fee
    }

    #[must_use]
    pub const fn storage_gas_price(&self) -> &Gas {
        &self.storage_gas_price
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
        config: &Config,
    ) -> Result<EpochState, LedgerError<Id>> {
        Ok(self
            .clone()
            .update_epoch_state(slot, config)?
            .epoch_state()
            .clone())
    }

    pub fn from_genesis_tx<Id>(
        tx: impl GenesisTx,
        config: &Config,
        epoch_nonce: Fr,
    ) -> Result<Self, LedgerError<Id>> {
        let transfer_op = tx.genesis_transfer();
        if !transfer_op.inputs.is_empty() {
            return Err(LedgerError::InputInGenesis(transfer_op.inputs[0]));
        }

        Ok(Self::from_utxos(transfer_op.utxos(), config, epoch_nonce))
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
                utxos: utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
            },
            epoch_state: EpochState {
                epoch: 0.into(),
                nonce,
                utxos,
                total_stake,
                lottery_0,
                lottery_1,
            },
            block_density,
            stake_inference,
            fee_window: [0u64; 120],
            average_execution_gas: 0u64,
            execution_base_fee: 0u64,
            storage_gas_ema: 0u64,
            storage_gas_price: 1u64,
            storage_gas_consumed_in_epoch: 0u64,
        }
    }
}

// This function upgrade the storage Gas price when a new epoch starts assuming
// the structure contains how much storage gas was consumed in the previous
// epoch according to <https://www.notion.so/nomos-tech/v1-1-Storage-Markets-Specification-326261aa09df804ab483f573f522baf5>
const fn update_storage_market(
    storage_gas_price: Gas,
    storage_gas_consumed_in_epoch: Gas,
    storage_gas_ema: Gas,
) -> (Gas, Gas) {
    let previous_price = storage_gas_price as u128;
    let total_storage_gas = storage_gas_consumed_in_epoch as u128;
    let previous_ema = storage_gas_ema as u128;

    let new_ema = ((total_storage_gas + previous_ema) / STORAGE_MARKET_EMA_DENOMINATOR) as Gas;
    let new_ema_unsigned = new_ema as u128;
    let comparator = STORAGE_MARKET_CLAMP_DENOMINATOR * total_storage_gas;
    let new_price = if comparator <= STORAGE_MARKET_CLAMP_DOWN_NUMERATOR * new_ema_unsigned {
        (previous_price * STORAGE_MARKET_CLAMP_DOWN_NUMERATOR / STORAGE_MARKET_CLAMP_DENOMINATOR)
            as Gas
    } else if comparator >= STORAGE_MARKET_CLAMP_UP_NUMERATOR * new_ema_unsigned {
        (previous_price * STORAGE_MARKET_CLAMP_UP_NUMERATOR / STORAGE_MARKET_CLAMP_DENOMINATOR)
            as Gas
    } else {
        (previous_price * total_storage_gas / new_ema_unsigned) as Gas
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
    use std::num::{NonZero, NonZeroU64};

    use lb_core::{
        crypto::{Digest as _, Hasher},
        mantle::{
            AuthenticatedMantleTx, MantleTx, Note, Op, OpProof::ZkSig, SignedMantleTx,
            Transaction as _, gas::MainnetGasConstants, ops::leader_claim::VoucherCm,
        },
        sdp::ServiceParameters,
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::Field as _;
    use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkKey};
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};

    use super::*;
    use crate::{
        Ledger,
        leader_proof::LeaderProof,
        mantle::sdp::{ServiceRewardsParameters, rewards},
    };

    type HeaderId = [u8; 32];

    #[must_use]
    pub fn utxo() -> Utxo {
        utxo_with_sk().1
    }

    #[must_use]
    pub fn utxo_with_sk() -> (ZkKey, Utxo) {
        let transfer_hash: Fr = BigUint::from(thread_rng().next_u64()).into();
        let zk_sk = ZkKey::from(BigUint::from(0u64));
        let utxo = Utxo {
            transfer_hash: transfer_hash.into(),
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
        pub fn set_execution_base_fee(self, new_execution_fee: Gas) -> Self {
            Self {
                execution_base_fee: new_execution_fee,
                ..self
            }
        }
    }

    fn update_ledger(
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
            .update_epoch_state::<HeaderId>(slot, ledger.config())
            .unwrap();
        let id = make_id(parent, slot, utxo);
        let proof = generate_proof(&ledger_state, &utxo, slot);
        let (_, state) = ledger.prepare_update::<_, MainnetGasConstants>(
            id,
            parent,
            slot,
            &proof,
            std::iter::empty::<&SignedMantleTx>(),
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
            lb_core::sdp::ServiceType::BlendNetwork,
            ServiceParameters {
                lock_period: 10,
                inactivity_period: 1,
                retention_period: 1,
                timestamp: 0,
                session_duration: 10,
            },
        );

        Config {
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                NonZero::new(1).unwrap(),
                NonNegativeRatio::new(1, 10.try_into().unwrap()),
                1f64.try_into().expect("1 > 0"),
            ),
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(service_params),
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
                min_stake: lb_core::sdp::MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
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
        LedgerState {
            utxos: utxos.clone(),
            nonce: Fr::ZERO,
            slot,
            next_epoch_state: EpochState {
                epoch: 1.into(),
                nonce: Fr::ZERO,
                utxos: utxos.clone(),
                total_stake,
                lottery_0,
                lottery_1,
            },
            epoch_state: EpochState {
                epoch: 0.into(),
                nonce: Fr::ZERO,
                utxos,
                total_stake,
                lottery_0,
                lottery_1,
            },
            stake_inference,
            fee_window: [0u64; 120],
            average_execution_gas: 0u64,
            block_density,
            execution_base_fee: 0u64,
            storage_gas_ema: 0u64,
            storage_gas_price: 1u64,
            storage_gas_consumed_in_epoch: 0u64,
        }
    }

    fn full_ledger_state(cryptarchia_ledger: LedgerState, config: &Config) -> crate::LedgerState {
        let mantle_ledger =
            crate::mantle::LedgerState::new(config, cryptarchia_ledger.epoch_state());
        crate::LedgerState {
            block_number: 0,
            cryptarchia_ledger,
            mantle_ledger,
        }
    }

    fn ledger(utxos: &[Utxo], config: Config) -> (Ledger<HeaderId>, HeaderId) {
        let genesis_state = genesis_state(utxos);
        (
            Ledger::new([0; 32], full_ledger_state(genesis_state, &config), config),
            [0; 32],
        )
    }

    fn apply_and_add_utxo(
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
        let mut block_state = ledger.states[&id].clone().cryptarchia_ledger;
        block_state.utxos = block_state.utxos.insert(utxo_add.id(), utxo_add).0;
        ledger
            .states
            .insert(id, full_ledger_state(block_state, &ledger.config));
        id
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
        let utxos = std::iter::repeat_with(utxo).take(4).collect::<Vec<_>>();
        let utxo_4 = utxo();
        let utxo_5 = utxo();

        let config = config();
        assert_eq!(config.epoch_length(), 100);
        let (mut ledger, genesis) = ledger(&utxos, config);
        // block density slot range should be [0, 59]
        assert_eq!(
            ledger.states[&genesis]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(0.into()..=59.into())
        );

        let h_1 = update_ledger(&mut ledger, genesis, 10, utxos[0]).unwrap();
        assert_eq!(
            ledger.states[&h_1].cryptarchia_ledger.epoch_state.epoch,
            0.into()
        );

        let h_2 = update_ledger(&mut ledger, h_1, 60, utxos[1]).unwrap();

        let h_3 = apply_and_add_utxo(&mut ledger, h_2, 90, utxos[2], utxo_4);

        // Epoch jump: epoch 0 -> 2
        // Jump to the slot that is not the 1st slot of epoch 2
        let h_4 = update_ledger(&mut ledger, h_3, 222, utxos[3]).unwrap();
        // nonce for epoch 2 should be taken at the end of slot 160, but in our case the
        // last block is at slot 90 because of epoch jumps
        assert_eq!(
            ledger.states[&h_4].cryptarchia_ledger.epoch_state.nonce,
            ledger.states[&h_3].cryptarchia_ledger.nonce,
        );
        // stake distribution snapshot should be taken at the end of slot 90
        assert_eq!(
            ledger.states[&h_4].cryptarchia_ledger.epoch_state.utxos,
            ledger.states[&h_3].cryptarchia_ledger.utxos,
        );
        // block density slot range should be [200, 259]
        assert_eq!(
            ledger.states[&h_4]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(200.into()..=259.into())
        );

        // Epoch transition: 0 -> 1
        // nonce for epoch 1 should be taken at the end of slot 10,
        // ignoring updates (`h_2` and `h_3`) after slot 59.
        let h_5 = apply_and_add_utxo(&mut ledger, h_3, 100, utxos[3], utxo_5);
        assert_eq!(
            ledger.states[&h_5].cryptarchia_ledger.epoch_state.nonce,
            ledger.states[&h_1].cryptarchia_ledger.nonce,
        );
        // stake distribution snapshot should be the same as the one in genesis
        assert_eq!(
            ledger.states[&h_5].cryptarchia_ledger.epoch_state.utxos,
            ledger.states[&genesis].cryptarchia_ledger.utxos,
        );
        // block density slot range should be [100, 159]
        assert_eq!(
            ledger.states[&h_5]
                .cryptarchia_ledger
                .block_density
                .period_range(),
            &(100.into()..=159.into())
        );

        // Epoch transition: 1 -> 2
        let h_6 = update_ledger(&mut ledger, h_5, 200, utxos[3]).unwrap();
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
        update_ledger(&mut ledger, h_0_1, 2 * epoch_length, utxo_1).unwrap();
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
        assert_eq!(
            ledger.states[&h5].cryptarchia_ledger.epoch_state.epoch,
            1.into()
        );
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
        assert_eq!(
            ledger.states[&h7].cryptarchia_ledger.epoch_state.epoch,
            2.into()
        );
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

        let slot = Slot::genesis() + 10;
        let ledger_state2 = ledger_state
            .cryptarchia_ledger
            .update_epoch_state::<HeaderId>(slot, ledger_config)
            .expect("Ledger needs to move forward");

        let slot2 = Slot::genesis() + 1;
        let update_epoch_err = ledger_state2
            .update_epoch_state::<HeaderId>(slot2, ledger_config)
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
        let slot = Slot::genesis() + 1;
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
        let slot = Slot::genesis() + 1;
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
    ) -> (SignedMantleTx, TransferOp, ZkSignature) {
        let sks = inputs
            .iter()
            .map(|(sk, _)| (*sk).clone())
            .collect::<Vec<_>>();
        let inputs = inputs.iter().map(|(_, utxo)| utxo.id()).collect::<Vec<_>>();
        let transfer_op = TransferOp::new(inputs, outputs);
        let mantle_tx = MantleTx {
            ops: vec![Op::Transfer(transfer_op.clone())],
            execution_gas_price: 1,
            storage_gas_price: 1,
        };
        let transfer_sig = ZkKey::multi_sign(&sks, &mantle_tx.hash().into()).unwrap();
        (
            SignedMantleTx {
                ops_proofs: vec![ZkSig(transfer_sig.clone())],
                mantle_tx,
            },
            transfer_op,
            transfer_sig,
        )
    }

    #[test]
    fn test_tx_processing_valid_transaction() {
        let note_sk = ZkKey::from(BigUint::from(1u8));
        let output_note1_sk = ZkKey::from(BigUint::from(2u8));
        let output_note2_sk = ZkKey::from(BigUint::from(3u8));
        let input_note = Note::new(11000, note_sk.to_public_key());
        let input_utxo = Utxo {
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: input_note,
        };

        let output_note1 = Note::new(4000, output_note1_sk.to_public_key());
        let output_note2 = Note::new(3000, output_note2_sk.to_public_key());

        let locked_notes = LockedNotes::new();
        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_sig) =
            create_tx_with_transfer(&[(&note_sk, &input_utxo)], vec![output_note1, output_note2]);

        let _fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx);
        let (new_state, balance) = ledger_state
            .try_apply_transfer::<(), MainnetGasConstants>(
                &locked_notes,
                &transfer_op,
                &transfer_sig,
                tx.hash(),
            )
            .unwrap();

        assert_eq!(
            balance,
            i128::from(input_note.value - output_note1.value - output_note2.value)
        );

        // Verify input was consumed
        assert!(!new_state.utxos.contains(&input_utxo.id()));

        // Verify outputs were created
        let (_, transfer_op, _) =
            create_tx_with_transfer(&[(&note_sk, &input_utxo)], vec![output_note1, output_note2]);
        let output_utxo1 = transfer_op.utxo_by_index(0).unwrap();
        let output_utxo2 = transfer_op.utxo_by_index(1).unwrap();

        assert!(new_state.utxos.contains(&output_utxo1.id()));
        assert!(new_state.utxos.contains(&output_utxo2.id()));

        // The new outputs can be spent in future transactions
        let (tx, transfer_op, transfer_sig) = create_tx_with_transfer(
            &[
                (&output_note1_sk, &output_utxo1),
                (&output_note2_sk, &output_utxo2),
            ],
            vec![],
        );
        let locked_notes = LockedNotes::new();
        let _fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx);
        let (final_state, final_balance) = new_state
            .try_apply_transfer::<(), MainnetGasConstants>(
                &locked_notes,
                &transfer_op,
                &transfer_sig,
                tx.hash(),
            )
            .unwrap();
        assert_eq!(
            final_balance,
            i128::from(output_note1.value + output_note2.value)
        );
        assert!(!final_state.utxos.contains(&output_utxo1.id()));
        assert!(!final_state.utxos.contains(&output_utxo2.id()));
    }

    #[test]
    fn test_tx_processing_invalid_input() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_note = Note::new(1000, input_sk.to_public_key());
        let input_utxo = Utxo {
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: input_note,
        };

        let non_existent_utxo_1 = Utxo {
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 1,
            note: input_note,
        };

        let non_existent_utxo_2 = Utxo {
            transfer_hash: Fr::from(BigUint::from(2u8)).into(),
            output_index: 0,
            note: input_note,
        };

        let non_existent_utxo_3 = Utxo {
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: Note::new(999, Fr::from(BigUint::from(1u8)).into()),
        };

        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);

        let invalid_utxos = [
            non_existent_utxo_1,
            non_existent_utxo_2,
            non_existent_utxo_3,
        ];

        let locked_notes = LockedNotes::new();
        for non_existent_utxo in invalid_utxos {
            let (tx, transfer_op, transfer_sig) =
                create_tx_with_transfer(&[(&ZkKey::zero(), &non_existent_utxo)], vec![]);
            let result = ledger_state
                .clone()
                .try_apply_transfer::<(), MainnetGasConstants>(
                    &locked_notes,
                    &transfer_op,
                    &transfer_sig,
                    tx.hash(),
                );
            assert!(matches!(result, Err(LedgerError::InvalidNote(_))));
        }
    }

    #[test]
    fn test_tx_processing_insufficient_balance() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_note = Note::new(1, input_sk.to_public_key());
        let input_utxo = Utxo {
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: input_note,
        };

        let output_note = Note::new(1, Fr::from(BigUint::from(2u8)).into());

        let locked_notes = LockedNotes::new();
        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_sig) =
            create_tx_with_transfer(&[(&input_sk, &input_utxo)], vec![output_note, output_note]);

        let (_, balance) = ledger_state
            .clone()
            .try_apply_transfer::<(), MainnetGasConstants>(
                &locked_notes,
                &transfer_op,
                &transfer_sig,
                tx.hash(),
            )
            .unwrap();
        assert_eq!(balance, -1);

        let (tx, transfer_op, transfer_sig) =
            create_tx_with_transfer(&[(&input_sk, &input_utxo)], vec![output_note]);
        assert_eq!(
            ledger_state
                .try_apply_transfer::<(), MainnetGasConstants>(
                    &locked_notes,
                    &transfer_op,
                    &transfer_sig,
                    tx.hash()
                )
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
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: input_note,
        };

        let locked_notes = LockedNotes::new();
        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_sig) =
            create_tx_with_transfer(&[(&input_sk, &input_utxo)], vec![]);

        let _fees = AuthenticatedMantleTx::total_gas_cost::<MainnetGasConstants>(&tx);
        let result = ledger_state.try_apply_transfer::<(), MainnetGasConstants>(
            &locked_notes,
            &transfer_op,
            &transfer_sig,
            tx.hash(),
        );
        assert!(result.is_ok());

        let (new_state, balance) = result.unwrap();
        assert_eq!(balance, 10000);

        // Verify input was consumed
        assert!(!new_state.utxos.contains(&input_utxo.id()));
    }

    #[test]
    fn test_output_not_zero() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            transfer_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: Note::new(10000, input_sk.to_public_key()),
        };

        let locked_notes = LockedNotes::new();
        let ledger_state = LedgerState::from_utxos([input_utxo], &config(), Fr::ZERO);
        let (tx, transfer_op, transfer_sig) = create_tx_with_transfer(
            &[(&input_sk, &input_utxo)],
            vec![Note::new(0, Fr::from(BigUint::from(2u8)).into())],
        );

        let result = ledger_state.try_apply_transfer::<(), MainnetGasConstants>(
            &locked_notes,
            &transfer_op,
            &transfer_sig,
            tx.hash(),
        );
        assert!(matches!(result, Err(LedgerError::ZeroValueNote)));
    }

    #[test]
    fn test_epoch_state_for_slot_with_empty_epochs() {
        let utxo = utxo();
        let config = config();
        let epoch_length = config.epoch_length();
        let ledger_state = genesis_state(&[utxo]);

        // Genesis state is at epoch 0, with epoch_state for epoch 0 and
        // next_epoch_state for epoch 1
        assert_eq!(ledger_state.epoch_state.epoch, 0.into());
        assert_eq!(ledger_state.next_epoch_state.epoch, 1.into());
        let initial_total_stake = ledger_state.epoch_state.total_stake;

        // Query for epoch 0 (current epoch) - should return epoch_state
        let epoch_0_slot: Slot = (epoch_length - 1).into();
        let epoch_0_state = ledger_state
            .epoch_state_for_slot::<HeaderId>(epoch_0_slot, &config)
            .expect("Should return epoch state for current epoch");
        assert_eq!(epoch_0_state.epoch, 0.into());
        assert_eq!(epoch_0_state.total_stake, initial_total_stake);

        // Query for epoch 1
        // Since epoch 0 has no block, total stake should be reduced
        let epoch_1_slot: Slot = (epoch_length + 1).into();
        let epoch_1_state = ledger_state
            .epoch_state_for_slot::<HeaderId>(epoch_1_slot, &config)
            .expect("Should return epoch state for next epoch");
        assert_eq!(epoch_1_state.epoch, 1.into());
        // With 0 density and LEARNING_RATE=1, total stake drops to minimum (1)
        assert_eq!(
            epoch_1_state.total_stake, 1,
            "Total stake should drop to minimum for empty epochs"
        );

        // Query for epoch 3 (multiple skipped epochs) - stake stays at minimum
        let epoch_2_slot: Slot = (2 * epoch_length + 1).into();
        let epoch_2_state = ledger_state
            .epoch_state_for_slot::<HeaderId>(epoch_2_slot, &config)
            .expect("Should synthesize epoch state for skipped epoch");
        assert_eq!(epoch_2_state.epoch, 2.into());
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
        assert_eq!(config.epoch(slot), 0.into());
        let proof = generate_proof(&genesis_state, &utxo, slot);
        let ledger_state_1 = genesis_state
            .try_apply_header::<DummyProof, HeaderId>(slot, &proof, &config)
            .unwrap();

        // Now, apply a header from the 2nd slot of epoch 2
        let slot = Slot::from(config.epoch_length() * 2 + 1);
        assert_eq!(config.epoch(slot), 2.into());

        // First, synthesize epoch state for epoch 2
        let synthesized_ledger_state = ledger_state_1
            .clone()
            .update_epoch_state::<HeaderId>(slot, &config)
            .unwrap();
        assert_eq!(synthesized_ledger_state.slot, slot);

        // Build a proof with the synthesized epoch state
        let proof = generate_proof(&synthesized_ledger_state, &utxo, slot);

        // Apply it to `ledger_state_1`.
        // Must succeed if epoch state in `ledger_state_1` is jumped
        // correctly to epoch 2 as the same as `synthesized_ledger_state`.
        let ledger_state_2 = ledger_state_1
            .clone()
            .try_apply_header::<DummyProof, HeaderId>(slot, &proof, &config)
            .unwrap();
        assert_eq!(ledger_state_2.slot, slot);
        assert_ne!(ledger_state_2.nonce, ledger_state_1.nonce); // advanced
        assert_eq!(ledger_state_2.epoch_state.epoch, 2.into());
    }

    fn stake_inference_from_config(config: &Config) -> StakeInference {
        StakeInference::new(
            config.consensus_config.stake_inference_learning_rate(),
            config.consensus_config.slot_activation_coeff().as_f64(),
            config.total_stake_inference_period(),
        )
    }

    #[test]
    fn test_storage_market_update() {
        // empty epoch
        assert_eq!((437, 340), update_storage_market(500, 0, 681));

        // Some random values
        // 1) raw = 113 * 1.125 = 127.125 -> 127
        assert_eq!((127, 450), update_storage_market(113, 600, 300));

        // 2) raw = 113 * 0.875 = 98.875 -> 98
        assert_eq!((98, 500), update_storage_market(113, 200, 800));

        // 3) raw = 221 * 1.125 = 248.625 -> 248
        assert_eq!((248, 550), update_storage_market(221, 900, 200));

        // 4) raw = 221 * 0.875 = 193.375 -> 193
        assert_eq!((193, 500), update_storage_market(221, 100, 900));

        // 5) raw = 345 * 1.125 = 388.125 -> 388
        assert_eq!((388, 165), update_storage_market(345, 250, 80));

        // 6) raw = 345 * 0.875 = 301.875 -> 301
        assert_eq!((301, 400), update_storage_market(345, 50, 750));

        // 7) raw = 517 * 1.125 = 581.625 -> 581
        assert_eq!((581, 160), update_storage_market(517, 220, 100));

        // 8) raw = 517 * 0.875 = 452.375 -> 452
        assert_eq!((452, 485), update_storage_market(517, 120, 850));

        // 9) raw = 999 * 1.125 = 1123.875 -> 1123
        assert_eq!((1123, 650), update_storage_market(999, 1000, 300));

        // 10) raw = 999 * 0.875 = 874.125 -> 874
        assert_eq!((874, 650), update_storage_market(999, 300, 1000));
    }

    #[test]
    fn test_execution_market_update() {
        // Create a base ledger first
        let mut ledger = LedgerState::from_utxos([], &config(), Fr::ZERO);

        // Some random values to test
        let old_avg = 1_596_688;
        let old_price = 113;
        let gas_used = 1_596_618;
        // 1) G_avg = (1596618 + 9*1596688)/10 = 1596681
        // price = 113 * (1 + 1 / 12_773_440) = 113.00000884648146 -> 113
        ledger.execution_base_fee = old_price;
        ledger.average_execution_gas = old_avg;
        ledger = ledger.update_execution_market(gas_used);
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (113, 1_596_681)
        );

        let old_avg = 1_596_676;
        let old_price = 221;
        let gas_used = 1_596_706;
        // 2) G_avg = (1596706 + 9*1596676)/10 = 1596679
        // price = 221 * (1 - 1 / 12_773_440) = 220.99998269847435 -> 220
        ledger.execution_base_fee = old_price;
        ledger.average_execution_gas = old_avg;
        ledger = ledger.update_execution_market(gas_used);
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (220, 1_596_679)
        );

        let old_avg = 1_597_925;
        let old_price = 345;
        let gas_used = 1_597_815;
        // 3) G_avg = (1597815 + 9*1597925)/10 = 1597914
        // price = 345 * (1 + 1234 / 12_773_440) = 345.0333293145777 -> 345
        ledger.execution_base_fee = old_price;
        ledger.average_execution_gas = old_avg;
        ledger = ledger.update_execution_market(gas_used);
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (345, 1_597_914)
        );

        let old_avg = 1_592_354;
        let old_price = 517;
        let gas_used = 1_592_404;
        // 4) G_avg = (1592404 + 9*1592354)/10 = 1592359
        // price = 517 * (1 - 4321 / 12_773_440) = 516.8251092109878 -> 516
        ledger.execution_base_fee = old_price;
        ledger.average_execution_gas = old_avg;
        ledger = ledger.update_execution_market(gas_used);
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (516, 1_592_359)
        );

        let old_avg = 1_604_466;
        let old_price = 999;
        let gas_used = 1_604_376;
        // 5) G_avg = (1604376 + 9*1604466)/10 = 1604457
        // price = 999 * (1 + 7777 / 12_773_440) = 999.6082326295813 -> 999
        ledger.execution_base_fee = old_price;
        ledger.average_execution_gas = old_avg;
        ledger = ledger.update_execution_market(gas_used);
        assert_eq!(
            (ledger.execution_base_fee, ledger.average_execution_gas),
            (999, 1_604_457)
        );
    }
}
