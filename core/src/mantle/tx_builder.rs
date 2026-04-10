use std::{cmp::Ordering, collections::HashMap};

use lb_key_management_system_keys::keys::ZkPublicKey;

use super::{GasCalculator as _, GasConstants, MantleTx, Note, Op, Utxo};
use crate::{
    mantle::{
        NoteId,
        gas::{GasCost, GasOverflow, GasPrice},
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
            pending_transfer: TransferOp::new(vec![], vec![]),
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
        self.pending_transfer.outputs.extend(notes);
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

        // TODO: reuse this for `LedgerState::try_apply_tx` with some refactoring
        // https://github.com/logos-blockchain/logos-blockchain/issues/2498
        let ops_balance: i128 = self
            .mantle_tx
            .ops
            .iter()
            .map(|op| match op {
                Op::ChannelDeposit(deposit) => -i128::from(deposit.amount),
                Op::ChannelWithdraw(withdraw) => i128::from(withdraw.amount),
                Op::LeaderClaim(_) => i128::from(self.context.leader_reward_amount),
                // `Op::Transfer` is not handled here since `self.ledger_inputs` and
                // `self.pending_transfer.outputs` already account for the balance changes from
                // `Op::Transfer`s.
                _ => 0,
            })
            .sum();

        in_sum - out_sum + ops_balance
    }

    pub fn gas_cost<G: GasConstants>(&self) -> Result<GasCost, GasOverflow> {
        let build = self.clone().build();
        build.total_gas_cost::<G>(&self.context.gas_context)
    }

    pub fn funding_delta<G: GasConstants>(&self) -> Result<i128, GasOverflow> {
        Ok(self.net_balance() - i128::from(self.gas_cost::<G>()?.into_inner()))
    }

    /// Returns all note IDs used as inputs in the transaction, including
    /// - Transfer operations already in the transaction
    /// - Additional transfer operations that will be added to the transaction
    pub fn input_notes(&self) -> impl Iterator<Item = NoteId> {
        self.mantle_tx
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Transfer(transfer) => Some(transfer.inputs.iter().copied()),
                _ => None,
            })
            .flatten()
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
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{
        gas::MainnetGasConstants,
        ops::{
            channel::{ChannelId, deposit::DepositOp, inscribe::InscriptionOp},
            leader_claim::LeaderClaimOp,
        },
        tx::MantleTxGasContext,
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

        // Check that the tx is already balanced becuase of zero gas price
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(builder.funding_delta::<MainnetGasConstants>().unwrap(), 0);
    }

    #[test]
    fn deposit_op() {
        // Build an operation
        let op = DepositOp {
            channel_id: [0; 32].into(),
            amount: 1,
            metadata: b"Mint 1 to Alice in Zone".to_vec(),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context).push_op(Op::ChannelDeposit(op.clone()));

        // Check that the balance reflects the deposit op
        assert_eq!(builder.net_balance(), -i128::from(op.amount)); // not yet funded
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            -i128::from(op.amount) // zero gas price for now
        );

        // Fund tx and add change note
        let builder = builder
            .add_ledger_input(Utxo::new(
                BigUint::ZERO.into(),
                0,
                Note::new(3, ZkPublicKey::zero()),
            ))
            .add_ledger_output(Note::new(2, ZkPublicKey::zero()));

        // Check the tx is balanced
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            0 // zero gas price for now
        );
    }

    #[test]
    fn withdraw_op() {
        // Build an operation
        let op = ChannelWithdrawOp {
            channel_id: [0; 32].into(),
            amount: 1,
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::new([(op.channel_id, 1)].into()),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context).push_op(Op::ChannelWithdraw(op.clone()));

        // Check that the balance reflects the withdraw op
        assert_eq!(builder.net_balance(), i128::from(op.amount)); // not yet funded
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            i128::from(op.amount) // zero gas price for now
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
    fn leader_claim_op() {
        // Build an operation
        let op = LeaderClaimOp {
            rewards_root: Fr::ZERO.into(),
            voucher_nullifier: Fr::ZERO.into(),
        };

        // Init a tx builder
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context.clone()).push_op(Op::LeaderClaim(op));

        // Check that the balance reflects the LeaderClaim op
        assert_eq!(
            builder.net_balance(),
            i128::from(context.leader_reward_amount) // not yet funded
        );
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            i128::from(context.leader_reward_amount) // zero gas price for now
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
    fn transfer_op() {
        // Init a tx builder for sending 30 to the recipient
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::default(),
            leader_reward_amount: 30,
        };
        let builder = MantleTxBuilder::new(context)
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()))
            .add_ledger_input(Utxo::new(
                BigUint::ZERO.into(),
                0,
                Note::new(50, ZkPublicKey::zero()),
            ));

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
        let builder = MantleTxBuilder::new(context)
            .push_op(Op::ChannelInscribe(InscriptionOp {
                channel_id,
                inscription: b"hello".into(),
                parent: [1; 32].into(),
                signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
            }))
            .push_op(Op::ChannelDeposit(DepositOp {
                channel_id,
                amount: 10,
                metadata: b"Mint 10 to Alice in Zone".to_vec(),
            }))
            .push_op(Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id,
                amount: 1,
            }))
            .push_op(Op::LeaderClaim(LeaderClaimOp {
                rewards_root: Fr::ZERO.into(),
                voucher_nullifier: Fr::ZERO.into(),
            }))
            .add_ledger_output(Note::new(40, ZkPublicKey::zero()));

        // Check the balance before funding tx: -10 + 1 + 30 - 40 = -19
        assert_eq!(builder.net_balance(), -19);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            -19 // zero gas price for now
        );

        // Fund tx
        let builder = builder.add_ledger_input(Utxo::new(
            BigUint::ZERO.into(),
            0,
            Note::new(19, ZkPublicKey::zero()),
        ));

        // Check the tx is balanced
        assert_eq!(builder.net_balance(), 0);
        assert_eq!(
            builder.funding_delta::<MainnetGasConstants>().unwrap(),
            0 // zero gas price for now
        );
    }
}
