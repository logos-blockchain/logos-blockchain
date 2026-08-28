use std::{cmp::Ordering, collections::HashMap};

use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_utils::bounded::BoundedError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    mantle::{
        GasProfile, Note, NoteId, Op, Utxo, Value,
        gas::{GasCost, GasOverflow},
        ledger::{BoundedUtxos, Inputs, Outputs},
        ops::{channel::ChannelId, transfer::TransferOp},
        transactions::mantle_tx::{MantleTx as _, MantleTxContext, RawMantleTx},
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
    #[error("Missing transfer threshold for channel {channel_id:?}")]
    MissingTransferThreshold { channel_id: ChannelId },
    #[error("Funded transaction has negative net balance: {net_balance}")]
    NegativeNetBalance { net_balance: i128 },
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

/// Builds a [`RawMantleTx`] incrementally.
///
/// The builder is intentionally free of any [`MantleTxContext`]: gas prices are
/// tip-dependent, so the context is supplied as a parameter to the fee-aware
/// methods ([`Self::minimum_gas_cost`], [`Self::funding_delta`],
/// [`Self::return_change`]) at the moment they run. This keeps the builder
/// serializable, so a partially built tx can be handed to a wallet (e.g. over
/// HTTP) to be funded against a freshly fetched context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MantleTxBuilder {
    mantle_tx: RawMantleTx,
    ledger_inputs: BoundedUtxos,
    pending_transfer: TransferOp,
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
            mantle_tx: RawMantleTx([].into()),
            ledger_inputs: BoundedUtxos::default(),
            pending_transfer: TransferOp::new(Inputs::empty(), Outputs::empty()),
            channel_multi_sig_proofs: HashMap::new(),
        }
    }

    pub fn push_op(self, op: Op) -> Result<Self, TxBuilderError> {
        self.extend_ops([op])
    }

    // TODO: Change this to a `Result` if trying to push too many ops in the genesis
    // block.
    pub fn extend_ops(mut self, ops: impl IntoIterator<Item = Op>) -> Result<Self, TxBuilderError> {
        for op in ops {
            self.mantle_tx
                .0
                .try_push(op)
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Ops)))?;
        }
        Ok(self)
    }

    pub fn add_ledger_input(self, utxo: Utxo) -> Result<Self, TxBuilderError> {
        self.extend_ledger_inputs([utxo])
    }

    pub fn extend_ledger_inputs(
        mut self,
        utxos: impl IntoIterator<Item = Utxo>,
    ) -> Result<Self, TxBuilderError> {
        for utxo in utxos {
            assert_eq!(self.pending_transfer.inputs.len(), self.ledger_inputs.len());
            self.pending_transfer
                .inputs
                .as_mut()
                .try_push(utxo.id())
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Inputs)))?;
            self.ledger_inputs
                .try_push(utxo)
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
            self.pending_transfer
                .outputs
                .try_push(note)
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Outputs)))?;
        }
        Ok(self)
    }

    /// Return a positive change output while reserving the requested
    /// percentage of the final transaction's mandatory fee.
    pub fn return_change<G: GasProfile>(
        self,
        context: &MantleTxContext,
        change_pk: ZkPublicKey,
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
                let tx_with_change = self.add_ledger_output(Note {
                    value: change,
                    pk: change_pk,
                })?;

                Ok(Some(tx_with_change))
            }
        }
    }

    pub fn with_dummy_change_note(&self) -> Result<Self, TxBuilderError> {
        self.clone().add_ledger_output(Note {
            value: 0,
            pk: ZkPublicKey::zero(),
        })
    }

    #[must_use]
    pub fn net_balance(&self) -> i128 {
        let in_sum: i128 = self
            .ledger_inputs
            .iter()
            .map(|utxo| i128::from(utxo.note.value))
            .sum();

        let out_sum: i128 = self
            .pending_transfer
            .outputs
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
    /// See [`RawMantleTx::minimum_total_gas_cost`] to understand why this is
    /// only a minimum, not an exact cost.
    pub fn minimum_gas_cost<G: GasProfile>(
        &self,
        context: &MantleTxContext,
    ) -> Result<GasCost, TxBuilderError> {
        for op in self.mantle_tx.ops() {
            let channel_id = match op {
                Op::ChannelWithdraw(operation) => Some(operation.channel_id),
                Op::ChannelTransfer(operation) => Some(operation.channel_id),
                _ => None,
            };
            if let Some(channel_id) = channel_id
                && context
                    .gas_context
                    .transfer_threshold(&channel_id)
                    .is_none()
            {
                return Err(TxBuilderError::MissingTransferThreshold { channel_id });
            }
        }

        let build = self.clone().build()?;
        Ok(build.minimum_total_gas_cost::<G>(&context.gas_context)?)
    }

    pub fn funding_delta<G: GasProfile>(
        &self,
        context: &MantleTxContext,
    ) -> Result<i128, TxBuilderError> {
        Ok(self.net_balance() - i128::from(self.minimum_gas_cost::<G>(context)?.into_inner()))
    }

    /// Returns the balance remaining after the mandatory fee and the
    /// percentage-based priority fee reserve.
    pub fn funding_delta_with_priority_fee<G: GasProfile>(
        &self,
        context: &MantleTxContext,
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

    /// Returns all note IDs already consumed or used in a service by this
    /// transaction, plus the funding inputs that will be appended as a
    /// transfer during build.
    pub fn notes_consumed_or_used_in_service(&self) -> impl Iterator<Item = NoteId> {
        self.mantle_tx
            .ops()
            .iter()
            .flat_map(|op| {
                let inputs: &[NoteId] = match op {
                    Op::Transfer(transfer) => transfer.inputs.as_ref(),
                    Op::ChannelDeposit(deposit) => deposit.inputs.as_ref(),
                    _ => &[],
                };
                let locked = match op {
                    Op::SDPDeclare(declare) => Some(declare.service_note_id),
                    Op::SDPWithdraw(withdraw) => Some(withdraw.service_note_id),
                    _ => None,
                };
                inputs.iter().copied().chain(locked)
            })
            .chain(self.ledger_inputs().iter().map(Utxo::id))
    }

    #[must_use]
    pub fn ledger_inputs(&self) -> &[Utxo] {
        &self.ledger_inputs
    }

    #[must_use]
    pub const fn channel_multi_sig_proofs(&self) -> &HashMap<usize, ChannelMultiSigProof> {
        &self.channel_multi_sig_proofs
    }

    // TODO: Change this to a `Result` if genesis tx already contains max number of
    // ops.
    pub fn build(mut self) -> Result<RawMantleTx, TxBuilderError> {
        if !self.pending_transfer.is_empty() {
            self.mantle_tx
                .0
                .try_push(Op::Transfer(self.pending_transfer))
                .map_err(|err| TxBuilderError::from((err, BoundedTag::Ops)))?;
        }
        Ok(self.mantle_tx)
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;
    use crate::{
        mantle::{
            gas::MainnetGasProfile,
            ops::{
                channel::{
                    deposit::{DepositOp, Metadata},
                    inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
                leader_claim::LeaderClaimOp,
                sdp::{SDPDeclareOp, SDPWithdrawOp},
            },
            transactions::{GasPrices, MantleTxGasContext},
        },
        sdp::{DeclarationId, Locator, ProviderId, ServiceType},
    };

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
                signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
            }))
            .unwrap()
            .add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(50, ZkPublicKey::zero())))
            .unwrap()
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()))
            .unwrap();

        let json = serde_json::to_string(&builder).expect("builder should serialize");
        let restored: MantleTxBuilder =
            serde_json::from_str(&json).expect("builder should deserialize");

        assert_eq!(restored.net_balance(), builder.net_balance());
        assert_eq!(restored.ledger_inputs(), builder.ledger_inputs());
        assert_eq!(restored.build().unwrap(), builder.build().unwrap());
    }

    #[test]
    fn inscription_op() {
        // Build an operation
        let op = InscriptionOp {
            channel_id: [0; 32].into(),
            inscription: b"hello".into(),
            parent: [1; 32].into(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::new(),
                HashMap::new(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 30,
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
            inputs: Inputs::new([NoteId(Fr::ZERO)]),
            metadata: b"Mint 1 to Alice in Zone".into(),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::new(),
                HashMap::new(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 30,
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
            inputs: Inputs::new([NoteId(Fr::ZERO)]),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                [(op.channel_id, 1)].into(),
                HashMap::new(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 30,
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
    fn withdraw_gas_cost_without_threshold_returns_error() {
        let channel_id = ChannelId::from([9; 32]);

        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([NoteId(Fr::ZERO)]),
        };

        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelWithdraw(withdraw_op))
            .unwrap();

        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::new(),
                HashMap::new(),
                GasPrices::new(1, 0),
            ),
            leader_reward_amount: 0,
        };

        let result = builder.minimum_gas_cost::<MainnetGasProfile>(&context);

        assert!(matches!(
            result,
            Err(TxBuilderError::MissingTransferThreshold {
                channel_id: missing_channel_id
            }) if missing_channel_id == channel_id
        ));
    }

    #[test]
    fn leader_claim_op() {
        // Build an operation
        let op = LeaderClaimOp {
            rewards_root: Fr::ZERO.into(),
            voucher_nullifier: Fr::ZERO.into(),
            pk: ZkPublicKey::zero(),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::new(),
                HashMap::new(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new().push_op(Op::LeaderClaim(op)).unwrap();

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
    fn transfer_op() {
        // Init a tx builder for sending 30 to the recipient
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::new(),
                HashMap::new(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new()
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()))
            .unwrap()
            .add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(50, ZkPublicKey::zero())));
        let builder = builder.unwrap();

        // Check that the balance is 10 (= 50 - 40)
        assert_eq!(builder.net_balance(), 10);
        assert_eq!(
            builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap(),
            10 // zero gas price for now
        );

        // Add change note
        let builder = builder
            .return_change::<MainnetGasProfile>(&context, ZkPublicKey::zero(), 0)
            .unwrap()
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
    fn all_ops() {
        // Init a tx builder for sending 30 to the recipient
        let channel_id = ChannelId::from([0; 32]);
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                [(channel_id, 1)].into(),
                HashMap::new(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp {
                channel_id,
                inscription: b"hello".into(),
                parent: [1; 32].into(),
                signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
            }))
            .unwrap()
            .push_op(Op::ChannelDeposit(DepositOp {
                channel_id,
                inputs: Inputs::new([NoteId(Fr::ZERO)]),
                metadata: b"Mint 10 to Alice in Zone".into(),
            }))
            .unwrap()
            .push_op(Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id,
                inputs: Inputs::new([NoteId(Fr::ZERO)]),
            }))
            .unwrap()
            .push_op(Op::LeaderClaim(LeaderClaimOp {
                rewards_root: Fr::ZERO.into(),
                voucher_nullifier: Fr::ZERO.into(),
                pk: ZkPublicKey::zero(),
            }))
            .unwrap()
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()))
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
            .add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(40, ZkPublicKey::zero())))
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
    fn notes_consumed_or_used_in_service() {
        let deposit_input = NoteId(Fr::from(1u64));
        let declare_service_note = NoteId(Fr::from(2u64));
        let withdraw_service_note = NoteId(Fr::from(3u64));
        let transfer_input = Utxo::new([0u8; 32], 0, Note::new(50, ZkPublicKey::zero()));

        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelDeposit(DepositOp {
                channel_id: [0; 32].into(),
                inputs: Inputs::new([deposit_input]),
                metadata: Metadata::empty(),
            }))
            .unwrap()
            .push_op(Op::SDPDeclare(SDPDeclareOp {
                service_type: ServiceType::BlendNetwork,
                locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
                provider_id: ProviderId(Ed25519Key::from_bytes(&[0; 32]).public_key()),
                zk_id: ZkPublicKey::zero(),
                service_note_id: declare_service_note,
            }))
            .unwrap()
            .push_op(Op::SDPWithdraw(SDPWithdrawOp {
                declaration_id: DeclarationId([0; 32]),
                service_note_id: withdraw_service_note,
                nonce: 1,
            }))
            .unwrap()
            .add_ledger_input(transfer_input)
            .unwrap();

        let consumed_or_used: Vec<_> = builder.notes_consumed_or_used_in_service().collect();
        assert!(
            consumed_or_used.contains(&deposit_input),
            "should contain deposit input"
        );
        assert!(
            consumed_or_used.contains(&declare_service_note),
            "should contain declare service note"
        );
        assert!(
            consumed_or_used.contains(&withdraw_service_note),
            "should contain withdraw service note"
        );
        assert!(
            consumed_or_used.contains(&transfer_input.id()),
            "should contain transfer input"
        );
        assert_eq!(consumed_or_used.len(), 4);
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
