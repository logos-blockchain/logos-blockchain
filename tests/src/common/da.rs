use std::{collections::HashSet, time::Duration};

use chain_service::CryptarchiaInfo;
use common_http_client::Error;
use executor_http_client::ExecutorHttpClient;
use futures::StreamExt as _;
use key_management_system_service::keys::{Ed25519Key, ZkKey};
use nomos_core::{
    block::Block,
    da::BlobId,
    mantle::{
        AuthenticatedMantleTx as _, MantleTx, Op, SignedMantleTx, Transaction as _,
        ledger::Tx as LedgerTx,
        ops::{
            OpProof,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
        },
    },
};
use reqwest::Url;

use crate::{adjust_timeout, common::chain::scan_chain_until, nodes::executor::Executor};

const TEST_SIGNING_KEY_BYTES: [u8; 32] = [0u8; 32];

pub const DA_TESTS_TIMEOUT: u64 = 120;

pub async fn disseminate_with_metadata(
    executor: &Executor,
    channel_id: ChannelId,
    parent_msg_id: MsgId,
    data: &[u8],
) -> Result<BlobId, Error> {
    let executor_config = executor.config();
    let backend_address = executor_config.http.backend_settings.address;
    let client = ExecutorHttpClient::new(None);
    let exec_url = Url::parse(&format!("http://{backend_address}")).unwrap();

    let signer = Ed25519Key::from_bytes(&TEST_SIGNING_KEY_BYTES).public_key();

    client
        .publish_blob(exec_url, channel_id, parent_msg_id, signer, data.to_vec())
        .await
}

/// Wait for the specified blob to appear on-chain and return its message id.
pub async fn wait_for_blob_onchain(
    executor: &Executor,
    channel_id: ChannelId,
    blob_id: BlobId,
) -> MsgId {
    const POLL_DELAY_MS: u64 = 200;
    let mut scanned_blocks = HashSet::new();
    let block_fut = async {
        loop {
            let CryptarchiaInfo { tip, .. } = executor.consensus_info().await;
            if let Some(msg_id) = scan_chain_until(
                tip,
                &mut scanned_blocks,
                |header_id| executor.get_block(header_id),
                |block| {
                    find_channel_op(block, &mut |op| {
                        if let Op::ChannelBlob(blob_op) = op
                            && blob_op.channel == channel_id
                            && blob_op.blob == blob_id
                        {
                            Some(blob_op.id())
                        } else {
                            None
                        }
                    })
                },
            )
            .await
            {
                return msg_id;
            }

            tokio::time::sleep(Duration::from_millis(POLL_DELAY_MS)).await;
        }
    };

    let timeout = adjust_timeout(Duration::from_secs(DA_TESTS_TIMEOUT));
    tokio::time::timeout(timeout, block_fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for blob shares"))
}

/// Sets up a test channel by sending an inscription transaction and waiting for
/// it to be included in a block.
///
/// Returns the channel ID together with the inscription message id, which
/// should be used as the parent for the first blob operation.
pub async fn setup_test_channel(executor: &Executor) -> (ChannelId, MsgId) {
    let test_channel_id = ChannelId::from([1u8; 32]);
    let inscription_tx = create_inscription_transaction_with_id(test_channel_id);
    executor.add_tx(inscription_tx).await.unwrap();

    let inscription_id = wait_for_inscription_onchain(executor, test_channel_id).await;

    (test_channel_id, inscription_id)
}

/// Creates an inscription transaction using the same hardcoded key as the mock
/// wallet adapter.
#[must_use]
pub fn create_inscription_transaction_with_id(id: ChannelId) -> SignedMantleTx {
    let signing_key = Ed25519Key::from_bytes(&TEST_SIGNING_KEY_BYTES);
    let signer = signing_key.public_key();

    let inscription_op = InscriptionOp {
        channel_id: id,
        inscription: format!("Test channel inscription {id:?}").into_bytes(),
        parent: MsgId::root(),
        signer,
    };

    let mantle_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscription_op)],
        ledger_tx: LedgerTx::new(vec![], vec![]),
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();
    let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

    SignedMantleTx::new(
        mantle_tx,
        vec![OpProof::Ed25519Sig(signature)],
        ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
    )
    .unwrap()
}

async fn wait_for_inscription_onchain(executor: &Executor, channel_id: ChannelId) -> MsgId {
    let block_fut = async {
        let mut scanned_blocks = HashSet::new();
        loop {
            let info = executor.consensus_info().await;
            if let Some(msg_id) = scan_chain_until(
                info.tip,
                &mut scanned_blocks,
                |header_id| executor.get_block(header_id),
                |block| {
                    find_channel_op(block, &mut |op| {
                        if let Op::ChannelInscribe(inscribe_op) = op
                            && inscribe_op.channel_id == channel_id
                        {
                            Some(inscribe_op.id())
                        } else {
                            None
                        }
                    })
                },
            )
            .await
            {
                return msg_id;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };

    let timeout = adjust_timeout(Duration::from_secs(DA_TESTS_TIMEOUT));
    tokio::time::timeout(timeout, block_fut)
        .await
        .unwrap_or_else(|_| {
            panic!("timed out waiting for inscription transaction to be included in block")
        })
}

pub async fn wait_for_shares_number(executor: &Executor, blob_id: BlobId, num_shares: usize) {
    let shares_fut = async {
        let mut got_shares = 0;
        while got_shares < num_shares {
            let shares_result = executor
                .get_shares(blob_id, [].into(), [].into(), true)
                .await;
            if let Ok(shares_stream) = shares_result {
                got_shares = shares_stream.collect::<Vec<_>>().await.len();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };

    let timeout = adjust_timeout(Duration::from_secs(DA_TESTS_TIMEOUT));
    assert!(
        (tokio::time::timeout(timeout, shares_fut).await).is_ok(),
        "timed out waiting for blob shares"
    );
}

fn find_channel_op<F>(block: &Block<SignedMantleTx>, matcher: &mut F) -> Option<MsgId>
where
    F: FnMut(&Op) -> Option<MsgId>,
{
    for tx in block.transactions() {
        for op in &tx.mantle_tx().ops {
            if let Some(msg_id) = matcher(op) {
                return Some(msg_id);
            }
        }
    }

    None
}
