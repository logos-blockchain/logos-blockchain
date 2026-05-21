use lb_common_http_client::{ApiBlock, Error as HttpClientError};
use lb_core::mantle::Utxo;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use thiserror::Error;

use super::{
    NodeHttpWalletChainSource, WalletChainSource, WalletId, WalletUtxos,
    chain::scan::{WalletChainScanner, WalletScanRequest, WalletSyncRequestError},
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
    #[error("wallet source scan did not return wallet `{wallet_id}`")]
    MissingWallet { wallet_id: WalletId },
    #[error(transparent)]
    Request(#[from] WalletSyncRequestError),
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
    let request = WalletScanRequest::new(wallet_id.clone(), [account.public_key()]);

    let wallet_utxos = scan_wallet_utxos_from_chain(source, &[request], genesis_utxos).await?;
    let available_utxos = wallet_utxos
        .get(wallet_id.as_str())
        .cloned()
        .ok_or(WalletFundingSourceFromChainError::MissingWallet { wallet_id })?;

    Ok(WalletFundingSource::new(account, available_utxos))
}

async fn scan_wallet_utxos_from_chain<BlockSource>(
    source: &mut BlockSource,
    requests: &[WalletScanRequest],
    genesis_utxos: &[Utxo],
) -> Result<WalletUtxos, WalletFundingSourceFromChainError<BlockSource::Error>>
where
    BlockSource: WalletChainSource,
{
    let mut scanner = WalletChainScanner::from_requests(requests)?;
    let mut tail_blocks = Vec::new();
    let mut current = source.tip();

    while let Some(block) = fetch_block(source, current).await? {
        current = block.header.parent_block;
        tail_blocks.push(block);
    }

    scanner.seed_genesis_utxos(genesis_utxos);
    tail_blocks.reverse();

    for block in tail_blocks {
        scan_block_transactions(&mut scanner, &block);
    }

    Ok(scanner.into_wallet_utxos())
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

fn scan_block_transactions(scanner: &mut WalletChainScanner, block: &ApiBlock) {
    for tx in &block.transactions {
        scanner.scan_transaction(tx);
    }
}
