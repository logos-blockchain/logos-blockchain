pub mod fee_horizon;
mod ids;
pub(crate) mod scanner;
mod tracked;
mod tracked_wallet;
mod transaction;

mod chain;
mod funding;
mod funding_from_chain;

pub use chain::{
    state::{
        GenesisNotes, TrackedWalletKeys, TrackedWalletKeysError, WalletUtxos, record_note_openings,
    },
    tracked_keys::{TrackedWalletKeysBySource, TrackedWalletKeysForSource},
};
pub use fee_horizon::{
    FeeProjectionError, TransactionFeeHorizon, TransactionFeePolicy, build_fee_horizon,
    project_storage_price,
};
pub(crate) use funding::{
    WalletFundingOutcome, WalletFundingPlan, WalletFundingUtxos, WalletSelectedInputs,
};
pub use funding::{
    WalletFundingPolicy, WalletFundingResources, WalletFundingSource, WalletInputSelectionStrategy,
    WalletReservedInputs,
};
pub use funding_from_chain::{funded_signed_spending_tx, funded_signed_tx};
pub use ids::{WalletChainSourceId, WalletId, wallet_id_for_chain_source};
pub use tracked::{
    RecordedWalletSubmission, TrackedWallets, TrackedWalletsState, WalletDiagnostics,
    WalletPendingStateDiagnostics, WalletUtxoSnapshotDiagnostics,
};
pub(crate) use tracked_wallet::TrackedWallet;
pub use tracked_wallet::{TrackedWalletState, WalletBalance, WalletOutputState, WalletStateView};
pub use transaction::{
    PreparedWalletTransaction, SignedWalletTransaction, WalletTransactionError,
    WalletTransactionIntent, prepare_wallet_transaction, transfer_proofs_for_funded_wallet_tx,
};
pub(crate) use transaction::{
    PreparedWalletTransactionWorkItem, estimate_workload_fee_requirements,
    extend_wallet_funding_inputs, finalize_prepared_wallet_transaction,
    prepare_wallet_transaction_work_item, random_nonce,
};
