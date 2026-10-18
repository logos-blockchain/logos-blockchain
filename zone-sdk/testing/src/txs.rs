//! Transactions a test builds outside the SDK: user deposits, hand-made
//! channel transfers and multi-inscription custom txs.

use lb_core::{
    mantle::{
        Note, NoteId, Op, OpProof, SignedOps, Utxo,
        channel::{SlotTimeframe, SlotTimeout},
        gas::GasCost,
        ledger::{Inputs, Outputs, verification_mode::StandardMode},
        ops::{
            OpId as _,
            channel::{
                ChannelId, MsgId, VerifiedChannelKeys,
                channel_transfer::ChannelTransferOp,
                config::ChannelConfigOp,
                deposit::{DepositOp, Metadata},
                inscribe::{Inscription, InscriptionOp},
                withdraw::ChannelWithdrawOp,
            },
            op_proof::samples::SampleProof as _,
        },
        traits::Hashable as _,
        transactions::{builder::MantleTxBuilder, states::Unverified, tx_list::OpProofs},
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
};
use lb_http_api_common::bodies::wallet::fund::WalletFundRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey, ZkSignature};
use lb_zone_sdk::adapter::Node as _;

use crate::node::SimNode;

/// Error building or funding a hand-made transaction.
#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("funding failed: {0}")]
    Funding(String),
    #[error("transaction assembly failed: {0}")]
    Assembly(String),
}

/// A deposit of `inputs` into `channel`, funded by the node from the notes
/// of `payer` like the node's deposit endpoint does, proven with sample ZK
/// signatures (the simulated node trusts ZK proofs).
pub async fn deposit_tx(
    node: &SimNode,
    channel: ChannelId,
    inputs: &[Utxo],
    metadata: &str,
    payer: ZkPublicKey,
) -> Result<(SignedOps<Unverified, StandardMode>, DepositOp), TxError> {
    let op = DepositOp {
        channel_id: channel,
        inputs: Inputs::try_new(inputs.iter().map(Utxo::id).collect::<Vec<_>>())
            .map_err(|e| TxError::Assembly(format!("deposit inputs: {e:?}")))?,
        metadata: Metadata::try_from(metadata.as_bytes().to_vec())
            .map_err(|e| TxError::Assembly(format!("metadata: {e:?}")))?,
    };
    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelDeposit(op.clone()))
        .map_err(|e| TxError::Assembly(format!("too many ops: {e}")))?;
    let response = node
        .fund_tx(fund_request(tx_builder, payer))
        .await
        .map_err(|e| TxError::Funding(e.to_string()))?;
    let mut proofs = OpProofs::from([OpProof::ZkSig(ZkSignature::sample())]);
    if let Some(transfer_proof) = response.transfer_proof {
        proofs
            .try_push(transfer_proof)
            .map_err(|e| TxError::Assembly(format!("too many proofs: {e:?}")))?;
    }
    let tx = SignedOps::from_parts(response.funded_tx, proofs)
        .map_err(|e| TxError::Assembly(e.to_string()))?;
    Ok((tx, op))
}

/// The channel notes a deposit re-creates, one per input in order.
#[must_use]
pub fn recreated_channel_notes(deposit: &DepositOp, inputs: &[Utxo]) -> Vec<Utxo> {
    let op_id = deposit.op_id();
    inputs
        .iter()
        .enumerate()
        .map(|(index, utxo)| Utxo::new(op_id, index, utxo.note))
        .collect()
}

fn fund_request(tx_builder: MantleTxBuilder, funding_pk: ZkPublicKey) -> WalletFundRequestBody {
    WalletFundRequestBody {
        tip: None,
        priority_fee_percent: 0,
        tx_builder,
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(u64::MAX),
    }
}

/// Chain `payloads` as inscriptions from `parent` in one funded transaction
/// signed by `signing_key`; returns the tx and the id of its last message.
pub async fn build_funded_custom_tx(
    node: &SimNode,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    funding_pk: ZkPublicKey,
    payloads: &[Inscription],
    mut parent: MsgId,
) -> Result<(SignedOps<Unverified, StandardMode>, MsgId), TxError> {
    let signer = signing_key.public_key().into_unverified();
    let mut tx_builder = MantleTxBuilder::new();
    for payload in payloads {
        let op = InscriptionOp {
            channel_id,
            inscription: payload.clone(),
            parent,
            signer,
        };
        parent = op.id();
        tx_builder = tx_builder
            .push_op(Op::ChannelInscribe(op))
            .map_err(|e| TxError::Assembly(format!("too many ops: {e}")))?;
    }
    let response = node
        .fund_tx(fund_request(tx_builder, funding_pk))
        .await
        .map_err(|e| TxError::Funding(e.to_string()))?;
    let funded_tx = response.funded_tx;
    let signature = signing_key.sign_payload(funded_tx.hash().as_signing_bytes());
    let mut op_proofs = OpProofs::try_from(vec![OpProof::Ed25519Sig(signature); payloads.len()])
        .map_err(|e| TxError::Assembly(format!("too many proofs: {e:?}")))?;
    if let Some(proof) = response.transfer_proof {
        op_proofs
            .try_push(proof)
            .map_err(|e| TxError::Assembly(format!("too many proofs: {e:?}")))?;
    }
    let signed = SignedOps::from_parts(funded_tx, op_proofs)
        .map_err(|e| TxError::Assembly(e.to_string()))?;
    Ok((signed, parent))
}

/// Who signs a channel multi-sig op: the key and its index in the channel's
/// accredited list (threshold 1).
pub struct Signer<'a> {
    pub key: &'a Ed25519Key,
    pub index: u16,
}

/// Fund `ops` from `funding_pk`, then prove every op with one multi-sig
/// signature of `signer` over the funded tx.
async fn build_multisig_tx(
    node: &SimNode,
    signer: Signer<'_>,
    funding_pk: ZkPublicKey,
    ops: Vec<Op>,
) -> Result<SignedOps<Unverified, StandardMode>, TxError> {
    let mut tx_builder = MantleTxBuilder::new();
    let count = ops.len();
    for op in ops {
        tx_builder = tx_builder
            .push_op(op)
            .map_err(|e| TxError::Assembly(format!("too many ops: {e}")))?;
    }
    let response = node
        .fund_tx(fund_request(tx_builder, funding_pk))
        .await
        .map_err(|e| TxError::Funding(e.to_string()))?;
    let funded_tx = response.funded_tx;
    let signature = signer.key.sign_payload(funded_tx.hash().as_signing_bytes());
    let proof = ChannelMultiSigProof::new(IndexedSignatures::from((signer.index, signature)));
    let mut op_proofs = OpProofs::try_from(vec![OpProof::ChannelMultiSigProof(proof); count])
        .map_err(|e| TxError::Assembly(format!("too many proofs: {e:?}")))?;
    if let Some(transfer_proof) = response.transfer_proof {
        op_proofs
            .try_push(transfer_proof)
            .map_err(|e| TxError::Assembly(format!("too many proofs: {e:?}")))?;
    }
    SignedOps::from_parts(funded_tx, op_proofs).map_err(|e| TxError::Assembly(e.to_string()))
}

/// A channel transfer of `inputs` into `outputs`, authorized by `signer`.
pub async fn build_channel_transfer_tx(
    node: &SimNode,
    channel_id: ChannelId,
    signer: Signer<'_>,
    funding_pk: ZkPublicKey,
    inputs: Vec<NoteId>,
    outputs: Vec<Note>,
) -> Result<SignedOps<Unverified, StandardMode>, TxError> {
    let transfer = ChannelTransferOp {
        channel_id,
        inputs: Inputs::try_new(inputs)
            .map_err(|e| TxError::Assembly(format!("transfer inputs: {e:?}")))?,
        outputs: Outputs::try_new(outputs)
            .map_err(|e| TxError::Assembly(format!("transfer outputs: {e:?}")))?,
    };
    build_multisig_tx(
        node,
        signer,
        funding_pk,
        vec![Op::ChannelTransfer(transfer)],
    )
    .await
}

/// Split one channel note into `dust_count` notes of value 1 owned by
/// `recipient`, authorized by `signing_key` at channel key index 0.
pub async fn build_channel_split_tx(
    node: &SimNode,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    funding_pk: ZkPublicKey,
    input_note: Utxo,
    recipient: ZkPublicKey,
    dust_count: usize,
) -> Result<SignedOps<Unverified, StandardMode>, TxError> {
    build_channel_transfer_tx(
        node,
        channel_id,
        Signer {
            key: signing_key,
            index: 0,
        },
        funding_pk,
        vec![input_note.id()],
        vec![Note::new(1, recipient); dust_count],
    )
    .await
}

/// A withdraw out of channel notes, shaped like the SDK's atomic withdraw.
#[derive(Clone, Debug)]
pub struct WithdrawPlan {
    /// Channel notes spent, with their values.
    pub inputs: Vec<(NoteId, u64)>,
    pub amount: u64,
    /// Gets `amount` as a released note.
    pub recipient: ZkPublicKey,
    /// Keeps the change in the channel.
    pub change_pk: ZkPublicKey,
}

/// Withdraw per `plan`, authorized by `signer`.
///
/// A transfer re-keys `amount` to the recipient and returns the change to
/// the change key, then a withdraw releases the recipient's note. No
/// inscription.
pub async fn build_channel_withdraw_tx(
    node: &SimNode,
    channel_id: ChannelId,
    signer: Signer<'_>,
    funding_pk: ZkPublicKey,
    plan: &WithdrawPlan,
) -> Result<SignedOps<Unverified, StandardMode>, TxError> {
    let total: u64 = plan.inputs.iter().map(|(_, value)| value).sum();
    let amount = plan.amount;
    let change = total
        .checked_sub(amount)
        .ok_or_else(|| TxError::Assembly(format!("inputs {total} below amount {amount}")))?;
    let mut outputs = vec![Note::new(amount, plan.recipient)];
    if change > 0 {
        outputs.push(Note::new(change, plan.change_pk));
    }
    let transfer = ChannelTransferOp {
        channel_id,
        inputs: Inputs::try_new(plan.inputs.iter().map(|(id, _)| *id).collect::<Vec<_>>())
            .map_err(|e| TxError::Assembly(format!("transfer inputs: {e:?}")))?,
        outputs: Outputs::try_new(outputs)
            .map_err(|e| TxError::Assembly(format!("transfer outputs: {e:?}")))?,
    };
    let recipient_note = transfer
        .utxos()
        .next()
        .map(|utxo| utxo.id())
        .ok_or_else(|| TxError::Assembly("transfer without outputs".to_owned()))?;
    let withdraw = ChannelWithdrawOp {
        channel_id,
        inputs: Inputs::new(recipient_note),
    };
    build_multisig_tx(
        node,
        signer,
        funding_pk,
        vec![Op::ChannelTransfer(transfer), Op::ChannelWithdraw(withdraw)],
    )
    .await
}

/// The posting rules a config sets.
#[derive(Clone, Debug)]
pub struct ConfigRules {
    pub keys: VerifiedChannelKeys,
    pub posting_timeframe: u32,
    pub posting_timeout: u32,
    pub configuration_threshold: u16,
    pub transfer_threshold: u16,
}

/// A channel config chaining on `parent` (the current config tip),
/// authorized by `signer` under the current accredited list.
pub async fn build_channel_config_tx(
    node: &SimNode,
    channel_id: ChannelId,
    signer: Signer<'_>,
    funding_pk: ZkPublicKey,
    parent: MsgId,
    rules: ConfigRules,
) -> Result<SignedOps<Unverified, StandardMode>, TxError> {
    let config = ChannelConfigOp {
        channel: channel_id,
        parent,
        keys: rules.keys,
        posting_timeframe: SlotTimeframe::from(rules.posting_timeframe),
        posting_timeout: SlotTimeout::from(rules.posting_timeout),
        configuration_threshold: rules.configuration_threshold,
        transfer_threshold: rules.transfer_threshold,
    };
    build_multisig_tx(node, signer, funding_pk, vec![Op::ChannelConfig(config)]).await
}
