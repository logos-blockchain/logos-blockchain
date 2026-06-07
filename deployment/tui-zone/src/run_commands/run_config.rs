use std::path::PathBuf;

use lb_core::mantle::{Transaction as _, ops::channel::config::Keys};

use crate::{
    cli::{ConfigArgs, RunResult},
    run_commands::{
        driver::{CommandGoal, WaitFor, drive_until_observed},
        utils::{
            load_or_create_signing_key, resolve_channel_id, save_cli_checkpoint,
            start_cli_sequencer, timestamp,
        },
    },
};

pub(crate) async fn run_config(args: ConfigArgs) -> RunResult<()> {
    if args.withdraw_threshold == 0 {
        return Err("withdraw_threshold must be greater than 0".into());
    }
    if args.configuration_threshold == 0 {
        return Err("configuration_threshold must be greater than 0".into());
    }

    let admin_key = load_or_create_signing_key(PathBuf::from(&args.node_key.key_path).as_path());
    let mut authorized_keys = vec![admin_key.public_key()];
    for key_path in &args.authorized_key_paths {
        let public_key = load_or_create_signing_key(PathBuf::from(key_path).as_path()).public_key();
        if !authorized_keys.contains(&public_key) {
            authorized_keys.push(public_key);
        }
    }
    if args.withdraw_threshold as usize > authorized_keys.len() {
        return Err(format!(
            "withdraw_threshold {} exceeds authorized key count {}",
            args.withdraw_threshold,
            authorized_keys.len()
        )
        .into());
    }
    if args.configuration_threshold as usize > authorized_keys.len() {
        return Err(format!(
            "configuration_threshold {} exceeds authorized key count {}",
            args.configuration_threshold,
            authorized_keys.len()
        )
        .into());
    }

    let channel_id = resolve_channel_id(&args.node_key)?;
    let mut sequencer = start_cli_sequencer(&args.node_key).await?;
    let status_rx = sequencer.subscribe_tx_status();
    let (result, checkpoint, signed_tx) = sequencer.handle().channel_config(
        Keys::try_from(authorized_keys)?,
        args.posting_timeframe.into(),
        args.posting_timeout.into(),
        args.configuration_threshold,
        args.withdraw_threshold,
    )?;
    save_cli_checkpoint(&channel_id, &checkpoint)?;
    let tx_hash = signed_tx.hash();
    let goal = CommandGoal::Tx { tx_hash };
    println!(
        "{} zone config submitted channel_id={} tx_hash={} withdraw_threshold={} configuration_threshold={}",
        timestamp(),
        hex::encode(channel_id.as_ref()),
        hex::encode(result.inscription_id().as_ref()),
        args.withdraw_threshold,
        args.configuration_threshold
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
        "zone_config",
    )
    .await?;
    Ok(())
}
