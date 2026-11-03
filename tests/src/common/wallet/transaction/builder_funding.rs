//! Applies wallet funding decisions to Mantle transaction builders.

use std::cmp::Ordering;

use lb_core::{
    crypto::ZkHasher,
    mantle::{
        Note,
        gas::MainnetGasProfile,
        transactions::{MantleTxBuilder, builder::FundingNote, tx_list::ops::OpsContext},
    },
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_wallet::{OwnedNote, WalletError};
use lb_zktransfer::MAX_INPUTS;

use super::{error::WalletTransactionError, intent::WalletTransactionIntent};
use crate::common::wallet::{
    WalletFundingOutcome, WalletFundingPlan, WalletFundingResources, WalletFundingUtxos,
    WalletInputSelectionStrategy, WalletSelectedInputs,
};

// The nonce of a note the test wallet creates, drawn at random so that no two
// notes of a key share a commitment.
#[must_use]
pub fn random_nonce() -> Fr {
    Fr::from(rand::random::<u128>())
}

pub(super) fn fund_wallet_transaction(
    intent: WalletTransactionIntent,
    resources: WalletFundingResources,
    priority_fee_percent: u64,
) -> Result<(MantleTxBuilder, OpsContext, Vec<Ed25519Key>), WalletError> {
    let (sender, fee_sponsor) = resources.into_parts();
    let (tx_builder, context, sender_output_total, leading_inscription_signers) =
        intent.into_parts();

    let funded_builder = match fee_sponsor {
        None => {
            let (sender, input_selection_strategy) = sender.into_funding_parts();
            fund_unsponsored_wallet_transaction(
                &tx_builder,
                sender,
                input_selection_strategy,
                &context,
                priority_fee_percent,
            )?
        }
        Some(fee_sponsor) => {
            let (sender, input_selection_strategy) = sender.into_funding_parts();
            fund_sponsored_wallet_transaction(
                tx_builder,
                sender_output_total,
                fee_sponsor.into_funding_utxos(),
                sender,
                input_selection_strategy,
                &context,
                priority_fee_percent,
            )?
        }
    };

    Ok((funded_builder, context, leading_inscription_signers))
}

/// Estimate the exact gas reserve for a bounded continuous-workload input
/// candidate, both with and without a change output. This is diagnostic-only:
/// funding still goes through the normal transaction builder.
pub fn estimate_workload_fee_requirements(
    intent: &WalletTransactionIntent,
    candidate_inputs: &[OwnedNote],
    priority_fee_percent: u64,
) -> Result<(u64, u64), WalletTransactionError> {
    let (tx_builder, context, _, _) = intent.clone().into_parts();
    let builder_with_inputs = extend_wallet_funding_inputs(&tx_builder, candidate_inputs)?;
    let fee_without_change = required_fee_with_priority(
        builder_with_inputs
            .minimum_gas_cost::<MainnetGasProfile>(&context)?
            .into_inner(),
        priority_fee_percent,
    )?;
    let builder_with_change = builder_with_inputs.with_dummy_change_note()?;
    let fee_with_change = required_fee_with_priority(
        builder_with_change
            .minimum_gas_cost::<MainnetGasProfile>(&context)?
            .into_inner(),
        priority_fee_percent,
    )?;

    Ok((fee_without_change, fee_with_change))
}

fn required_fee_with_priority(
    mandatory_fee: u64,
    priority_fee_percent: u64,
) -> Result<u64, WalletTransactionError> {
    let mandatory_fee = u128::from(mandatory_fee);
    let priority_fee = mandatory_fee
        .checked_mul(u128::from(priority_fee_percent))
        .and_then(|fee| fee.checked_add(99))
        .ok_or(lb_core::mantle::gas::GasOverflow)?
        / 100;
    u64::try_from(mandatory_fee + priority_fee)
        .map_err(|_| lb_core::mantle::gas::GasOverflow.into())
}

fn fund_unsponsored_wallet_transaction(
    tx_builder: &MantleTxBuilder,
    sender: WalletFundingUtxos,
    input_selection_strategy: WalletInputSelectionStrategy,
    context: &OpsContext,
    priority_fee_percent: u64,
) -> Result<MantleTxBuilder, WalletError> {
    let sender_change_pk = sender.change_pk();
    let available_utxos = sender.into_available_utxos();
    let funding_plan = match input_selection_strategy {
        WalletInputSelectionStrategy::LargestFirst => {
            WalletFundingPlan::largest_first(available_utxos, Vec::new())
        }
        WalletInputSelectionStrategy::SmallestFirst => {
            WalletFundingPlan::smallest_first(available_utxos, Vec::new())
        }
        WalletInputSelectionStrategy::AllProvided => {
            WalletFundingPlan::all_provided(available_utxos)
        }
    };

    fund_builder_from_plan(
        tx_builder,
        sender_change_pk,
        &funding_plan,
        context,
        priority_fee_percent,
    )
}

fn fund_sponsored_wallet_transaction(
    tx_builder: MantleTxBuilder,
    output_total: u64,
    fee_sponsor: WalletFundingUtxos,
    sender: WalletFundingUtxos,
    input_selection_strategy: WalletInputSelectionStrategy,
    context: &OpsContext,
    priority_fee_percent: u64,
) -> Result<MantleTxBuilder, WalletError> {
    let sender_change_pk = sender.change_pk();
    let fee_sponsor_change_pk = fee_sponsor.change_pk();
    let sender_inputs = match input_selection_strategy {
        WalletInputSelectionStrategy::AllProvided => WalletSelectedInputs::all_provided_covering(
            sender.into_available_utxos(),
            output_total,
        )?,
        WalletInputSelectionStrategy::LargestFirst
        | WalletInputSelectionStrategy::SmallestFirst => {
            WalletSelectedInputs::largest_first_covering(
                sender.into_available_utxos(),
                output_total,
            )?
        }
    };
    let builder_with_sender_outputs = add_sender_change_output(
        tx_builder,
        sender_change_pk,
        output_total,
        sender_inputs.total(),
    )?;

    fund_builder_from_plan(
        &builder_with_sender_outputs,
        fee_sponsor_change_pk,
        &WalletFundingPlan::smallest_first(
            fee_sponsor.into_available_utxos(),
            sender_inputs.into_utxos(),
        ),
        context,
        priority_fee_percent,
    )
}

fn add_sender_change_output(
    tx_builder: MantleTxBuilder,
    change_pk: ZkPublicKey,
    output_total: u64,
    input_total: u64,
) -> Result<MantleTxBuilder, WalletError> {
    let change = input_total - output_total;
    if change > 0 {
        tx_builder
            .add_ledger_output(Note::new(change, random_nonce(), change_pk))
            .map_err(Into::into)
    } else {
        Ok(tx_builder)
    }
}

fn fund_builder_from_plan(
    tx_builder: &MantleTxBuilder,
    change_pk: ZkPublicKey,
    plan: &WalletFundingPlan,
    context: &OpsContext,
    priority_fee_percent: u64,
) -> Result<MantleTxBuilder, WalletError> {
    plan.fund_with(|selected_inputs| {
        evaluate_funding_inputs(
            tx_builder,
            selected_inputs,
            change_pk,
            context,
            priority_fee_percent,
        )
    })
}

/// Add wallet funding inputs using the same transfer layout used during
/// transaction preparation.
///
/// The funding Transfer spends at most four notes, proven against the root
/// their paths lead to: the notes of one wallet observation share it.
pub fn extend_wallet_funding_inputs(
    tx_builder: &MantleTxBuilder,
    funding_utxos: &[OwnedNote],
) -> Result<MantleTxBuilder, WalletError> {
    let Some(first) = funding_utxos.first() else {
        return Ok(tx_builder.clone());
    };
    let funding_root = first.path.root::<ZkHasher>(first.note.commitment().0);
    tx_builder
        .clone()
        .extend_funding_inputs(
            funding_root,
            funding_utxos.iter().map(|utxo| FundingNote {
                note: utxo.note,
                nullifier: utxo.nullifier,
            }),
        )
        .map_err(Into::into)
}

fn evaluate_funding_inputs(
    tx_builder: &MantleTxBuilder,
    selected_inputs: &[OwnedNote],
    change_pk: ZkPublicKey,
    context: &OpsContext,
    priority_fee_percent: u64,
) -> Result<WalletFundingOutcome<MantleTxBuilder>, WalletError> {
    if selected_inputs.is_empty() && tx_builder.funding_inputs().is_empty() {
        return Ok(WalletFundingOutcome::NeedsMoreInputs);
    }

    if tx_builder.funding_inputs().len() + selected_inputs.len() <= MAX_INPUTS {
        return evaluate_standard_funding_inputs(
            tx_builder,
            selected_inputs,
            change_pk,
            context,
            priority_fee_percent,
        );
    }

    // A ZkTransfer spends at most `MAX_INPUTS` notes, more cannot fund it.
    Ok(WalletFundingOutcome::NeedsMoreInputs)
}

fn evaluate_standard_funding_inputs(
    tx_builder: &MantleTxBuilder,
    selected_inputs: &[OwnedNote],
    change_pk: ZkPublicKey,
    context: &OpsContext,
    priority_fee_percent: u64,
) -> Result<WalletFundingOutcome<MantleTxBuilder>, WalletError> {
    let funded_builder = extend_wallet_funding_inputs(tx_builder, selected_inputs)?;

    match funded_builder
        .funding_delta_with_priority_fee::<MainnetGasProfile>(context, priority_fee_percent)?
        .cmp(&0)
    {
        Ordering::Less => Ok(WalletFundingOutcome::NeedsMoreInputs),
        Ordering::Equal => Ok(WalletFundingOutcome::Funded(funded_builder)),
        Ordering::Greater => Ok(funded_builder
            .return_change::<MainnetGasProfile>(
                context,
                change_pk,
                random_nonce(),
                priority_fee_percent,
            )?
            .map_or(
                WalletFundingOutcome::NeedsMoreInputs,
                WalletFundingOutcome::Funded,
            )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lb_core::mantle::{
        Op,
        note_set::LEDGER_SET,
        notes::NoteNullifier,
        ops::channel::{
            ChannelId, MsgId,
            inscribe::{ChannelSteps, Inscription, InscriptionOp},
        },
        transactions::{GasPrices, tx_list::ops::OpsGasContext},
    };
    use lb_mmr::MerklePath;
    use lb_testing_framework::configs::wallet::WalletAccount;

    use super::*;
    use crate::common::wallet::WalletFundingSource;

    #[test]
    fn zero_cost_wallet_transaction_still_uses_funding_input() {
        let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0)),
        };
        let tx_builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([0xAA; 32]),
                inscription: Inscription::new_unchecked(vec![0xab; 1024]),
                parent: MsgId::root(),
                signer: signing_key.public_key().into_unverified(),
                steps: ChannelSteps::default(),
            }))
            .expect("inscription test builder should fit op bounds");
        assert_eq!(
            tx_builder
                .funding_delta::<MainnetGasProfile>(&context)
                .expect("zero-gas inscription funding delta should calculate"),
            0
        );

        let account = WalletAccount::deterministic(1, 2_000_000, false)
            .expect("test wallet account should build");
        let funding_utxo = OwnedNote {
            note: Note::new(2_000_000, Fr::from(7u64), account.public_key()),
            note_set: LEDGER_SET,
            nullifier: NoteNullifier(Fr::from(7u64)),
            path: MerklePath::try_new(0, Vec::new()).expect("an empty path is bounded"),
        };
        let funding_source = WalletFundingSource::new(account, vec![funding_utxo.clone()]);

        let funded_builder = fund_unsponsored_wallet_transaction(
            &tx_builder,
            funding_source.into_funding_utxos(),
            WalletInputSelectionStrategy::LargestFirst,
            &context,
            0,
        )
        .expect("zero-cost wallet transaction should still select a funding input");

        assert_eq!(
            funded_builder.funding_inputs(),
            &[FundingNote {
                note: funding_utxo.note,
                nullifier: funding_utxo.nullifier,
            }]
        );
        assert_eq!(
            funded_builder
                .funding_delta::<MainnetGasProfile>(&context)
                .expect("funded inscription delta should calculate"),
            0
        );

        let funded_tx = funded_builder.build().expect("funded builder should build");
        let Some(Op::Transfer(transfer)) = funded_tx.iter().last() else {
            panic!("wallet funding should leave a transfer op at the end");
        };
        assert!(!transfer.inputs.is_empty());
    }
}
