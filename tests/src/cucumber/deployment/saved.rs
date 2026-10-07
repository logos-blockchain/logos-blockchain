//! Temporary saved-configuration integration with the Cucumber suite.
//!
//! Selection, setup and restrictions live here. Steps keep only guards where
//! they could otherwise change fixed inputs or derive a missing wallet.
//! Native files are parsed and launched by the Logos adapter's `saved` module.

use std::path::{Path, PathBuf};

use lb_testing_framework::{
    PreparedConfigBundle, SavedDeployment, SavedLogosEnv, configs::deployment::SdpFundingConfig,
};
use testing_framework_app::AppDeployer;
use testing_framework_core::scenario::{ClusterStartMode, DynError};

use super::{CucumberClusterApp, DeploymentInput, LocalDeployment};
use crate::cucumber::{
    error::{StepError, StepResult},
    world::{
        ClusterState, CucumberWorld, DeployerKind, GenesisTokens, ManualClusterKind,
        ManualClusterSpec,
    },
};

pub fn validate_deployer(deployer: DeployerKind) {
    assert!(
        prepared_config_path().is_none() || deployer == DeployerKind::Local,
        "CUCUMBER_PREPARED_CONFIG requires the local deployer"
    );
}

pub fn config_for_scenario(scenario_name: &str) -> Option<PathBuf> {
    let path = prepared_config_path()?;
    PreparedConfigBundle::load(&path)
        .and_then(|bundle| bundle.require_scenario(scenario_name))
        .unwrap_or_else(|error| panic!("prepared configuration {}: {error:#}", path.display()));
    Some(path)
}

fn prepared_config_path() -> Option<PathBuf> {
    std::env::var_os("CUCUMBER_PREPARED_CONFIG").map(PathBuf::from)
}

pub async fn install_cluster(
    world: &mut CucumberWorld,
    path: &Path,
    spec: ManualClusterSpec,
) -> StepResult {
    validate_prepared_cluster(world, spec)?;
    let deployment = SavedDeployment::load(path, spec.capacity)?;
    world.wallet_registry.wallet_accounts = deployment
        .wallet_accounts()
        .iter()
        .map(|(index, account)| (*index, account.clone()))
        .collect();
    world.chain.genesis_block_utxos = deployment.genesis_utxos().to_vec();
    let app = world
        .cluster
        .implementation
        .deploy_input(DeploymentInput::Saved(deployment))
        .await?;
    world.cluster.install_local(app)?;
    world.cluster.manual_cluster_spec = Some(spec);
    Ok(())
}

fn validate_prepared_cluster(
    world: &CucumberWorld,
    spec: ManualClusterSpec,
) -> Result<(), StepError> {
    validate_genesis_wallets(&world.cluster, &world.chain.genesis_tokens)?;
    if !matches!(spec.kind, ManualClusterKind::Generated)
        || world.lifecycle.genesis_time.is_some()
        || world
            .wallet_registry
            .fee_state
            .sponsored_genesis_account
            .is_some()
        || world
            .cluster
            .blend_core_nodes
            .is_some_and(|count| count != 0)
        || world.cluster.sdp_funding_config != SdpFundingConfig::default()
        || world.tokio_console_profile_enabled()
    {
        return Err(StepError::InvalidArgument {
            message: "saved configuration cannot be regenerated for custom genesis, \
                      sponsored fee accounts, Blend providers, devnet or profiling settings"
                .into(),
        });
    }
    Ok(())
}

/// Saved bundles satisfy funding requests by checking the original notes.
/// The generated path records the request for its deployment builder.
pub fn validate_genesis_wallets(cluster: &ClusterState, wallets: &[GenesisTokens]) -> StepResult {
    let Some(path) = &cluster.prepared_config else {
        return Ok(());
    };
    let validate = || -> Result<(), DynError> {
        let bundle = PreparedConfigBundle::load(path)?;
        for wallet in wallets {
            bundle.require_wallet_funding(
                wallet.account_index,
                wallet.token_count,
                wallet.token_amount,
            )?;
        }
        Ok(())
    };
    validate().map_err(|error| StepError::InvalidArgument {
        message: format!("prepared configuration {}: {error:#}", path.display()),
    })
}

pub fn require_generated_config(cluster: &ClusterState, operation: &str) -> StepResult {
    if let Some(path) = &cluster.prepared_config {
        return Err(StepError::InvalidArgument {
            message: format!(
                "{operation} requires generated configuration; a saved bundle was selected at {}",
                path.display()
            ),
        });
    }
    Ok(())
}

pub fn require_wallet_account(world: &CucumberWorld, account_index: usize) -> StepResult {
    if world.cluster.prepared_config.is_some()
        && !world
            .wallet_registry
            .wallet_accounts
            .contains_key(&account_index)
    {
        return Err(StepError::InvalidArgument {
            message: format!("saved configuration has no wallet account {account_index}"),
        });
    }
    Ok(())
}

pub(super) async fn deploy_logos(deployment: SavedDeployment) -> Result<LocalDeployment, DynError> {
    let inputs = deployment.shared_deployment().clone();
    let app = SavedLogosEnv::prepare_app(deployment)
        .await?
        .with_start_mode(ClusterStartMode::OnDemand);
    AppDeployer::new()
        .deploy(CucumberClusterApp { app, inputs })
        .await
}
