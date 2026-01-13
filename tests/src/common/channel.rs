use std::{collections::HashSet, time::Duration};

use key_management_system_service::keys::{Ed25519Key, ZkKey};
use nomos_core::{
    block::Block,
    mantle::{
        AuthenticatedMantleTx as _, MantleTx, Op, SignedMantleTx, Transaction as _,
        ledger::Tx as LedgerTx,
        ops::{
            OpProof,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
        },
    },
};

use crate::{adjust_timeout, common::chain::scan_chain_until, nodes::executor::Executor};

const TEST_SIGNING_KEY_BYTES: [u8; 32] = [0u8; 32];

pub const DA_TESTS_TIMEOUT: u64 = 120;

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
