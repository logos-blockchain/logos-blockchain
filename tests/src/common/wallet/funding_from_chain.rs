use std::collections::{BTreeMap, HashMap, HashSet};

use lb_common_http_client::Error as HttpClientError;
use lb_core::mantle::{
    Op, OpProof, SignedOps, TxHash,
    gas::MainnetGasProfile,
    ledger::verification_mode::StandardMode,
    ops::ZkTransferProof,
    traits::Hashable as _,
    transactions::{
        GasPrices, MantleTxBuilder, OpProofs,
        builder::ZkTransferOpening,
        states::Unverified,
        tx_list::ops::{OpsContext, OpsGasContext},
    },
};
use lb_key_management_system_service::keys::UnsecuredZkKey;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use lb_wallet::transaction::WitnessContext;
use thiserror::Error;

use super::{
    WalletId,
    chain::state::{GenesisNotes, TrackedWalletKeys},
    scanner::{
        accounting::{ScannerAccounting, ScannerAccountingError},
        block_source::block_events,
        error::ScannerError,
    },
};
use crate::common::wallet::{
    random_nonce, record_note_openings, transfer_proofs_for_funded_wallet_tx,
};

#[derive(Debug, Error)]
enum DirectWalletSourceError {
    #[error(transparent)]
    Source(#[from] HttpClientError),
    #[error(transparent)]
    Accounting(#[from] ScannerAccountingError),
    #[error(transparent)]
    Events(#[from] ScannerError),
}

/// Build, fund, and sign a single-op transaction.
///
/// The op fee is paid from the funding wallet (synced from chain), whose
/// trailing transfer op gets its own proof. The op proof is built via
/// `op_proof` from the funded transaction hash. Returns the signed
/// transaction and its fee at genesis gas prices.
pub async fn funded_signed_tx(
    node: &NodeHttpClient,
    genesis: &GenesisNotes,
    funding_account: &WalletAccount,
    op: Op,
    op_proof: impl FnOnce(TxHash) -> OpProof,
) -> (SignedOps<Unverified, StandardMode>, u64) {
    let tx_builder = MantleTxBuilder::new()
        .push_op(op)
        .expect("op should fit op bounds");
    fund_and_sign(
        node,
        genesis,
        funding_account,
        [],
        &tx_builder,
        &WitnessContext::default(),
        |tx_hash, _| op_proof(tx_hash),
    )
    .await
}

// Same for an op that spends: `opening` opens its inputs, owned by
// `spender_keys`, and `op_proof` wraps its ZkTransfer proof.
#[expect(clippy::too_many_arguments, reason = "the spending inputs")]
pub async fn funded_signed_spending_tx(
    node: &NodeHttpClient,
    genesis: &GenesisNotes,
    funding_account: &WalletAccount,
    spender_keys: impl IntoIterator<Item = UnsecuredZkKey>,
    op: Op,
    opening: ZkTransferOpening,
    witness_context: &WitnessContext,
    op_proof: impl FnOnce(TxHash, ZkTransferProof) -> OpProof,
) -> (SignedOps<Unverified, StandardMode>, u64) {
    let tx_builder = MantleTxBuilder::new()
        .push_op_with_opening(op, opening)
        .expect("op should fit op bounds");
    fund_and_sign(
        node,
        genesis,
        funding_account,
        spender_keys,
        &tx_builder,
        witness_context,
        |tx_hash, zk_proofs| {
            op_proof(
                tx_hash,
                zk_proofs.remove(&0).expect("the leading op spends"),
            )
        },
    )
    .await
}

async fn fund_and_sign(
    node: &NodeHttpClient,
    genesis: &GenesisNotes,
    funding_account: &WalletAccount,
    spender_keys: impl IntoIterator<Item = UnsecuredZkKey>,
    tx_builder: &MantleTxBuilder,
    witness_context: &WitnessContext,
    op_proof: impl FnOnce(TxHash, &mut BTreeMap<usize, ZkTransferProof>) -> OpProof,
) -> (SignedOps<Unverified, StandardMode>, u64) {
    let accounting = current_wallet_accounting(node, genesis, funding_account, spender_keys)
        .await
        .expect("funding wallet source should sync from chain");

    let tx_context = OpsContext {
        gas_context: OpsGasContext::new(HashMap::new(), GasPrices::default()),
    };

    let funded_builder = accounting
        .wallet_state()
        .fund_tx::<MainnetGasProfile>(
            tx_builder,
            funding_account.public_key(),
            random_nonce(),
            [funding_account.public_key()],
            &tx_context,
            &HashSet::new(),
            0,
        )
        .expect("funding transaction should succeed");
    let fee = funded_builder
        .minimum_gas_cost::<MainnetGasProfile>(&tx_context)
        .expect("funded tx gas cost should calculate")
        .into_inner();

    let openings = funded_builder.openings();
    // The change note is adopted once its commitment is appended.
    record_note_openings(
        openings
            .values()
            .flat_map(|opening| opening.outputs.iter().copied()),
    );
    let mantle_tx = funded_builder.build().expect("funded builder should build");
    let tx_hash = mantle_tx.hash();

    // Every op that spends is proven from the wallet's witnesses, the leading
    // op's proof included.
    let mut zk_proofs = transfer_proofs_for_funded_wallet_tx(
        accounting.wallet_state(),
        accounting.known_notes(),
        &mantle_tx,
        &openings,
        witness_context,
        accounting.secret_keys(),
    )
    .expect("transfer proofs should build");
    let mut proofs = vec![op_proof(tx_hash, &mut zk_proofs)];
    proofs.extend(zk_proofs.into_values().map(OpProof::ZkTransfer));
    let op_proofs = OpProofs::try_from(proofs).unwrap();
    let signed_tx =
        SignedOps::from_parts(mantle_tx, op_proofs).expect("op proofs should match the ops");

    (signed_tx, fee)
}

async fn current_wallet_accounting(
    client: &NodeHttpClient,
    genesis: &GenesisNotes,
    account: &WalletAccount,
    spender_keys: impl IntoIterator<Item = UnsecuredZkKey>,
) -> Result<ScannerAccounting, DirectWalletSourceError> {
    let tip = client.consensus_info().await?.cryptarchia_info.tip;
    let tracked_wallet = TrackedWalletKeys::new(
        WalletId::from(account.label.clone()),
        std::iter::once(UnsecuredZkKey::new(*account.secret_key.as_fr())).chain(spender_keys),
    );

    wallet_accounting_from_chain(client, tip, &[tracked_wallet], genesis).await
}

// Follows the chain from genesis to `tip`, block by block with their events.
async fn wallet_accounting_from_chain(
    client: &NodeHttpClient,
    tip: lb_core::header::HeaderId,
    tracked_wallets: &[TrackedWalletKeys],
    genesis: &GenesisNotes,
) -> Result<ScannerAccounting, DirectWalletSourceError> {
    let mut accounting = ScannerAccounting::new(tracked_wallets.to_vec(), genesis)?;
    let mut tail_blocks = Vec::new();
    let mut current = tip;

    while let Some(block) = client.block(&current).await? {
        current = block.header.parent_block;
        tail_blocks.push(block);
    }

    tail_blocks.reverse();

    // The genesis block creates the genesis notes the accounting starts from.
    for block in tail_blocks.into_iter().skip(1) {
        let events = block_events(client, block.header.id).await?;
        accounting
            .apply_block(&block, &events)
            .map_err(ScannerAccountingError::from)?;
    }

    Ok(accounting)
}
