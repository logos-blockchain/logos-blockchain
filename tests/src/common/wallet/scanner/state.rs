use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use lb_core::header::HeaderId;

/// Shared scanner status snapshot across scanner tasks and cucumber waits.
pub type SharedWalletScannerState = Arc<Mutex<WalletScannerState>>;

#[derive(Debug, Default)]
/// Status for all wallet scanner fork groups.
pub struct WalletScannerState {
    /// Scanner state by fork group id.
    pub groups: BTreeMap<String, ForkGroupScannerState>,
}

#[derive(Debug, Clone)]
/// Status for one fork-group scanner task.
pub struct ForkGroupScannerState {
    /// Fork group id; empty string represents the ungrouped cluster.
    pub group_id: String,
    /// Wallet name used for best-node selection in this group.
    pub representative_wallet: String,
    /// Current node selected as scanner source.
    pub source_node: Option<String>,
    /// Highest scanner-applied synthetic height.
    pub applied_height: u64,
    /// Highest scanner-applied consensus slot.
    pub applied_slot: Option<u64>,
    /// Highest scanner-applied block tip.
    pub applied_tip: Option<HeaderId>,
    /// Latest selected target height.
    pub target_height: Option<u64>,
    /// Latest selected target slot.
    pub target_slot: Option<u64>,
    /// Latest selected target tip.
    pub target_tip: Option<HeaderId>,
    /// Current scanner lifecycle status.
    pub status: ScannerStatus,
    /// Last scanner error, if any.
    pub last_error: Option<String>,
    /// Number of unique transaction hashes observed by the scanner.
    pub observed_tx_hashes: usize,
    /// Number of wallets tracked by this group.
    pub wallet_count: usize,
}

impl ForkGroupScannerState {
    #[must_use]
    /// Create initial status for one fork-group scanner.
    pub const fn new(group_id: String, representative_wallet: String, wallet_count: usize) -> Self {
        Self {
            group_id,
            representative_wallet,
            source_node: None,
            applied_height: 0,
            applied_slot: None,
            applied_tip: None,
            target_height: None,
            target_slot: None,
            target_tip: None,
            status: ScannerStatus::Starting,
            last_error: None,
            observed_tx_hashes: 0,
            wallet_count,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// High-level lifecycle state for a scanner group.
pub enum ScannerStatus {
    /// Scanner task has been configured but has not scanned yet.
    #[default]
    Starting,
    /// Scanner is applying historical blocks.
    Backfilling,
    /// Scanner is following new blocks.
    Tailing,
    /// Scanner is selecting or waiting for a source node.
    WaitingForSource,
    /// Scanner hit an error and will retry.
    Error,
}
