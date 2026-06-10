use std::{cmp::Ordering, collections::HashMap};

use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_utils::bounded_vec::BoundedError;
use thiserror::Error;

use super::{GasCalculator as _, GasConstants, MantleTx, Note, Op, Utxo};
use crate::{
    mantle::{
        NoteId,
        encoding::BoundedUtxos,
        gas::{GasCost, GasOverflow},
        ledger::{Inputs, Outputs},
        ops::{channel::withdraw::ChannelWithdrawOp, transfer::TransferOp},
        tx::{GasPrices, MantleTxContext},
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

#[derive(Debug, Error)]
pub enum TxBuilderError {
    #[error("Too many operations in transaction: attempted {actual}, max {max}")]
    TooManyOps { actual: usize, max: usize },
    #[error("Too many ledger inputs in transfer: attempted {actual}, max {max}")]
    TooManyInputs { actual: usize, max: usize },
    #[error("Too many ledger outputs in transfer: attempted {actual}, max {max}")]
    TooManyOutputs { actual: usize, max: usize },
    #[error("Gas computation overflow: {0}")]
    GasOverflow(#[from] GasOverflow),
}

#[derive(Debug, Clone, Copy)]
enum TooManyTag {
    Ops,
    Inputs,
    Outputs,
}

impl From<(BoundedError, TooManyTag)> for TxBuilderError {
    fn from((err, tag): (BoundedError, TooManyTag)) -> Self {
        let (actual, max) = match err {
            BoundedError::TooLong { actual, max } => (actual, max),
            BoundedError::EmptyInput => (0, 0),
        };

        match tag {
            TooManyTag::Ops => Self::TooManyOps { actual, max },
            TooManyTag::Inputs => Self::TooManyInputs { actual, max },
            TooManyTag::Outputs => Self::TooManyOutputs { actual, max },
        }
    }
}

#[derive(Debug, Clone)]
pub struct MantleTxBuilder {
    mantle_tx: MantleTx,
    ledger_inputs: BoundedUtxos,
    pending_transfer: TransferOp,
    // Maps a Proof to its Op by the Op Index
    channel_multi_sig_proofs: HashMap<usize, ChannelMultiSigProof>,
    context: MantleTxContext,
}

// TODO: refactor to support more than 32 inputs (more than a single transfer)
impl MantleTxBuilder {
    #[must_use]
    pub fn new(context: MantleTxContext) -> Self {
        Self {
            mantle_tx: MantleTx([].into()),
            ledger_inputs: BoundedUtxos::default(),
            pending_transfer: TransferOp::new(Inputs::empty(), Outputs::empty()),
            channel_multi_sig_proofs: HashMap::new(),
            context,
        }
    }

    #[must_use]
    pub fn get_gas_prices(&self) -> GasPrices {
        self.context.gas_context.get_gas_prices()
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
                .map_err(|err| TxBuilderError::from((err, TooManyTag::Ops)))?;
        }
        Ok(self)
    }

    pub fn push_channel_withdraw(
        self,
        op: ChannelWithdrawOp,
        proof: ChannelMultiSigProof,
    ) -> Result<Self, TxBuilderError> {
        let mut builder = self.push_op(Op::ChannelWithdraw(op))?;
        let index = builder.mantle_tx.ops().len() - 1;
        builder.channel_multi_sig_proofs.insert(index, proof);
        Ok(builder)
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
                .map_err(|err| TxBuilderError::from((err, TooManyTag::Inputs)))?;
            self.ledger_inputs
                .try_push(utxo)
                .map_err(|err| TxBuilderError::from((err, TooManyTag::Inputs)))?;
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
                .as_mut()
                .try_push(note)
                .map_err(|err| TxBuilderError::from((err, TooManyTag::Outputs)))?;
        }
        Ok(self)
    }

    pub fn return_change<G: GasConstants>(
        self,
        change_pk: ZkPublicKey,
    ) -> Result<Option<Self>, TxBuilderError> {
        // Calculate the funding delta with a dummy change note to account for
        // the gas cost increase from adding the output
        let delta_with_change = self.with_dummy_change_note()?.funding_delta::<G>()?;

        match delta_with_change.cmp(&0) {
            Ordering::Less | Ordering::Equal => {
                // NOTE: the `Equal` is important here since we
                // cannot create zero-valued outputs.

                // The increase in cost due to the change note means
                // we have insufficient funds, need more UTXO's.
                Ok(None)
            }
            Ordering::Greater => {
                // We have enough balance to cover the increase in cost from the change
                // note. Use return_change which properly accounts for the gas cost
                // increase from adding the change output.
                let change =
                    u64::try_from(delta_with_change).expect("Positive delta must fit in u64");

                let tx_with_change = self.add_ledger_output(Note {
                    value: change,
                    pk: change_pk,
                })?;

                // Now the net balance should exactly equal the gas cost.
                assert_eq!(tx_with_change.funding_delta::<G>()?, 0);

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

    pub fn gas_cost<G: GasConstants>(&self) -> Result<GasCost, TxBuilderError> {
        let build = self.clone().build()?;
        Ok(build.total_gas_cost::<G>(&self.context.gas_context)?)
    }

    pub fn funding_delta<G: GasConstants>(&self) -> Result<i128, TxBuilderError> {
        Ok(self.net_balance() - i128::from(self.gas_cost::<G>()?.into_inner()))
    }

    /// Returns all note IDs already consumed or locked by this transaction,
    /// plus the funding inputs that will be appended as a transfer during
    /// build.
    pub fn consumed_or_locked_notes(&self) -> impl Iterator<Item = NoteId> {
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
                    Op::SDPDeclare(declare) => Some(declare.locked_note_id),
                    Op::SDPWithdraw(withdraw) => Some(withdraw.locked_note_id),
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
    pub fn build(mut self) -> Result<MantleTx, TxBuilderError> {
        self.mantle_tx
            .0
            .try_push(Op::Transfer(self.pending_transfer))
            .map_err(|err| TxBuilderError::from((err, TooManyTag::Ops)))?;
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
            gas::MainnetGasConstants,
            ops::{
                channel::{
                    ChannelId,
                    deposit::{DepositOp, Metadata},
                    inscribe::InscriptionOp,
                },
                leader_claim::LeaderClaimOp,
                sdp::{SDPDeclareOp, SDPWithdrawOp},
            },
            tx::MantleTxGasContext,
        },
        sdp::{DeclarationId, Locator, ProviderId, ServiceType},
    };

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
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context)
            .push_op(Op::ChannelInscribe(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(builder.funding_delta::<MainnetGasConstants>().unwrap(), 0);
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
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context)
            .push_op(Op::ChannelDeposit(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(builder.funding_delta::<MainnetGasConstants>().unwrap(), 0);
    }

    #[test]
    fn withdraw_op() {
        // Build an operation
        let withdraw_note = Note {
            value: 5,
            pk: ZkPublicKey::zero(),
        };
        let op = ChannelWithdrawOp {
            channel_id: [0; 32].into(),
            outputs: Outputs::new([withdraw_note]),
            withdraw_nonce: 0,
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
        let builder = MantleTxBuilder::new(context)
            .push_op(Op::ChannelWithdraw(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(builder.funding_delta::<MainnetGasConstants>().unwrap(), 0);
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
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context)
            .push_op(Op::LeaderClaim(op))
            .unwrap();

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(builder.funding_delta::<MainnetGasConstants>().unwrap(), 0);
    }

    #[test]
    fn transfer_op() {
        // Init a tx builder for sending 30 to the recipient
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context)
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()))
            .unwrap()
            .add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(50, ZkPublicKey::zero())));
        let builder = builder.unwrap();

        // Check that the balance is 10 (= 50 - 40)
        assert_eq!(builder.net_balance(), 10);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            10 // zero gas price for now
        );

        // Add change note
        let builder = builder
            .return_change::<MainnetGasConstants>(ZkPublicKey::zero())
            .unwrap()
            .unwrap();

        // Check the tx is balanced
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
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
        let withdraw_note = Note {
            value: 5,
            pk: ZkPublicKey::zero(),
        };
        let builder = MantleTxBuilder::new(context)
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
                outputs: Outputs::new([withdraw_note]),
                withdraw_nonce: 0,
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
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            -40 // zero gas price for now
        );

        // Fund tx
        let builder = builder
            .add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(40, ZkPublicKey::zero())))
            .unwrap();

        // Check the tx is balanced
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            0 // zero gas price for now
        );
    }

    #[test]
    fn consumed_or_locked_notes() {
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };

        let deposit_input = NoteId(Fr::from(1u64));
        let declare_locked = NoteId(Fr::from(2u64));
        let withdraw_locked = NoteId(Fr::from(3u64));
        let transfer_input = Utxo::new([0u8; 32], 0, Note::new(50, ZkPublicKey::zero()));

        let builder = MantleTxBuilder::new(context)
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
                locked_note_id: declare_locked,
            }))
            .unwrap()
            .push_op(Op::SDPWithdraw(SDPWithdrawOp {
                declaration_id: DeclarationId([0; 32]),
                locked_note_id: withdraw_locked,
                nonce: 1,
            }))
            .unwrap()
            .add_ledger_input(transfer_input)
            .unwrap();

        let consumed_or_locked: Vec<_> = builder.consumed_or_locked_notes().collect();
        assert!(
            consumed_or_locked.contains(&deposit_input),
            "should contain deposit input"
        );
        assert!(
            consumed_or_locked.contains(&declare_locked),
            "should contain declare locked note"
        );
        assert!(
            consumed_or_locked.contains(&withdraw_locked),
            "should contain withdraw locked note"
        );
        assert!(
            consumed_or_locked.contains(&transfer_input.id()),
            "should contain transfer input"
        );
        assert_eq!(consumed_or_locked.len(), 4);
    }
}
