use std::{collections::BTreeMap, sync::Arc, time::Duration};

use lb_core::{header::HeaderId, mantle::Utxo};
use lb_testing_framework::NodeHttpClient;

use super::state::SharedWalletScannerState;
use crate::{
    common::wallet::{TrackedWalletKeys, WalletUtxos},
    cucumber::{
        error::StepError,
        wallet::best_node::{BestNodeInfo, display_group_key},
        world::{SharedObservedTransactionHashes, SharedTrackedWallets},
    },
};

/// Shared callback used by scanner tasks to select the current best source
/// node.
pub type SharedBestNodeSelector = Arc<
    dyn for<'a> Fn(
            String,
            Vec<String>,
            BTreeMap<String, NodeHttpClient>,
            BTreeMap<String, String>,
            String,
            &'a mut String,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<BestNodeInfo, StepError>> + Send + 'a>,
        > + Send
        + Sync,
>;

/// Maximum trailing block window used when restoring scanner state from a
/// wallet snapshot.
pub const MAX_SCANNER_SNAPSHOT_RESCAN_BLOCKS: u64 = 100;
/// Default trailing block window used when restoring scanner state from a
/// wallet snapshot.
pub const DEFAULT_SCANNER_SNAPSHOT_RESCAN_BLOCKS: u64 = 50;

#[derive(Clone, Debug, Default)]
/// Initial state used by a wallet scanner task.
pub enum ScannerSeed {
    /// Start from genesis UTXOs and scan forward.
    #[default]
    Genesis,
    /// Start from a restored wallet snapshot and verify a trailing slot window
    /// before scanning blocks after the snapshot tip.
    Snapshot {
        /// UTXOs restored for wallets tracked by this scanner.
        wallet_utxos: WalletUtxos,
        /// Snapshot chain tip.
        tip: HeaderId,
        /// Snapshot chain height.
        height: u64,
        /// Snapshot chain slot.
        slot: u64,
        /// Node names whose header-height cache should receive the seed tip.
        source_node_names: Vec<String>,
        /// Number of trailing slots to verify before tailing from the seed tip.
        rescan_blocks: u64,
    },
}

impl ScannerSeed {
    #[must_use]
    /// Return a copy of this seed containing only UTXOs for `wallet_keys`.
    pub fn filtered_for_wallets(&self, wallet_keys: &[TrackedWalletKeys]) -> Self {
        let Self::Snapshot {
            wallet_utxos,
            tip,
            height,
            slot,
            source_node_names,
            rescan_blocks,
        } = self
        else {
            return Self::Genesis;
        };

        let wallet_ids = wallet_keys
            .iter()
            .map(|wallet| wallet.wallet_id().clone())
            .collect::<std::collections::HashSet<_>>();
        let wallet_utxos = wallet_utxos
            .iter()
            .filter(|(wallet_id, _)| wallet_ids.contains(*wallet_id))
            .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos.clone()))
            .collect();

        Self::Snapshot {
            wallet_utxos,
            tip: *tip,
            height: *height,
            slot: *slot,
            source_node_names: source_node_names.clone(),
            rescan_blocks: (*rescan_blocks).min(MAX_SCANNER_SNAPSHOT_RESCAN_BLOCKS),
        }
    }
}

#[derive(Clone)]
/// Runtime configuration for one fork-group wallet scanner task.
pub struct ForkGroupScannerConfig {
    /// Fork group id; empty string represents the ungrouped cluster.
    pub group_id: String,
    /// Wallet name used for best-node selection logging and grouping.
    pub representative_wallet_name: String,
    /// Wallet public keys tracked by this scanner.
    pub wallet_keys: Vec<TrackedWalletKeys>,
    /// HTTP clients for nodes in this scanner's group.
    pub node_clients: BTreeMap<String, NodeHttpClient>,
    /// Mapping from wallet name to its connected node name.
    pub wallet_to_node: BTreeMap<String, String>,
    /// Node names that belong to this scanner's fork group.
    pub group_nodes: Vec<String>,
    /// Shared tracked wallet read model updated by scanner observations.
    pub wallets: SharedTrackedWallets,
    /// Shared set of transaction hashes observed in scanned blocks.
    pub observed_transaction_hashes: SharedObservedTransactionHashes,
    /// Shared scanner status used by cucumber waits and diagnostics.
    pub scanner_state: SharedWalletScannerState,
    /// Poll interval between scanner iterations.
    pub poll_interval: Duration,
    /// Number of applied blocks between intermediate wallet-state publications.
    pub range_batch_size: u64,
    /// Genesis UTXOs used to initialize scanner accounting.
    pub genesis_utxos: Vec<Utxo>,
    /// Optional restored snapshot seed for scanner startup.
    pub seed: ScannerSeed,
    /// Best-node selector used on each scanner iteration.
    pub best_node_selector: SharedBestNodeSelector,
}

impl ForkGroupScannerConfig {
    /// Helper to get the node client for a given node name and group id.
    pub fn node_client_for_group(&self, node_name: &str) -> Result<&NodeHttpClient, StepError> {
        self.node_clients
            .get(node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "wallet scanner selected unknown node '{}' for group '{}'",
                    node_name,
                    display_group_key(&self.group_id)
                ),
            })
    }
}
