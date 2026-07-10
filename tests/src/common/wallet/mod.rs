mod ids;
pub(crate) mod scanner;
mod tracked;
mod tracked_wallet;
mod transaction;

mod chain;
mod funding;
mod funding_from_chain;

pub use chain::{
    source::{NodeHttpWalletChainSource, WalletChainSource},
    state::{
        TrackedWalletKeys, TrackedWalletKeysError, WalletObservedOutput, WalletObservedSpend,
        WalletUtxos,
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
    funded_signed_tx, wallet_funding_source_from_chain, wallet_utxos_from_chain,
};
pub use ids::{WalletChainSourceId, WalletId, wallet_id_for_chain_source};
pub use tracked::{
    RecordedWalletSubmission, TrackedWallets, TrackedWalletsState, WalletDiagnostics,
    WalletPendingStateDiagnostics, WalletUtxoSnapshotDiagnostics,
};
pub(crate) use tracked_wallet::TrackedWallet;
pub use tracked_wallet::{TrackedWalletState, WalletBalance, WalletOutputState, WalletStateView};
pub use transaction::{
    PreparedWalletTransaction, SignedWalletTransaction, WalletTransactionError,
    WalletTransactionIntent, fund_builder_from_wallet_source, prepare_wallet_transaction,
    transfer_proofs_for_funded_wallet_tx, wallet_state_from_utxos,
};
pub(crate) use transaction::{
    PreparedWalletTransactionWorkItem, finalize_prepared_wallet_transaction,
    prepare_wallet_transaction_work_item,
};
