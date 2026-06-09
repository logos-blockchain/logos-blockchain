use std::collections::HashSet;

use lb_common_http_client::{ApiBlock, Error as HttpClientError};
use lb_core::mantle::{TxHash, Utxo};
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use thiserror::Error;

use super::{
    NodeHttpWalletChainSource, WalletChainSource, WalletId, WalletUtxos,
    chain::state::{TrackedWalletKeys, TrackedWalletKeysError, WalletChainState},
};
use crate::common::wallet::WalletFundingSource;

#[derive(Debug, Error)]
pub enum DirectWalletSourceError {
    #[error(transparent)]
    Source(#[from] HttpClientError),
    #[error(transparent)]
    Funding(#[from] WalletFundingSourceFromChainError<HttpClientError>),
}

#[derive(Debug, Error)]
pub enum WalletFundingSourceFromChainError<FetchError> {
    #[error("wallet source sync did not return wallet `{wallet_id}`")]
    MissingWallet { wallet_id: WalletId },
    #[error(transparent)]
    TrackedKeys(#[from] TrackedWalletKeysError),
    #[error(transparent)]
    FetchBlock(FetchError),
}

pub async fn current_wallet_funding_source(
    client: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    account: WalletAccount,
) -> Result<WalletFundingSource, DirectWalletSourceError> {
    let mut source =
        NodeHttpWalletChainSource::from_client("direct-wallet-source", client.clone()).await?;

    Ok(wallet_funding_source_from_chain(&mut source, genesis_utxos, account).await?)
}

pub async fn wallet_funding_source_from_chain<BlockSource>(
    source: &mut BlockSource,
    genesis_utxos: &[Utxo],
    account: WalletAccount,
) -> Result<WalletFundingSource, WalletFundingSourceFromChainError<BlockSource::Error>>
where
    BlockSource: WalletChainSource,
{
    let wallet_id = WalletId::from(account.label.clone());
    let tracked_wallet = TrackedWalletKeys::new(wallet_id.clone(), [account.public_key()]);

    let (wallet_utxos, _, _) =
        wallet_utxos_from_chain(source, &[tracked_wallet], genesis_utxos).await?;
    let available_utxos = wallet_utxos
        .get(wallet_id.as_str())
        .cloned()
        .ok_or(WalletFundingSourceFromChainError::MissingWallet { wallet_id })?;

    Ok(WalletFundingSource::new(account, available_utxos))
}

pub async fn wallet_utxos_from_chain<BlockSource>(
    source: &mut BlockSource,
    tracked_wallets: &[TrackedWalletKeys],
    genesis_utxos: &[Utxo],
) -> Result<
    (WalletUtxos, HashSet<TxHash>, usize),
    WalletFundingSourceFromChainError<BlockSource::Error>,
>
where
    BlockSource: WalletChainSource,
{
    let mut chain_state = WalletChainState::from_tracked_wallets(tracked_wallets)?;
    let mut tail_blocks = Vec::new();
    let mut current = source.tip();

    while let Some(block) = fetch_block(source, current).await? {
        current = block.header.parent_block;
        tail_blocks.push(block);
    }

    chain_state.seed_genesis_utxos(genesis_utxos);
    tail_blocks.reverse();
    let tail_blocks_len = tail_blocks.len();

    let mut transactions_hashes = HashSet::new();
    for block in tail_blocks {
        apply_block_transactions(&mut chain_state, &block);
        transactions_hashes.extend(block.transactions.iter().map(lb_node::Transaction::hash));
    }

    Ok((
        chain_state.into_wallet_utxos(),
        transactions_hashes,
        tail_blocks_len,
    ))
}

async fn fetch_block<BlockSource>(
    source: &mut BlockSource,
    header_id: lb_core::header::HeaderId,
) -> Result<Option<ApiBlock>, WalletFundingSourceFromChainError<BlockSource::Error>>
where
    BlockSource: WalletChainSource,
{
    source
        .fetch_block(header_id)
        .await
        .map_err(WalletFundingSourceFromChainError::FetchBlock)
}

fn apply_block_transactions(chain_state: &mut WalletChainState, block: &ApiBlock) {
    for tx in &block.transactions {
        chain_state.apply_transaction(tx);
    }
}
