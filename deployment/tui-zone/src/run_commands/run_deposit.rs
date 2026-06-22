use lb_core::mantle::{
    Op, OpProof, SignedMantleTx, Transaction as _, ops::channel::inscribe::Inscription,
};
use lb_key_management_system_service::keys::ZkKey;

use crate::{
    cli::{DepositArgs, RunResult},
    run_commands::{
        ZONE_WALLET_FUNDS_EXPORT,
        driver::{CommandGoal, WaitFor, drive_until_observed},
        types::WalletFundsExport,
        utils::{
            build_deposit_op, build_deposit_transfer, decode_exported_utxos,
            decode_required_hex_bincode, node_client, print_channel_balance, query_channel_state,
            read_json, resolve_channel_id, start_cli_sequencer_with_channel_state, timestamp,
            validate_kind,
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
    let (transfer, _reserved_inputs) =
        build_deposit_transfer(available_utxos, funding_public_key, args.amount)?;
    let channel_id = resolve_channel_id(&args.node_key)?;
    let deposit = build_deposit_op(channel_id, &transfer, &args.metadata)?;
    let inscription = Inscription::try_from(args.message.into_bytes())?;
    let (mut sequencer, channel_state) =
        start_cli_sequencer_with_channel_state(&args.node_key).await?;
    print_channel_balance("deposit before", &channel_id, channel_state.as_ref());
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

    let node = node_client(&args.node_key.node_url)?;
    let channel_state = query_channel_state(&node, channel_id).await;
    println!(
        "{} deposit: balance={}, tx_hash={} msg_id={}",
        timestamp(),
        if let Some(state) = channel_state {
            state.balance
        } else {
            0
        },
        hex::encode(tx_hash.as_ref()),
        hex::encode(msg_id.as_ref())
    );
    Ok(())
}
