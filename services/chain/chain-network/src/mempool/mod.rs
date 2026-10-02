use async_trait::async_trait;
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::mantle::{
    gas::MainnetGasProfile,
    ledger::verification_mode::StandardMode,
    traits::{Hashable, SignedMantleTx},
    transactions::{
        hash::{TxHash, TxHashPrefix},
        states::Preverified,
    },
};
use lb_cryptarchia_engine::Slot;
use lb_tx_service::TxsWithCommonPrefix;
use overwatch::DynError;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn add_transaction(&self, tx: Tx) -> Result<(), DynError>;

    async fn remove_transactions(&self, ids: &[TxHash]) -> Result<(), DynError>;

    /// Every transaction the mempool holds.
    async fn pending_transactions(&self) -> Result<Vec<Tx>, DynError>;

    /// The local transactions a single proposal reference could mean.
    ///
    /// A reference is only the leading hash bytes, so in principle several
    /// mempool transactions could answer to it. The stream is unbounded and
    /// unordered: what a non-unique match means is a consensus question, so it
    /// is the caller's to decide.
    async fn get_transactions_by_prefix(
        &self,
        prefix: TxHashPrefix,
    ) -> Result<TxsWithCommonPrefix<Tx>, DynError>;
}

/// Removes from the mempool the transactions that no block at `slot` on top of
/// the tip could include under the era of `slot`, and returns how many. Run
/// when an era starts, whose rules may reject what the era before admitted.
pub async fn remove_inapplicable_transactions<Cryptarchia, Mempool>(
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    mempool: &Mempool,
    slot: Slot,
) -> Result<usize, DynError>
where
    Cryptarchia: CryptarchiaServiceData<Tx: SignedMantleTx<Preverified, StandardMode> + Send>,
    Mempool: MempoolAdapter<Cryptarchia::Tx>,
{
    let tip = cryptarchia.info().await?.cryptarchia_info.tip;
    let state = cryptarchia
        .get_ledger_state(tip)
        .await?
        .ok_or_else(|| format!("No ledger state at the tip {tip:?}"))?;
    let eras = cryptarchia.get_ledger_eras().await?;
    let pending = mempool
        .pending_transactions()
        .await?
        .iter()
        .map(|tx| tx.signed_ops().clone())
        .collect::<Vec<_>>();
    let inapplicable = state
        .inapplicable_transactions::<_, MainnetGasProfile>(slot, &eras, &pending)
        .into_iter()
        .map(Hashable::hash)
        .collect::<Vec<_>>();
    if !inapplicable.is_empty() {
        mempool.remove_transactions(&inapplicable).await?;
    }
    Ok(inapplicable.len())
}
