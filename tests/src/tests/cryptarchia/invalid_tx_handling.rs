use std::{collections::HashSet, time::Duration};

use common_http_client::CommonHttpClient;
use key_management_system_service::keys::{ZkKey, ZkPublicKey};
use nomos_core::mantle::{
    MantleTx, Note, SignedMantleTx, Transaction as _, TxHash, ledger::Tx as LedgerTx,
    ops::channel::ChannelId,
};
use num_bigint::BigUint;
use reqwest::Url;
use serial_test::serial;
use tests::{
    common::{chain::scan_chain_until, da::create_inscription_transaction_with_id},
    nodes::validator::Validator,
    topology::{Topology, TopologyConfig},
};
use tokio::time::timeout;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(60);

/// Verifies that invalid transactions don't prevent valid transactions from
/// being included in blocks.
#[tokio::test]
#[serial]
async fn invalid_transactions_are_handled() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    let validator = &topology.validators()[0];

    let validator_url = Url::parse(
        format!(
            "http://{}",
            validator.config().http.backend_settings.address
        )
        .as_str(),
    )
    .unwrap();

    let client = CommonHttpClient::new(None);

    let invalid_tx = create_invalid_transaction_with_id(0);
    let invalid_hash = invalid_tx.hash();
    client
        .post_transaction(validator_url.clone(), invalid_tx)
        .await
        .expect("Invalid transaction should be accepted by mempool for later pruning");
    let invalid_tx_hashes = [invalid_hash];

    let first_valid_tx = create_inscription_transaction_with_id(ChannelId::from([1u8; 32]));
    let first_valid_hash = first_valid_tx.hash();
    client
        .post_transaction(validator_url.clone(), first_valid_tx)
        .await
        .expect("First valid transaction should be accepted by mempool");

    let first_batch_hashes = [first_valid_hash];

    timeout(
        PROCESS_TIMEOUT,
        wait_for_transactions_processing(validator, &first_batch_hashes, &invalid_tx_hashes),
    )
    .await
    .expect("first transaction processing timed out");

    let second_valid_tx = create_inscription_transaction_with_id(ChannelId::from([2u8; 32]));
    let second_valid_hash = second_valid_tx.hash();
    client
        .post_transaction(validator_url.clone(), second_valid_tx)
        .await
        .expect("Second valid transaction should be accepted by mempool");

    let second_batch_hashes = [second_valid_hash];

    timeout(
        PROCESS_TIMEOUT,
        wait_for_transactions_processing(validator, &second_batch_hashes, &invalid_tx_hashes),
    )
    .await
    .expect("second transaction processing timed out");
}

async fn wait_for_transactions_processing(
    validator: &Validator,
    valid_tx_hashes: &[TxHash],
    invalid_tx_hashes: &[TxHash],
) {
    let mut found_valid_txs = HashSet::new();
    let mut found_invalid_txs = HashSet::new();
    let mut scanned_blocks = HashSet::new();

    loop {
        let info = validator.consensus_info(false).await;
        let _: Option<()> = scan_chain_until(
            info.tip,
            &mut scanned_blocks,
            |header_id| validator.get_block(header_id),
            |block| {
                for tx in block.transactions() {
                    let hash = nomos_core::mantle::Transaction::hash(tx);
                    if valid_tx_hashes.contains(&hash) {
                        found_valid_txs.insert(hash);
                    }
                    if invalid_tx_hashes.contains(&hash) {
                        found_invalid_txs.insert(hash);
                    }
                }
                None
            },
        )
        .await;

        if found_valid_txs.len() == valid_tx_hashes.len() {
            println!(
                "Found {}/{} valid transactions",
                found_valid_txs.len(),
                valid_tx_hashes.len()
            );
            break;
        }

        println!(
            "Found {}/{} valid transactions so far...",
            found_valid_txs.len(),
            valid_tx_hashes.len()
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert_eq!(
        valid_tx_hashes.len(),
        found_valid_txs.len(),
        "Not all valid transactions were included in blocks"
    );

    assert!(
        found_invalid_txs.is_empty(),
        "Invalid transactions found in blocks: {found_invalid_txs:?}"
    );
}

fn create_invalid_transaction_with_id(id: usize) -> SignedMantleTx {
    let output_note = Note::new(
        1000 + id as u64,
        ZkPublicKey::new(BigUint::from(1u8).into()),
    );

    let mantle_tx = MantleTx {
        ops: Vec::new(),
        ledger_tx: LedgerTx::new(vec![], vec![output_note]),
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    SignedMantleTx {
        ops_proofs: Vec::new(),
        ledger_tx_proof: ZkKey::multi_sign(&[], mantle_tx.hash().as_ref()).unwrap(),
        mantle_tx,
    }
}
