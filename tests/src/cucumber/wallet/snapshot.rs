use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use lb_testing_framework::{LbcEnv, configs::wallet::WalletAccount};
use serde::{Deserialize, Serialize};
use testing_framework_core::scenario::{
    DynError, SnapshotArtifact, SnapshotContext, SnapshotExtension, SnapshotSpec, SnapshotStore,
};

use crate::{
    common::wallet::{TrackedWallets, TrackedWalletsState},
    cucumber::{
        defaults::snapshots_root_dir,
        error::{StepError, StepResult},
        world::{CucumberWorld, WalletInfoMap},
    },
};

/// Snapshot extension id used for Cucumber wallet state.
pub const WALLET_SNAPSHOT_EXTENSION_ID: &str = "wallet";

/// Serializable Cucumber wallet state.
///
/// This is test-framework state, not node state. It contains wallet aliases,
/// account keys, and the tracked wallet read model so wallet checks can
/// continue from the snapshot point instead of scanning from genesis again.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletSnapshot {
    wallet_info: WalletInfoMap,
    wallet_accounts: HashMap<usize, WalletAccount>,
    tracked_wallets: TrackedWalletsState,
}

impl WalletSnapshot {
    fn from_world(world: &CucumberWorld) -> Result<Self, StepError> {
        Ok(Self {
            wallet_info: world.wallet_info.clone(),
            wallet_accounts: world.wallet_accounts.clone(),
            tracked_wallets: world.with_wallets(TrackedWallets::to_state)?,
        })
    }

    fn is_empty(&self) -> bool {
        self.wallet_info.is_empty()
            && self.wallet_accounts.is_empty()
            && self.tracked_wallets.is_empty()
    }

    fn apply(self, world: &mut CucumberWorld) -> StepResult {
        world.wallet_info = self.wallet_info;
        world.wallet_accounts = self.wallet_accounts;
        world.with_wallets_mut(|wallets| wallets.replace_from_state(self.tracked_wallets))?;
        Ok(())
    }

    fn into_artifact(self) -> Result<SnapshotArtifact, DynError> {
        let wallet_count = self.wallet_info.len();
        let account_count = self.wallet_accounts.len();

        Ok(SnapshotArtifact::new(
            1,
            serde_json::json!({
                "wallet_count": wallet_count,
                "account_count": account_count,
            }),
            serde_json::to_value(self)?,
        ))
    }

    fn from_artifact(artifact: &SnapshotArtifact) -> Result<Self, DynError> {
        Ok(serde_json::from_value(artifact.payload.clone())?)
    }
}

/// Snapshot extension that saves and loads Cucumber wallet state.
///
/// The extension is independent of node-state restore. Tests decide lifecycle
/// ordering; the safe Cucumber flow saves wallet state after nodes are stopped
/// and restores wallet state before starting nodes from node state.
#[derive(Clone)]
pub struct WalletSnapshotExtension {
    snapshot: Arc<Mutex<Option<WalletSnapshot>>>,
}

impl WalletSnapshotExtension {
    /// Create an extension instance backed by an already captured wallet
    /// snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: WalletSnapshot) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(Some(snapshot))),
        }
    }

    fn artifact(&self) -> Result<Option<SnapshotArtifact>, DynError> {
        let snapshot = self
            .snapshot
            .lock()
            .map_err(|_| "wallet snapshot extension state lock is poisoned")?
            .clone();

        snapshot.map(WalletSnapshot::into_artifact).transpose()
    }

    fn replace_from_artifact(&self, artifact: &SnapshotArtifact) -> Result<(), DynError> {
        let snapshot = WalletSnapshot::from_artifact(artifact)?;
        *self
            .snapshot
            .lock()
            .map_err(|_| "wallet snapshot extension state lock is poisoned")? = Some(snapshot);
        Ok(())
    }
}

#[async_trait]
impl SnapshotExtension<LbcEnv> for WalletSnapshotExtension {
    fn id(&self) -> &'static str {
        WALLET_SNAPSHOT_EXTENSION_ID
    }

    async fn save(
        &self,
        _context: &SnapshotContext<LbcEnv>,
        _spec: &SnapshotSpec,
    ) -> Result<Option<SnapshotArtifact>, DynError> {
        self.artifact()
    }

    async fn load(
        &self,
        _context: &SnapshotContext<LbcEnv>,
        artifact: &SnapshotArtifact,
        _spec: &SnapshotSpec,
    ) -> Result<(), DynError> {
        self.replace_from_artifact(artifact)
    }
}

/// Save Cucumber wallet state as a wallet extension artifact in
/// `snapshot_name`.
pub fn save_wallet_snapshot(snapshot_name: &str, world: &CucumberWorld) -> StepResult {
    let artifact = WalletSnapshot::from_world(world)?
        .into_artifact()
        .map_err(|e| snapshot_error(&e))?;

    SnapshotStore::new(snapshots_root_dir())
        .save_extension_artifact(snapshot_name, WALLET_SNAPSHOT_EXTENSION_ID, artifact)
        .map(|_| ())
        .map_err(|e| snapshot_error(&e))
}

/// Save Cucumber wallet state only when the world currently has wallet state.
pub fn save_wallet_snapshot_if_present(snapshot_name: &str, world: &CucumberWorld) -> StepResult {
    let snapshot = WalletSnapshot::from_world(world)?;
    if snapshot.is_empty() {
        return Ok(());
    }

    let artifact = snapshot.into_artifact().map_err(|e| snapshot_error(&e))?;

    SnapshotStore::new(snapshots_root_dir())
        .save_extension_artifact(snapshot_name, WALLET_SNAPSHOT_EXTENSION_ID, artifact)
        .map(|_| ())
        .map_err(|e| snapshot_error(&e))
}

/// Restore Cucumber wallet state from the wallet extension artifact.
///
/// This clears the current wallet test-framework state before applying the
/// artifact. It does not touch node runtime directories or start/stop nodes.
pub fn restore_wallet_snapshot(snapshot_name: &str, world: &mut CucumberWorld) -> StepResult {
    let artifact = SnapshotStore::new(snapshots_root_dir())
        .load_extension_artifact(snapshot_name, WALLET_SNAPSHOT_EXTENSION_ID)
        .map_err(|e| snapshot_error(&e))?;

    let snapshot = WalletSnapshot::from_artifact(&artifact).map_err(|e| snapshot_error(&e))?;
    clear_wallet_snapshot_state(world)?;

    snapshot.apply(world)?;

    world.reset_wallet_block_feed();
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

/// Restore any wallet state stored in `snapshot_name`.
///
/// Missing wallet state is allowed here because generic snapshot restore is
/// extension-aware but not extension-specific. A malformed wallet artifact
/// still fails the step.
pub fn restore_wallet_snapshot_if_present(
    snapshot_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    let artifact = SnapshotStore::new(snapshots_root_dir())
        .read_manifest(snapshot_name)
        .map_err(|e| snapshot_error(&e))?
        .extensions
        .get(WALLET_SNAPSHOT_EXTENSION_ID)
        .cloned();

    let Some(artifact) = artifact else {
        return Ok(());
    };

    let snapshot = WalletSnapshot::from_artifact(&artifact).map_err(|e| snapshot_error(&e))?;
    clear_wallet_snapshot_state(world)?;

    snapshot.apply(world)?;

    world.reset_wallet_block_feed();
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

fn clear_wallet_snapshot_state(world: &mut CucumberWorld) -> StepResult {
    world.wallet_info.clear();
    world.wallet_accounts.clear();
    world.with_wallets_mut(|wallets| {
        wallets.replace_from_state(TrackedWalletsState::default());
    })?;

    world.reset_wallet_block_feed();
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

fn snapshot_error(source: &DynError) -> StepError {
    StepError::LogicalError {
        message: source.to_string(),
    }
}
