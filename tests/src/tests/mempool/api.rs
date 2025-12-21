use common_http_client::CommonHttpClient;
use key_management_system_service::keys::ZkKey;
use nomos_core::mantle::{MantleTx, SignedMantleTx, Transaction as _, ledger::Tx as LedgerTx};
use reqwest::Url;
use serial_test::serial;
use tests::topology::{Topology, TopologyConfig};

#[tokio::test]
#[serial]
async fn test_post_mantle_tx() {
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

    let mantle_tx = MantleTx {
        ops: Vec::new(),
        ledger_tx: LedgerTx::new(vec![], vec![]),
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let signed_tx = SignedMantleTx {
        ops_proofs: Vec::new(),
        ledger_tx_proof: ZkKey::multi_sign(&[], mantle_tx.hash().as_ref()).unwrap(),
        mantle_tx,
    };

    let client = CommonHttpClient::new(None);
    let res = client.post_transaction(validator_url, signed_tx).await;
    assert!(res.is_ok());
}
