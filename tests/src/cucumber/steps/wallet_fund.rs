//! Steps exercising the node's `/wallet/fund` HTTP endpoint: fund a
//! transaction from the node's wallet, assemble the returned proofs and
//! submit the result to the mempool.

use cucumber::{gherkin::Step, when};
use lb_core::mantle::{
    Note, Op, OpProof, SignedMantleTx, Transaction as _,
    gas::GasCost,
    ops::channel::{
        ChannelId, MsgId,
        inscribe::{Inscription, InscriptionOp},
    },
    transactions::builder::MantleTxBuilder,
};
use lb_http_api_common::bodies::wallet::fund::{WalletFundRequestBody, WalletFundResponseBody};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_testing_framework::NodeHttpClient;
use tracing::info;

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    world::CucumberWorld,
};

/// Fund a payment transaction from the node's wallet: the fund endpoint must
/// pull wallet inputs to cover the output, append the fee transfer and return
/// a transfer proof that the ledger accepts.
#[when(
    expr = "I fund a transaction paying {int} LGO from node {string} wallet to wallet {string} as {string}"
)]
async fn step_fund_payment_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    amount: u64,
    node_name: String,
    receiver_wallet_name: String,
    transaction_alias: String,
) -> StepResult {
    let receiver_pk = world.resolve_wallet(&receiver_wallet_name)?.public_key()?;
    let funding_wallet = world.resolve_wallet(&format!("{node_name}_WALLET"))?;
    let funding_pk = funding_wallet.public_key()?;
    let client = world.resolve_node_http_client(&node_name)?;

    let tx_builder = MantleTxBuilder::new()
        .add_ledger_output(Note::new(amount, receiver_pk))
        .map_err(|source| StepError::LogicalError {
            message: format!(
                "Step `{}` error: failed to add output: {source}",
                step.value
            ),
        })?;

    let response = fund_via_node(&client, step, tx_builder, funding_pk).await?;

    let Some(transfer_proof) = response.transfer_proof else {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funding a payment must return a transfer proof",
                step.value
            ),
        });
    };
    let has_transfer = response
        .funded_tx
        .ops()
        .iter()
        .any(|op| matches!(op, Op::Transfer(_)));
    if !has_transfer {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funded payment must contain a transfer op",
                step.value
            ),
        });
    }

    let signed_tx =
        SignedMantleTx::new(response.funded_tx, vec![transfer_proof]).map_err(|source| {
            StepError::LogicalError {
                message: format!(
                    "Step `{}` error: assembling the funded transaction failed: {source:?}",
                    step.value
                ),
            }
        })?;
    let tx_hash = signed_tx.hash();

    world
        .submit_transaction(&funding_wallet, &signed_tx, &client)
        .await?;
    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted funded payment `{transaction_alias}` of {amount} LGO from node `{node_name}` wallet"
    );

    Ok(())
}

/// Fund an inscription-only transaction: at non-zero gas prices the node
/// appends a fee transfer paid from its wallet and returns the transfer
/// proof. The caller signs the channel op over the FUNDED hash and
/// assembles the proofs in op order — the split-signing flow a zone
/// sequencer uses.
#[when(expr = "I fund an inscription transaction on node {string} as {string}")]
async fn step_fund_inscription_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    transaction_alias: String,
) -> StepResult {
    let funding_wallet = world.resolve_wallet(&format!("{node_name}_WALLET"))?;
    let funding_pk = funding_wallet.public_key()?;
    let client = world.resolve_node_http_client(&node_name)?;

    // Deterministic sequencer key claiming a fresh channel; every scenario
    // runs on a fresh chain, so the channel cannot pre-exist.
    let signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());
    let inscription =
        Inscription::try_from(b"wallet fund endpoint".to_vec()).map_err(|source| {
            StepError::LogicalError {
                message: format!(
                    "Step `{}` error: failed to build inscription: {source}",
                    step.value
                ),
            }
        })?;
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent: MsgId::root(),
        signer: signing_key.public_key(),
    };

    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelInscribe(inscription_op))
        .map_err(|source| StepError::LogicalError {
            message: format!(
                "Step `{}` error: failed to push inscription op: {source}",
                step.value
            ),
        })?;

    let response = fund_via_node(&client, step, tx_builder, funding_pk).await?;

    let Some(transfer_proof) = response.transfer_proof else {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funding an inscription at non-zero gas prices must return a \
                transfer proof",
                step.value
            ),
        });
    };
    let ops = response.funded_tx.ops();
    if ops.len() != 2
        || !matches!(ops[0], Op::ChannelInscribe(_))
        || !matches!(ops[1], Op::Transfer(_))
    {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funded inscription must be [inscribe, fee transfer], got {} ops",
                step.value,
                ops.len()
            ),
        });
    }

    let tx_hash = response.funded_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());
    let signed_tx = SignedMantleTx::new(
        response.funded_tx,
        vec![OpProof::Ed25519Sig(signature), transfer_proof],
    )
    .map_err(|source| StepError::LogicalError {
        message: format!(
            "Step `{}` error: assembling the funded transaction failed: {source:?}",
            step.value
        ),
    })?;

    world
        .submit_transaction(&funding_wallet, &signed_tx, &client)
        .await?;
    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted funded inscription `{transaction_alias}` via node `{node_name}` fund endpoint"
    );

    Ok(())
}

async fn fund_via_node(
    client: &NodeHttpClient,
    step: &Step,
    tx_builder: MantleTxBuilder,
    funding_pk: ZkPublicKey,
) -> Result<WalletFundResponseBody, StepError> {
    client
        .fund_tx(WalletFundRequestBody {
            tip: None,
            tx_builder,
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(u64::MAX),
        })
        .await
        .map_err(|source| StepError::StepFail {
            message: format!("Step `{}` error: fund request failed: {source}", step.value),
        })
}
