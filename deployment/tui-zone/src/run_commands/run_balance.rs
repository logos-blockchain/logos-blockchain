use crate::{
    cli::{RunResult, StateArgs},
    run_commands::utils::{
        node_client, print_channel_balance, print_channel_state, query_channel_state,
        resolve_channel_id,
    },
};

pub(crate) async fn run_state_balance(args: StateArgs) -> RunResult<()> {
    let channel_id = resolve_channel_id(&args.node_key)?;
    let node = node_client(&args.node_key.node_url)?;
    let channel_state = query_channel_state(&node, channel_id).await;
    print_channel_balance("balance", &channel_id, channel_state.as_ref());
    Ok(())
}

pub(crate) async fn run_state_full(args: StateArgs) -> RunResult<()> {
    let channel_id = resolve_channel_id(&args.node_key)?;
    let node = node_client(&args.node_key.node_url)?;
    let channel_state = query_channel_state(&node, channel_id).await;
    print_channel_state("state", &channel_id, channel_state.as_ref());
    Ok(())
}
