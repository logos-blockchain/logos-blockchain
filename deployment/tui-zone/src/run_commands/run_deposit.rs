use lb_core::mantle::{
    Op, OpProof, SignedMantleTx, Transaction as _, ops::channel::inscribe::Inscription,
};
use lb_key_management_system_service::keys::ZkKey;

use crate::{
    cli::{DepositArgs, RunResult},
    run_commands::{
        ZONE_DEPOSIT_SUBMISSION, ZONE_FILE_TRANSFER_VERSION, ZONE_WALLET_FUNDS_EXPORT,
        driver::{CommandGoal, WaitFor, drive_until_observed},
        types::{DepositSubmission, WalletFundsExport},
        utils::{
            build_deposit_op, build_deposit_transfer, decode_exported_utxos,
            decode_required_hex_bincode, read_json, resolve_channel_id, start_cli_sequencer,
            timestamp, validate_kind, write_json,
        },
    },
};

pub(crate) async fn run_deposit(args: DepositArgs) -> RunResult<()> {
    let funds = read_json::<WalletFundsExport>(&args.funds)?;
    validate_kind(&funds.kind, ZONE_WALLET_FUNDS_EXPORT, funds.version)?;
    let wallet_key = decode_required_hex_bincode::<ZkKey>(
        funds.secret_key.as_deref(),
        "funds JSON is missing secret_key; rerun EXPORT_FUNDS with include_secret true",
    )?;
    let funding_public_key = wallet_key.to_public_key();
    let available_utxos = decode_exported_utxos(&funds)?;
    let (transfer, reserved_inputs) =
        build_deposit_transfer(available_utxos, funding_public_key, args.amount)?;
    let channel_id = resolve_channel_id(&args.node_key)?;
    let deposit = build_deposit_op(channel_id, &transfer, &args.metadata)?;
    let inscription = Inscription::try_from(args.message.into_bytes())?;
    let mut sequencer = start_cli_sequencer(&args.node_key).await?;
    let status_rx = sequencer.subscribe_tx_status();
    let goal_inputs = deposit.inputs.clone();
    let goal_metadata = deposit.metadata.clone();
    let (tx, msg_id, sequencer_sig) = sequencer.handle().prepare_tx(
        [Op::Transfer(transfer), Op::ChannelDeposit(deposit)].into(),
        inscription,
    )?;
    let user_sig = ZkKey::multi_sign(&[wallet_key], &tx.hash().to_fr())?;
    let signed_tx = SignedMantleTx::new(
        tx,
        vec![
            OpProof::ZkSig(user_sig.clone()),
            OpProof::ZkSig(user_sig),
            OpProof::Ed25519Sig(sequencer_sig),
        ],
    )?;
    let tx_hash = signed_tx.hash();
    if args.submit {
        let goal = CommandGoal::Deposit {
            tx_hash,
            inputs: goal_inputs,
            amount: args.amount,
            metadata: goal_metadata,
        };
        let (_result, _checkpoint) = sequencer.handle().submit_signed_tx(signed_tx, msg_id)?;
        println!(
            "{} deposit: submitted tx_hash={} msg_id={}",
            timestamp(),
            hex::encode(tx_hash.as_ref()),
            hex::encode(msg_id.as_ref())
        );
        let wait_for = if args.wait_finalized {
            WaitFor::Finalized
        } else {
            WaitFor::OnChain
        };
        drive_until_observed(
            &channel_id,
            &mut sequencer,
            status_rx,
            goal,
            wait_for,
            "deposit",
        )
        .await?;
    }
    let submission = DepositSubmission {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_DEPOSIT_SUBMISSION.to_owned(),
        channel_id: hex::encode(channel_id.as_ref()),
        amount: args.amount,
        tx_hash: hex::encode(tx_hash.as_ref()),
        msg_id: hex::encode(msg_id.as_ref()),
        recipient_wallet_public_key: funds.public_key,
        reserved_input_ids: reserved_inputs
            .iter()
            .map(|utxo| hex::encode(utxo.id().as_bytes()))
            .collect(),
    };
    if let Some(path) = args.out {
        write_json(&path, &submission)?;
    }
    println!(
        "{} deposit: tx_hash={} msg_id={}",
        timestamp(),
        submission.tx_hash,
        submission.msg_id
    );
    Ok(())
}
