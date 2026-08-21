use std::collections::HashMap;

use lb_common_http_client::Error as HttpClientError;
use lb_core::mantle::{
    Op, OpProof, SignedOps, TxHash, Utxo,
    gas::MainnetGasProfile,
    ledger::verification_mode::StandardMode,
    ops::channel::{ChannelId, ChannelKeyIndex},
    traits::Hashable as _,
    transactions::{
        GasPrices, MantleTxBuilder, OpProofs,
        states::Unverified,
        tx_list::ops::{OpsContext, OpsGasContext},
    },
};
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use thiserror::Error;

use super::{
    WalletId,
    chain::state::{TrackedWalletKeys, TrackedWalletKeysError},
    scanner::accounting::ScannerAccounting,
};
use crate::common::wallet::{
    WalletFundingSource, chain::state::WalletUtxos, fund_builder_from_wallet_source,
    transfer_proofs_for_funded_wallet_tx,
};

#[derive(Debug, Error)]
enum DirectWalletSourceError {
    #[error(transparent)]
    Source(#[from] HttpClientError),
    #[error("wallet source sync did not return wallet `{wallet_id}`")]
    MissingWallet { wallet_id: WalletId },
    #[error(transparent)]
    TrackedKeys(#[from] TrackedWalletKeysError),
}

/// Build, fund, and sign a single-op transaction.
///
/// The op fee is paid from the funding wallet (synced from chain), whose
/// trailing transfer op gets its own proof. The op proof is built via
/// `op_proof` from the funded transaction hash. `transfer_thresholds` is
/// needed by the gas-size predictor for `ChannelWithdraw` and
/// `ChannelTransfer` ops. Returns the signed transaction and its fee at
/// genesis gas prices.
#[expect(
    clippy::implicit_hasher,
    reason = "The thresholds map is forwarded to MantleTxGasContext, which requires the default hasher."
)]
pub async fn funded_signed_tx(
    node: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    funding_account: &WalletAccount,
    transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    op: Op,
    op_proof: impl FnOnce(TxHash) -> OpProof,
) -> (SignedOps<Unverified, StandardMode>, u64) {
    let funding_source =
        current_wallet_funding_source(node, genesis_utxos, funding_account.clone())
            .await
            .expect("funding wallet source should sync from chain");

    let tx_context = OpsContext {
        gas_context: OpsGasContext::new(transfer_thresholds, HashMap::new(), GasPrices::default()),
        leader_reward_amount: 0,
    };
    let tx_builder = MantleTxBuilder::new()
        .push_op(op)
        .expect("op should fit op bounds");

    let funded_builder = fund_builder_from_wallet_source(&funding_source, &tx_builder, &tx_context)
        .expect("funding transaction should succeed");
    let fee = funded_builder
        .minimum_gas_cost::<MainnetGasProfile>(&tx_context)
        .expect("funded tx gas cost should calculate")
        .into_inner();

    let mantle_tx = funded_builder.build().expect("funded builder should build");
    let tx_hash = mantle_tx.hash();

    let mut proofs = vec![op_proof(tx_hash)];
    proofs.extend(
        transfer_proofs_for_funded_wallet_tx(&mantle_tx, &funding_account.secret_key)
            .expect("transfer proofs should build"),
    );
    let op_proofs = OpProofs::try_from(proofs).unwrap();
    let signed_tx =
        SignedOps::from_parts(mantle_tx, op_proofs).expect("op proofs should match the ops");

    (signed_tx, fee)
}

async fn current_wallet_funding_source(
    client: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    account: WalletAccount,
) -> Result<WalletFundingSource, DirectWalletSourceError> {
    let tip = client.consensus_info().await?.cryptarchia_info.tip;

    wallet_funding_source_from_chain(client, tip, genesis_utxos, account).await
}

async fn wallet_funding_source_from_chain(
    client: &NodeHttpClient,
    tip: lb_core::header::HeaderId,
    genesis_utxos: &[Utxo],
    account: WalletAccount,
) -> Result<WalletFundingSource, DirectWalletSourceError> {
    let wallet_id = WalletId::from(account.label.clone());
    let tracked_wallet = TrackedWalletKeys::new(wallet_id.clone(), [account.public_key()]);

    let wallet_utxos =
        wallet_utxos_from_chain(client, tip, &[tracked_wallet], genesis_utxos).await?;
    let available_utxos = wallet_utxos
        .get(wallet_id.as_str())
        .cloned()
        .ok_or(DirectWalletSourceError::MissingWallet { wallet_id })?;

    Ok(WalletFundingSource::new(account, available_utxos))
}

async fn wallet_utxos_from_chain(
    client: &NodeHttpClient,
    tip: lb_core::header::HeaderId,
    tracked_wallets: &[TrackedWalletKeys],
    genesis_utxos: &[Utxo],
) -> Result<WalletUtxos, DirectWalletSourceError> {
    let mut accounting = ScannerAccounting::new(tracked_wallets.to_vec(), genesis_utxos)?;
    let mut tail_blocks = Vec::new();
    let mut current = tip;

    while let Some(block) = client.block(&current).await? {
        current = block.header.parent_block;
        tail_blocks.push(block);
    }

    tail_blocks.reverse();

    for block in tail_blocks {
        accounting.apply_block(&block);
    }

    Ok(accounting.wallet_utxos())
}
