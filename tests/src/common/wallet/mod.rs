mod ids;
mod tracked;
mod tracked_wallet;
mod transaction;

mod chain;
mod funding;
mod funding_from_chain;

pub use chain::{
    feed::{
        WalletBlockFeedTracker, WalletBlockFeedTrackerError, WalletFeedTrackingBatch,
        WalletFeedTrackingResult, WalletObservedBlock,
    },
    source::{NodeHttpWalletChainSource, WalletChainSource},
    state::{TrackedWalletKeys, TrackedWalletKeysError, WalletObservedOutput, WalletObservedSpend},
    sync::{
        WalletSyncResult, WalletSyncResults, WalletSyncTrackedKeysBySource,
        WalletSyncTrackedKeysError, WalletSyncTrackedKeysForSource, WalletSyncedBlock,
        WalletSyncedOutput, WalletSyncedSpend, WalletUtxos,
    },
    tracked_keys::{TrackedWalletKeysBySource, TrackedWalletKeysForSource},
};
pub use funding::{
    WalletFundedTransfer, WalletFundingPolicy, WalletFundingResources, WalletFundingSource,
    WalletReservedInputs, build_wallet_funded_transfer,
};
pub(crate) use funding::{
    WalletFundingOutcome, WalletFundingPlan, WalletFundingUtxos, WalletSelectedInputs,
};
pub use funding_from_chain::{
    DirectWalletSourceError, WalletFundingSourceFromChainError, current_wallet_funding_source,
    wallet_funding_source_from_chain, wallet_utxos_from_chain,
};
pub use ids::{WalletChainSourceId, WalletId, wallet_id_for_chain_source};
pub use tracked::{
    RecordedWalletSubmission, TrackedWallets, WalletDiagnostics, WalletPendingStateDiagnostics,
    WalletUtxoSnapshotDiagnostics,
};
pub(crate) use tracked_wallet::TrackedWallet;
pub use tracked_wallet::{WalletBalance, WalletOutputState, WalletStateView};
pub use transaction::{
    PreparedWalletTransaction, SignedWalletTransaction, WalletTransactionError,
    WalletTransactionIntent, fund_builder_from_wallet_source, prepare_wallet_transaction,
    wallet_state_from_utxos,
};
