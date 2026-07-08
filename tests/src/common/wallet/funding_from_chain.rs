use std::collections::{HashMap, HashSet};

use lb_common_http_client::{ApiBlock, Error as HttpClientError};
use lb_core::mantle::{
    Op, OpProof, SignedMantleTx, Transaction as _, TxHash, Utxo,
    gas::MainnetGasConstants,
    ops::channel::{ChannelId, ChannelKeyIndex},
    transactions::{GasPrices, MantleTxBuilder, MantleTxContext, MantleTxGasContext},
};
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use thiserror::Error;

use super::{
    NodeHttpWalletChainSource, WalletChainSource, WalletId,
    chain::state::{TrackedWalletKeys, TrackedWalletKeysError, WalletChainState},
};
use crate::common::wallet::{
    WalletFundingSource, chain::state::WalletUtxos, fund_builder_from_wallet_source,
    transfer_proofs_for_funded_wallet_tx,
};

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

/// Build, fund, and sign a single-op transaction.
///
/// The op fee is paid from the funding wallet (synced from chain), whose
/// trailing transfer op gets its own proof. The op proof is built via
/// `op_proof` from the funded transaction hash. `withdraw_thresholds` is
/// needed by the gas-size predictor for `ChannelWithdraw` ops. Returns the
/// signed transaction and its fee at genesis gas prices.
#[expect(
    clippy::implicit_hasher,
    reason = "The thresholds map is forwarded to MantleTxGasContext, which requires the default hasher."
)]
pub async fn funded_signed_tx(
    node: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    funding_account: &WalletAccount,
    withdraw_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    op: Op,
    op_proof: impl FnOnce(TxHash) -> OpProof,
) -> (SignedMantleTx, u64) {
    let funding_source =
        current_wallet_funding_source(node, genesis_utxos, funding_account.clone())
            .await
            .expect("funding wallet source should sync from chain");

    let tx_context = MantleTxContext {
        gas_context: MantleTxGasContext::new(
            withdraw_thresholds,
            HashMap::new(),
            GasPrices::default(),
        ),
        leader_reward_amount: 0,
    };
    let tx_builder = MantleTxBuilder::new()
        .push_op(op)
        .expect("op should fit op bounds");

    let funded_builder = fund_builder_from_wallet_source(&funding_source, &tx_builder, &tx_context)
        .expect("funding transaction should succeed");
    let fee = funded_builder
        .gas_cost::<MainnetGasConstants>(&tx_context)
        .expect("funded tx gas cost should calculate")
        .into_inner();

    let mantle_tx = funded_builder.build().expect("funded builder should build");
    let tx_hash = mantle_tx.hash();

    let mut proofs = vec![op_proof(tx_hash)];
    proofs.extend(
        transfer_proofs_for_funded_wallet_tx(&mantle_tx, &funding_account.secret_key)
            .expect("transfer proofs should build"),
    );

    let signed_tx =
        SignedMantleTx::new(mantle_tx, proofs).expect("funded transaction should be valid");

    (signed_tx, fee)
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
