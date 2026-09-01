use super::*;

#[when(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
#[then(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
async fn step_all_nodes_reached_min_height_and_converged(
    world: &mut CucumberWorld,
    step: &Step,
    min_height: u64,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    nodes_converged(
        world,
        &step.value,
        Some(min_height),
        max_diff_height,
        time_out_seconds,
    )
    .await
}

#[when(expr = "all nodes agree on LIB in {int} seconds")]
#[then(expr = "all nodes agree on LIB in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_all_nodes_agree_on_lib(
    world: &mut CucumberWorld,
    step: &Step,
    time_out_seconds: u64,
) -> StepResult {
    ensure_all_nodes_agree_on_lib(world, &step.value, time_out_seconds).await
}

#[when("I wait for all nodes to be synced to the chain")]
#[then("I wait for all nodes to be synced to the chain")]
async fn step_wait_for_all_nodes_to_be_synced_to_the_chain(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    wait_for_all_nodes_to_be_synced_to_chain(world, &step.value).await
}

#[when("I query cryptarchia info for all nodes")]
#[then("I query cryptarchia info for all nodes")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_query_cryptarchia_info_all_nodes(world: &mut CucumberWorld, step: &Step) {
    get_cryptarchia_info_all_nodes(world, &step.value).await;
}

#[then(expr = "I stop all nodes")]
async fn step_stop_all_nodes(world: &mut CucumberWorld) -> StepResult {
    let runtime_dir_by_node_name: Vec<(String, String)> = world
        .nodes_info
        .iter()
        .map(|(node_name, info)| (node_name.clone(), info.started_node.name.clone()))
        .collect();

    if world.snapshots.save.extensions.is_some() {
        prepare_all_wallets_snapshot(world).await?;
    }

    world.reset_wallet_scanner_after_current_iteration().await;
    world.zone.clear();
    stop_active_manual_cluster(world)?;

    if let Some(snapshot_name) = world.snapshots.save.node_state.take() {
        create_snapshots_all_nodes(world, &snapshot_name)?;
    }

    if let Some(snapshot_name) = world.snapshots.save.extensions.take() {
        save_prepared_all_wallets_snapshot(&snapshot_name, world)?;
    }

    for (node_name, _) in &runtime_dir_by_node_name {
        info!(target: TARGET, "Stopping node '{node_name}'");
    }
    world.nodes_info.clear();

    Ok(())
}

#[when(
    expr = "I send {int} transactions of {int} LGO each from wallet {string} to blend core zk key of node {string}"
)]
async fn step_send_multiple_transactions_to_blend_core_zk_key(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_transactions: usize,
    output_value: u64,
    sender_wallet_name: String,
    receiver_node_name: String,
) -> StepResult {
    let receiver_blend_zk_pk = blend_zk_pk_for_node(world, &receiver_node_name)?;
    let sender_node_name = world.resolve_wallet(&sender_wallet_name)?.node_name;
    let sender_node_client = world
        .nodes_info
        .get(&sender_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{sender_node_name}' not found in world state"),
        })?
        .started_node
        .client
        .clone();

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = wait_wallet_send_ready(
        world,
        &step.value,
        &sender_wallet_name,
        180,
        number_of_transactions as u64 * output_value,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    for _ in 0..number_of_transactions {
        let tx_hashes = create_and_submit_transaction_hashes_with_utxo_cache(
            world,
            &step.value,
            &sender_wallet_name,
            &[(receiver_blend_zk_pk, output_value)],
            Some(&best_node_info),
            Some(&mut available_utxos),
        )
        .await
        .inspect_err(|error| {
            warn!(target: TARGET, "Step `{}` error: {error}", step.value);
        })?;

        wait_for_transactions_inclusion(&sender_node_client, &tx_hashes, Duration::from_mins(2))
            .await
            .inspect_err(|error| {
                warn!(target: TARGET, "Step `{}` error: {error}", step.value);
            })?;

        info!(
            target: TARGET,
            "Sent and included normal transaction from `{sender_wallet_name}` to blend zk key of {receiver_node_name}, value: {output_value}, tx count: {}",
            tx_hashes.len(),
        );
    }

    Ok(())
}

fn blend_zk_pk_for_node(world: &CucumberWorld, node_name: &str) -> Result<ZkPublicKey, StepError> {
    let node_info = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' not found in world state"),
        })?;

    let user_config_path = node_info.runtime_dir.join(USER_CONFIG_FILE);
    let blend_zk_pk_hex = blend_core_zk_pk_from_node_yaml(&user_config_path)?;
    let blend_zk_pk = ZkPublicKey::from_bytes(&hex::decode(blend_zk_pk_hex)?)?;

    Ok(blend_zk_pk)
}

/// Wait for the node-local wallet API to expose a funded note for a Blend key.
///
/// Blend ZK keys are read from node configuration and are not scenario wallets,
/// so the wallet scanner does not currently track them. Keep this exception
/// node-local because the returned note is immediately consumed by that node's
/// SDP declaration endpoint.
async fn wait_for_blend_funded_note(
    world: &CucumberWorld,
    node_name: &str,
    blend_zk_pk: ZkPublicKey,
) -> Result<lb_core::mantle::NoteId, StepError> {
    let base_url = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' not found in world state"),
        })?
        .started_node
        .client
        .base_url()
        .clone();
    let timeout = Duration::from_secs(30);
    let started = Instant::now();
    let client = CommonHttpClient::new(None);

    loop {
        let last_error = match client
            .get_wallet_balance(base_url.clone(), blend_zk_pk, None)
            .await
        {
            Ok(wallet_balance) => {
                if let Some(note_id) = wallet_balance.notes.keys().next().copied() {
                    return Ok(note_id);
                }
                "wallet has no notes yet".to_owned()
            }
            Err(error) => error.to_string(),
        };

        if started.elapsed() >= timeout {
            return Err(StepError::Timeout {
                message: format!(
                    "Timed out waiting for a funded note on Blend ZK key of '{node_name}' via \
                     '{base_url}' (last error: {last_error})"
                ),
            });
        }

        sleep(Duration::from_millis(250)).await;
    }
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Required to be mutable by cucumber step function signature"
)]
#[expect(unused_variables, reason = "Cucumber step function signature")]
#[then(expr = "I declare node {string} as blend core node via the CLI binary")]
async fn step_run_blend_sdp_declaration_cli(
    world: &mut CucumberWorld,
    step: &Step,
    declarer_node_name: String,
) -> StepResult {
    let user_config_path = node_user_config_path(world, &declarer_node_name)?;
    let locator = blend_core_locator_from_node_yaml(&user_config_path)?;
    let blend_zk_pk = blend_zk_pk_for_node(world, &declarer_node_name)?;
    let service_note_id =
        wait_for_blend_funded_note(world, &declarer_node_name, blend_zk_pk).await?;
    let service_note_id_json =
        serde_json::to_string(&service_note_id).map_err(|error| StepError::LogicalError {
            message: format!("Failed to serialize service note ID: {error}"),
        })?;
    let service_note_id_hex = service_note_id_json.trim_matches('"').to_owned();

    let declarer_api_base_url = world
        .nodes_info
        .get(&declarer_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{declarer_node_name}' not found in world state"),
        })?
        .started_node
        .client
        .base_url()
        .clone();

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let output = tokio::process::Command::new("cargo")
        .current_dir(workspace_root)
        .arg("run")
        .arg("-p")
        .arg("logos-blockchain-tools")
        .arg("--bin")
        .arg("logos-blockchain-tools-api")
        .arg("--")
        .arg("sdp")
        .arg("post-blend-declaration")
        .arg("--user-config-path")
        .arg(user_config_path)
        .arg("--blend-addr")
        .arg(format!("{locator}"))
        .arg("--service-note-id")
        .arg(service_note_id_hex)
        .arg("--node-address")
        .arg(declarer_api_base_url.to_string())
        .output()
        .await?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(StepError::StepFail {
            message: format!(
                "Blend declaration CLI failed for node '{declarer_node_name}'\nstdout:\n{stdout}\nstderr:\n{stderr}"
            ),
        });
    }

    Ok(())
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Required to be mutable by cucumber step function signature"
)]
#[then(expr = "I declare node {string} as blend core node via the API")]
async fn step_run_blend_sdp_declaration_api(
    world: &mut CucumberWorld,
    step: &Step,
    declarer_node_name: String,
) -> StepResult {
    let user_config_path = node_user_config_path(world, &declarer_node_name)?;
    let locator = blend_core_locator_from_node_yaml(&user_config_path)?;
    let blend_zk_pk = blend_zk_pk_for_node(world, &declarer_node_name)?;
    let service_note_id =
        wait_for_blend_funded_note(world, &declarer_node_name, blend_zk_pk).await?;

    let declarer_node_client = world
        .nodes_info
        .get(&declarer_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{declarer_node_name}' not found in world state"),
        })?
        .started_node
        .client
        .clone();

    let declaration_id = declarer_node_client
        .join_blend_network(locator, service_note_id)
        .await
        .inspect_err(|error| {
            warn!(target: TARGET, "Step `{}` error: {error}", step.value);
        })?;

    info!(
        target: TARGET,
        "Node '{declarer_node_name}' joined blend core via API, declaration id: {declaration_id}"
    );

    Ok(())
}

fn node_user_config_path(world: &CucumberWorld, node_name: &str) -> Result<PathBuf, StepError> {
    let node_info = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' not found in world state"),
        })?;

    Ok(node_info.runtime_dir.join(USER_CONFIG_FILE))
}

#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: Address this at some point."
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Required to be mutable by cucumber step function signature"
)]
#[then(expr = "blend core SDP declaration for node {string} is included on node {string}")]
async fn step_verify_blend_sdp_declaration_included(
    world: &mut CucumberWorld,
    step: &Step,
    declarer_node_name: String,
    api_node_name: String,
) -> StepResult {
    let blend_zk_pk = blend_zk_pk_for_node(world, &declarer_node_name)?;
    let service_note_id =
        wait_for_blend_funded_note(world, &declarer_node_name, blend_zk_pk).await?;

    let step_timeout = Duration::from_secs(30);
    let start_time = Instant::now();
    loop {
        let declarations_result = world
            .nodes_info
            .get(&api_node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Node '{api_node_name}' not found in world state"),
            })?
            .started_node
            .client
            .get_sdp_declarations()
            .await;

        let declarations = match declarations_result {
            Ok(declarations) => declarations,
            Err(error) => {
                let error_message = error.to_string();
                if error_message.contains("404 Not Found") {
                    info!(
                        target: TARGET,
                        "Skipping declaration visibility assertion on '{api_node_name}' because testing SDP endpoint is unavailable: {error_message}",
                    );
                    return Ok(());
                }

                warn!(target: TARGET, "Step `{}` error: {error}", step.value);
                return Err(error.into());
            }
        };

        if declarations.values().any(|declaration| {
            declaration.service_note_id == service_note_id && declaration.zk_id == blend_zk_pk
        }) {
            info!(
                target: TARGET,
                "Blend declaration observed for node '{declarer_node_name}'"
            );
            break;
        }

        if start_time.elapsed() >= step_timeout {
            return Err(StepError::Timeout {
                message: format!(
                    "Timed out waiting for declaration submitted by '{declarer_node_name}' to appear on node '{api_node_name}'"
                ),
            });
        }

        sleep(Duration::from_millis(250)).await;
    }

    Ok(())
}
