use std::{cmp::Ordering, collections::HashMap, ops::DerefMut as _};

use lb_key_management_system_keys::keys::ZkPublicKey;

use super::{GasCalculator as _, GasConstants, MantleTx, Note, Op, Utxo};
use crate::{
    mantle::{
        NoteId,
        gas::{GasCost, GasOverflow, GasPrice},
        ledger::{Inputs, Outputs},
        ops::{channel::withdraw::ChannelWithdrawOp, transfer::TransferOp},
        tx::MantleTxContext,
    },
    proofs::channel_withdraw_proof::ChannelWithdrawProof,
};

#[derive(Debug, Clone)]
pub struct MantleTxBuilder {
    mantle_tx: MantleTx,
    ledger_inputs: Vec<Utxo>,
    pending_transfer: TransferOp,
    // Maps a Proof to its Op by the Op Index
    channel_withdraw_proofs: HashMap<usize, ChannelWithdrawProof>,
    context: MantleTxContext,
}

// TODO: refactor to support more than 32 inputs (more than a single transfer)
impl MantleTxBuilder {
    #[must_use]
    pub fn new(context: MantleTxContext) -> Self {
        Self {
            mantle_tx: MantleTx {
                ops: vec![],
                execution_gas_price: 0.into(),
                storage_gas_price: 0.into(),
            },
            ledger_inputs: vec![],
            pending_transfer: TransferOp::new(Inputs::new(vec![]), Outputs::new(vec![])),
            channel_withdraw_proofs: HashMap::new(),
            context,
        }
    }

    #[must_use]
    pub fn push_op(self, op: Op) -> Self {
        self.extend_ops([op])
    }

    #[must_use]
    pub fn extend_ops(mut self, ops: impl IntoIterator<Item = Op>) -> Self {
        self.mantle_tx.ops.extend(ops);
        self
    }

    #[must_use]
    pub fn push_channel_withdraw(self, op: ChannelWithdrawOp, proof: ChannelWithdrawProof) -> Self {
        let mut builder = self.push_op(Op::ChannelWithdraw(op));
        let index = builder.mantle_tx.ops.len() - 1;
        builder.channel_withdraw_proofs.insert(index, proof);
        builder
    }

    #[must_use]
    pub fn add_ledger_input(self, utxo: Utxo) -> Self {
        self.extend_ledger_inputs([utxo])
    }

    #[must_use]
    pub fn extend_ledger_inputs(mut self, utxos: impl IntoIterator<Item = Utxo>) -> Self {
        for utxo in utxos {
            self.pending_transfer.inputs.push(utxo.id());
            self.ledger_inputs.push(utxo);
        }
        self
    }

    #[must_use]
    pub fn add_ledger_output(self, note: Note) -> Self {
        self.extend_ledger_outputs([note])
    }

    #[must_use]
    pub fn extend_ledger_outputs(mut self, notes: impl IntoIterator<Item = Note>) -> Self {
        self.pending_transfer.outputs.deref_mut().extend(notes);
        self
    }

    #[must_use]
    pub const fn set_execution_gas_price(mut self, price: GasPrice) -> Self {
        self.mantle_tx.execution_gas_price = price;
        self
    }

    #[must_use]
    pub const fn set_storage_gas_price(mut self, price: GasPrice) -> Self {
        self.mantle_tx.storage_gas_price = price;
        self
    }

    pub fn return_change<G: GasConstants>(
        self,
        change_pk: ZkPublicKey,
    ) -> Result<Option<Self>, GasOverflow> {
        // Calculate the funding delta with a dummy change note to account for
        // the gas cost increase from adding the output
        let delta_with_change = self.with_dummy_change_note().funding_delta::<G>()?;

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
                });

                // Now the net balance should exactly equal the gas cost.
                assert_eq!(tx_with_change.funding_delta::<G>().unwrap(), 0);

                Ok(Some(tx_with_change))
            }
        }
    }

    #[must_use]
    pub fn with_dummy_change_note(&self) -> Self {
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

    pub fn gas_cost<G: GasConstants>(&self) -> Result<GasCost, GasOverflow> {
        let build = self.clone().build();
        build.total_gas_cost::<G>(&self.context.gas_context)
    }

    pub fn funding_delta<G: GasConstants>(&self) -> Result<i128, GasOverflow> {
        Ok(self.net_balance() - i128::from(self.gas_cost::<G>()?.into_inner()))
    }

    /// Returns all note IDs already consumed or locked by this transaction,
    /// plus the funding inputs that will be appended as a transfer during
    /// build.
    pub fn consumed_or_locked_notes(&self) -> impl Iterator<Item = NoteId> {
        self.mantle_tx
            .ops
            .iter()
            .flat_map(|op| {
                let inputs: &[NoteId] = match op {
                    Op::Transfer(transfer) => &transfer.inputs,
                    Op::ChannelDeposit(deposit) => &deposit.inputs,
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
    pub const fn channel_withdraw_proofs(&self) -> &HashMap<usize, ChannelWithdrawProof> {
        &self.channel_withdraw_proofs
    }

    #[must_use]
    pub fn build(mut self) -> MantleTx {
        self.mantle_tx.ops.push(Op::Transfer(self.pending_transfer));
        self.mantle_tx
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;
    use crate::{
        mantle::{
            gas::MainnetGasConstants,
            ops::{
                channel::{ChannelId, deposit::DepositOp, inscribe::InscriptionOp},
                leader_claim::LeaderClaimOp,
                sdp::{SDPDeclareOp, SDPWithdrawOp},
            },
            tx::MantleTxGasContext,
        },
        sdp::{DeclarationId, ProviderId, ServiceType},
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
        let builder = MantleTxBuilder::new(context).push_op(Op::ChannelInscribe(op));

        // Check that the tx is already balanced because of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(builder.funding_delta::<MainnetGasConstants>().unwrap(), 0);
    }

    #[test]
    fn deposit_op() {
        // Build an operation
        let op = DepositOp {
            channel_id: [0; 32].into(),
            inputs: Inputs::new(vec![NoteId(Fr::ZERO)]),
            metadata: b"Mint 1 to Alice in Zone".to_vec(),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context).push_op(Op::ChannelDeposit(op));

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
            outputs: Outputs::new(vec![withdraw_note]),
            withdraw_nonce: 0,
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new([(op.channel_id, 1)].into()),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context).push_op(Op::ChannelWithdraw(op));

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
        let builder = MantleTxBuilder::new(context).push_op(Op::LeaderClaim(op));

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
            .add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(50, ZkPublicKey::zero())));

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
            gas_context: MantleTxGasContext::new([(channel_id, 1)].into()),
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
            .push_op(Op::ChannelDeposit(DepositOp {
                channel_id,
                inputs: Inputs::new(vec![NoteId(Fr::ZERO)]),
                metadata: b"Mint 10 to Alice in Zone".to_vec(),
            }))
            .push_op(Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id,
                outputs: Outputs::new(vec![withdraw_note]),
                withdraw_nonce: 0,
            }))
            .push_op(Op::LeaderClaim(LeaderClaimOp {
                rewards_root: Fr::ZERO.into(),
                voucher_nullifier: Fr::ZERO.into(),
                pk: ZkPublicKey::zero(),
            }))
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()));

        // Check the balance before funding tx
        assert_eq!(builder.net_balance(), -40);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            -40 // zero gas price for now
        );

        // Fund tx
        let builder =
            builder.add_ledger_input(Utxo::new([0u8; 32], 0, Note::new(40, ZkPublicKey::zero())));

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
                inputs: Inputs::new(vec![deposit_input]),
                metadata: vec![],
            }))
            .push_op(Op::SDPDeclare(SDPDeclareOp {
                service_type: ServiceType::BlendNetwork,
                locators: vec![],
                provider_id: ProviderId(Ed25519Key::from_bytes(&[0; 32]).public_key()),
                zk_id: ZkPublicKey::zero(),
                locked_note_id: declare_locked,
            }))
            .push_op(Op::SDPWithdraw(SDPWithdrawOp {
                declaration_id: DeclarationId([0; 32]),
                locked_note_id: withdraw_locked,
                nonce: 1,
            }))
            .add_ledger_input(transfer_input);

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
