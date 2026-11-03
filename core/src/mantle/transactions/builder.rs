use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};

use lb_groth16::{AdditiveGroup as _, Fr, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_utils::bounded::{BoundedError, UpperBoundedVec};
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    mantle::{
        GasProfile, Note, Op, Value,
        gas::{GasCost, GasOverflow, TxGasCalculator as _},
        notes::{InputNullifiers, NoteNullifier, OutputCommitments},
        ops::transfer::TransferOp,
        transactions::{
            Ops,
            tx_list::ops::{OpsContext, mantle_spec},
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

#[derive(Debug, Error)]
pub enum TxBuilderError {
    #[error("Invalid operation bounds in transaction: {source}")]
    InvalidOpsBounds { source: BoundedError },
    #[error("Invalid ledger input bounds in transfer: {source}")]
    InvalidInputsBounds { source: BoundedError },
    #[error("Invalid ledger output bounds in transfer: {source}")]
    InvalidOutputsBounds { source: BoundedError },
    #[error("Gas computation overflow: {0}")]
    GasOverflow(#[from] GasOverflow),
    #[error("Funded transaction has negative net balance: {net_balance}")]
    NegativeNetBalance { net_balance: i128 },
    #[error("The funding transfer creates notes but spends none")]
    FundingWithoutInputs,
}

#[derive(Debug, Clone, Copy)]
enum BoundedTag {
    Ops,
    Inputs,
    Outputs,
}

impl From<(BoundedError, BoundedTag)> for TxBuilderError {
    fn from((err, tag): (BoundedError, BoundedTag)) -> Self {
        match tag {
            BoundedTag::Ops => Self::InvalidOpsBounds { source: err },
            BoundedTag::Inputs => Self::InvalidInputsBounds { source: err },
            BoundedTag::Outputs => Self::InvalidOutputsBounds { source: err },
        }
    }
}

// The notes a ZkTransfer spends and creates, in the order of the Operation's
// nullifiers and commitments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZkTransferOpening {
    pub inputs: Vec<Note>,
    pub outputs: Vec<Note>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingNote {
    pub note: Note,
    pub nullifier: NoteNullifier,
}

pub type FundingInputs = UpperBoundedVec<FundingNote, MAX_INPUTS>;
pub type FundingOutputs = UpperBoundedVec<Note, MAX_OUTPUTS>;

/// Builds an [`Ops`] incrementally.
///
/// The builder is intentionally free of any [`OpsContext`]: gas prices are
/// tip-dependent, so the context is supplied as a parameter to the fee-aware
/// methods ([`Self::minimum_gas_cost`], [`Self::funding_delta`],
/// [`Self::return_change`]) at the moment they run. This keeps the builder
/// serializable, so a partially built tx can be handed to a wallet (e.g. over
/// HTTP) to be funded against a freshly fetched context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MantleTxBuilder {
    #[serde(rename = "mantle_tx", with = "mantle_spec")] // Shape as per spec
    ops: Ops,
    funding_inputs: FundingInputs,
    funding_outputs: FundingOutputs,
    // The ledger set root the funding notes are proven against
    #[serde(with = "serde_fr")]
    funding_root: Fr,
    // Maps a ZkTransfer opening to its Op by the Op Index
    openings: BTreeMap<usize, ZkTransferOpening>,
    // Maps a Proof to its Op by the Op Index
    channel_multi_sig_proofs: HashMap<usize, ChannelMultiSigProof>,
}

impl Default for MantleTxBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// TODO: refactor to support more than 32 inputs (more than a single transfer)
impl MantleTxBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ops: Ops::empty(),
            funding_inputs: FundingInputs::empty(),
            funding_outputs: FundingOutputs::empty(),
            funding_root: Fr::ZERO,
            openings: BTreeMap::new(),
            channel_multi_sig_proofs: HashMap::new(),
        }
    }

    pub fn push_op(self, op: Op) -> Result<Self, TxBuilderError> {
        self.extend_ops([op])
    }

    pub fn push_op_with_opening(
        self,
        op: Op,
        opening: ZkTransferOpening,
    ) -> Result<Self, TxBuilderError> {
        let op_index = self.ops.len();
        let mut builder = self.push_op(op)?;
        builder.openings.insert(op_index, opening);
        Ok(builder)
    }

    // TODO: Change this to a `Result` if trying to push too many ops in the genesis
    // block.
    pub fn extend_ops(mut self, ops: impl IntoIterator<Item = Op>) -> Result<Self, TxBuilderError> {
        for op in ops {
            self.ops
                .0
                .try_push(op)
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Ops)))?;
        }
        Ok(self)
    }

    pub fn extend_funding_inputs(
        mut self,
        funding_root: Fr,
        notes: impl IntoIterator<Item = FundingNote>,
    ) -> Result<Self, TxBuilderError> {
        self.funding_root = funding_root;
        for note in notes {
            self.funding_inputs
                .try_push(note)
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Inputs)))?;
        }
        Ok(self)
    }

    pub fn add_ledger_output(self, note: Note) -> Result<Self, TxBuilderError> {
        self.extend_ledger_outputs([note])
    }

    pub fn extend_ledger_outputs(
        mut self,
        notes: impl IntoIterator<Item = Note>,
    ) -> Result<Self, TxBuilderError> {
        for note in notes {
            self.funding_outputs
                .try_push(note)
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Outputs)))?;
        }
        Ok(self)
    }

    /// Return a positive change output while reserving the requested
    /// percentage of the final transaction's mandatory fee.
    pub fn return_change<G: GasProfile>(
        self,
        context: &OpsContext,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        priority_fee_percent: u64,
    ) -> Result<Option<Self>, TxBuilderError> {
        // Calculate the mandatory fee with a dummy change note so the reserve
        // is based on the final transaction shape.
        let candidate = self.with_dummy_change_note()?;
        let available_change =
            candidate.funding_delta_with_priority_fee::<G>(context, priority_fee_percent)?;

        match available_change.cmp(&1) {
            Ordering::Less => {
                // The change output would be zero-valued or unaffordable, so
                // the caller must try a larger set of funding inputs.
                Ok(None)
            }
            Ordering::Equal | Ordering::Greater => {
                let change = u64::try_from(available_change).expect("change must fit in u64");
                let tx_with_change =
                    self.add_ledger_output(Note::new(change, change_nonce, change_pk))?;

                Ok(Some(tx_with_change))
            }
        }
    }

    pub fn with_dummy_change_note(&self) -> Result<Self, TxBuilderError> {
        self.clone()
            .add_ledger_output(Note::new(0, Fr::ZERO, ZkPublicKey::zero()))
    }

    #[must_use]
    pub fn net_balance(&self) -> i128 {
        let in_sum: i128 = self
            .funding_inputs
            .iter()
            .map(|funding| i128::from(funding.note.value))
            .sum();

        let out_sum: i128 = self
            .funding_outputs
            .iter()
            .map(|n| i128::from(n.value))
            .sum();

        in_sum - out_sum
    }

    /// The fee this transaction actually pays: its net balance (inputs minus
    /// outputs). This can exceed the raw gas cost once priority tips are
    /// introduced. Only meaningful once the builder is funded/balanced.
    ///
    /// Only accounts for builder-managed value (ledger inputs and the pending
    /// transfer). `Transfer` ops pushed directly carry note ids whose values
    /// are unknown to the builder and are not priced.
    pub fn tx_fee(&self) -> Result<GasCost, TxBuilderError> {
        let net_balance = self.net_balance();
        u64::try_from(net_balance)
            .map(GasCost::new)
            .map_err(|_| TxBuilderError::NegativeNetBalance { net_balance })
    }

    /// Predicts the minimum gas cost of the transaction once signed.
    /// See [`TxGasCalculator::total_gas_cost`] to understand why this is
    /// only a minimum, not an exact cost.
    pub fn minimum_gas_cost<G: GasProfile>(
        &self,
        context: &OpsContext,
    ) -> Result<GasCost, TxBuilderError> {
        // The excess value has a fixed width, so the funding Transfer costs the
        // same before the builder balances.
        let build = self.ops_with_funding(0)?;
        Ok(build.by_ref().total_gas_cost::<G>(&context.gas_context)?)
    }

    pub fn funding_delta<G: GasProfile>(
        &self,
        context: &OpsContext,
    ) -> Result<i128, TxBuilderError> {
        Ok(self.net_balance() - i128::from(self.minimum_gas_cost::<G>(context)?.into_inner()))
    }

    /// Returns the balance remaining after the mandatory fee and the
    /// percentage-based priority fee reserve.
    pub fn funding_delta_with_priority_fee<G: GasProfile>(
        &self,
        context: &OpsContext,
        priority_fee_percent: u64,
    ) -> Result<i128, TxBuilderError> {
        let mandatory_fee = self.minimum_gas_cost::<G>(context)?.into_inner();
        let required_fee = Self::required_fee(mandatory_fee, priority_fee_percent)?;
        Ok(self.net_balance() - i128::from(required_fee))
    }

    fn required_fee(
        mandatory_fee: Value,
        priority_fee_percent: u64,
    ) -> Result<Value, TxBuilderError> {
        let priority_fee_amount = Self::priority_fee_amount(mandatory_fee, priority_fee_percent)?;
        mandatory_fee
            .checked_add(priority_fee_amount)
            .ok_or_else(|| GasOverflow.into())
    }

    /// Calculates the priority fee amount for a mandatory fee using integer
    /// arithmetic, rounding up to the next whole fee unit.
    fn priority_fee_amount(
        mandatory_fee: Value,
        priority_fee_percent: u64,
    ) -> Result<Value, TxBuilderError> {
        let numerator = u128::from(mandatory_fee)
            .checked_mul(u128::from(priority_fee_percent))
            .and_then(|value| value.checked_add(99))
            .ok_or(GasOverflow)?;
        Value::try_from(numerator / 100).map_err(|_| GasOverflow.into())
    }

    /// Returns all nullifiers spent by this transaction, plus the funding
    /// inputs that will be appended as a transfer during build.
    pub fn nullifiers_consumed(&self) -> impl Iterator<Item = NoteNullifier> {
        self.ops
            .iter()
            .flat_map(|op| {
                let inputs: &[NoteNullifier] = match op {
                    Op::Transfer(transfer) => &transfer.inputs,
                    Op::ChannelDeposit(deposit) => &deposit.inputs,
                    Op::SDPDeclare(declare) => &declare.inputs,
                    Op::SDPWithdraw(withdraw) => {
                        core::slice::from_ref(&withdraw.service_note_nullifier)
                    }
                    _ => &[],
                };
                inputs.iter().copied()
            })
            .chain(self.funding_inputs.iter().map(|funding| funding.nullifier))
    }

    #[must_use]
    pub fn funding_inputs(&self) -> &[FundingNote] {
        &self.funding_inputs
    }

    // The ZkTransfer openings by Op Index, with the funding Transfer's at the
    // index `build` appends it.
    #[must_use]
    pub fn openings(&self) -> BTreeMap<usize, ZkTransferOpening> {
        let mut openings = self.openings.clone();
        if self.funds() {
            openings.insert(
                self.ops.len(),
                ZkTransferOpening {
                    inputs: self
                        .funding_inputs
                        .iter()
                        .map(|funding| funding.note)
                        .collect(),
                    outputs: self.funding_outputs.to_vec(),
                },
            );
        }
        openings
    }

    #[must_use]
    pub const fn channel_multi_sig_proofs(&self) -> &HashMap<usize, ChannelMultiSigProof> {
        &self.channel_multi_sig_proofs
    }

    // TODO: Change this to a `Result` if genesis tx already contains max number of
    // ops.
    pub fn build(self) -> Result<Ops, TxBuilderError> {
        if self.funding_inputs.is_empty() && !self.funding_outputs.is_empty() {
            return Err(TxBuilderError::FundingWithoutInputs);
        }
        let net_balance = self.net_balance();
        let excess_value = Value::try_from(net_balance)
            .map_err(|_| TxBuilderError::NegativeNetBalance { net_balance })?;
        self.ops_with_funding(excess_value)
    }

    const fn funds(&self) -> bool {
        !self.funding_inputs.is_empty() || !self.funding_outputs.is_empty()
    }

    // The Operations followed by the funding Transfer, when the builder funds
    // the transaction.
    fn ops_with_funding(&self, excess_value: Value) -> Result<Ops, TxBuilderError> {
        let mut ops = self.ops.clone();
        if self.funds() {
            let inputs = InputNullifiers::try_from_iter(
                self.funding_inputs.iter().map(|funding| funding.nullifier),
            )
            .map_err(|err| TxBuilderError::from((err, BoundedTag::Inputs)))?;
            let outputs =
                OutputCommitments::try_from_iter(self.funding_outputs.iter().map(Note::commitment))
                    .map_err(|err| TxBuilderError::from((err, BoundedTag::Outputs)))?;
            ops.0
                .try_push(Op::Transfer(TransferOp::new(
                    inputs,
                    outputs,
                    self.funding_root,
                    excess_value,
                )))
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Ops)))?;
        }
        Ok(ops)
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Field as _;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;
    use crate::mantle::{
        gas::MainnetGasProfile,
        ops::channel::{
            ChannelId,
            deposit::DepositOp,
            inscribe::{ChannelSteps, InscriptionOp},
            withdraw::ChannelWithdrawOp,
        },
        transactions::{GasPrices, tx_list::ops::OpsGasContext},
    };

    fn funding_note(value: Value, nonce: u64, nullifier: u64) -> FundingNote {
        FundingNote {
            note: Note::new(value, Fr::from(nonce), ZkPublicKey::new(Fr::from(9u64))),
            nullifier: NoteNullifier(Fr::from(nullifier)),
        }
    }

    #[test]
    fn serde_round_trip() {
        // The builder crosses the HTTP boundary (e.g. the wallet fund
        // endpoint), so a serialized builder must deserialize back to the
        // same transaction.
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp {
                channel_id: [0; 32].into(),
                inscription: b"hello".into(),
                parent: [1; 32].into(),
                signer: Ed25519Key::from_bytes(&[0; 32])
                    .public_key()
                    .into_unverified(),
                steps: ChannelSteps::default(),
            }))
            .unwrap()
            .extend_funding_inputs(Fr::from(5u64), [funding_note(50, 1, 3)])
            .unwrap()
            .add_ledger_output(Note::new(40, Fr::from(2u64), ZkPublicKey::zero()))
            .unwrap();

        let json = serde_json::to_string(&builder).expect("builder should serialize");
        let restored: MantleTxBuilder =
            serde_json::from_str(&json).expect("builder should deserialize");

        assert_eq!(restored.net_balance(), builder.net_balance());
        assert_eq!(restored.funding_inputs(), builder.funding_inputs());
        assert_eq!(restored.openings(), builder.openings());
        assert_eq!(restored.build().unwrap(), builder.build().unwrap());
    }

    #[test]
    fn inscription_op() {
        // Build an operation
        let op = InscriptionOp {
            channel_id: [0; 32].into(),
            inscription: b"hello".into(),
            parent: [1; 32].into(),
            signer: Ed25519Key::from_bytes(&[0; 32])
                .public_key()
                .into_unverified(),
            steps: ChannelSteps::default(),
        };

        // Init a tx builder
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0)),
        };
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap(),
            0
        );
    }

    #[test]
    fn deposit_op() {
        // Build an operation
        let op = DepositOp {
            channel_id: [0; 32].into(),
            inputs: [NoteNullifier(Fr::ZERO)].into(),
            commitment_merkle_root: Fr::ZERO,
            outputs: OutputCommitments::default(),
            excess_value: 0,
            metadata: b"Mint 1 to Alice in Zone".into(),
        };

        // Init a tx builder
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0)),
        };
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelDeposit(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap(),
            0
        );
    }

    #[test]
    fn withdraw_op() {
        // Build an operation
        let op = ChannelWithdrawOp {
            channel_id: [0; 32].into(),
            inputs: [NoteNullifier(Fr::ZERO)].into(),
            commitment_merkle_root: Fr::ZERO,
            outputs: OutputCommitments::default(),
        };

        // Init a tx builder
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0)),
        };
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelWithdraw(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap(),
            0
        );
    }

    #[test]
    fn all_ops() {
        // Init a tx builder for sending 30 to the recipient
        let channel_id = ChannelId::from([0; 32]);
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0)),
        };
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp {
                channel_id,
                inscription: b"hello".into(),
                parent: [1; 32].into(),
                signer: Ed25519Key::from_bytes(&[0; 32])
                    .public_key()
                    .into_unverified(),
                steps: ChannelSteps::default(),
            }))
            .unwrap()
            .push_op(Op::ChannelDeposit(DepositOp {
                channel_id,
                inputs: [NoteNullifier(Fr::ZERO)].into(),
                commitment_merkle_root: Fr::ZERO,
                outputs: OutputCommitments::default(),
                excess_value: 0,
                metadata: b"Mint 10 to Alice in Zone".into(),
            }))
            .unwrap()
            .push_op(Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id,
                inputs: [NoteNullifier(Fr::ZERO)].into(),
                commitment_merkle_root: Fr::ZERO,
                outputs: OutputCommitments::default(),
            }))
            .unwrap()
            .add_ledger_output(Note::new(40, Fr::from(2u64), ZkPublicKey::zero()))
            .unwrap();

        // Check the balance before funding tx
        assert_eq!(builder.net_balance(), -40);
        assert_eq!(
            builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap(),
            -40 // zero gas price for now
        );

        // Fund tx
        let builder = builder
            .extend_funding_inputs(Fr::ZERO, [funding_note(40, 1, 1)])
            .unwrap();

        // Check the tx is balanced
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap(),
            0 // zero gas price for now
        );
    }

    #[test]
    fn build_appends_the_funding_transfer_with_its_opening() {
        let input = funding_note(1_000, 1, 3);
        let output = Note::new(400, Fr::from(2u64), ZkPublicKey::new(Fr::from(9u64)));
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp::sample()))
            .unwrap()
            .extend_funding_inputs(Fr::from(5u64), [input])
            .unwrap()
            .add_ledger_output(output)
            .unwrap();
        assert_eq!(builder.net_balance(), 600);
        assert_eq!(
            builder.nullifiers_consumed().collect::<Vec<_>>(),
            vec![NoteNullifier(Fr::from(3u64))]
        );
        let openings = builder.openings();

        let ops: Vec<Op> = builder.build().unwrap().iter().cloned().collect();
        assert_eq!(ops.len(), 2);
        assert_eq!(
            ops[1],
            Op::Transfer(TransferOp::new(
                [NoteNullifier(Fr::from(3u64))].into(),
                [output.commitment()].into(),
                Fr::from(5u64),
                600
            ))
        );
        assert_eq!(
            openings,
            BTreeMap::from([(
                1,
                ZkTransferOpening {
                    inputs: vec![input.note],
                    outputs: vec![output]
                }
            )])
        );
    }

    #[test]
    fn funding_spends_at_most_four_notes() {
        let builder = MantleTxBuilder::new()
            .extend_funding_inputs(Fr::ZERO, (1..=4).map(|i| funding_note(10, i, i)))
            .unwrap();
        assert!(
            builder
                .extend_funding_inputs(Fr::ZERO, [funding_note(10, 5, 5)])
                .is_err()
        );
    }

    #[test]
    fn funding_outputs_without_inputs_are_rejected() {
        let builder = MantleTxBuilder::new()
            .add_ledger_output(Note::new(1, Fr::ONE, ZkPublicKey::zero()))
            .unwrap();
        assert!(matches!(
            builder.build(),
            Err(TxBuilderError::FundingWithoutInputs)
        ));
    }

    #[test]
    fn priority_fee_percentage_rounds_up_without_a_cap() {
        assert_eq!(MantleTxBuilder::priority_fee_amount(0, 12).unwrap(), 0);
        assert_eq!(MantleTxBuilder::priority_fee_amount(1, 12).unwrap(), 1);
        assert_eq!(MantleTxBuilder::priority_fee_amount(8, 12).unwrap(), 1);
        assert_eq!(MantleTxBuilder::priority_fee_amount(9, 12).unwrap(), 2);
        assert_eq!(MantleTxBuilder::priority_fee_amount(100, 101).unwrap(), 101);
    }

    #[test]
    fn priority_fee_percentage_handles_u64_boundaries() {
        assert_eq!(
            MantleTxBuilder::priority_fee_amount(u64::MAX, 100).unwrap(),
            u64::MAX
        );
        assert!(matches!(
            MantleTxBuilder::priority_fee_amount(u64::MAX, 101),
            Err(TxBuilderError::GasOverflow(_))
        ));
        assert!(matches!(
            MantleTxBuilder::required_fee(u64::MAX, 100),
            Err(TxBuilderError::GasOverflow(_))
        ));
    }
}
