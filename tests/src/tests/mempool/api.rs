use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::{MantleTx, SignedMantleTx};
use logos_blockchain_tests::topology::{Topology, TopologyConfig};
use reqwest::Url;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_post_mantle_tx() {
    let topology = Topology::spawn(
        TopologyConfig::two_validators(),
        Some("test_post_mantle_tx"),
    )
    .await;
    let validator = &topology.validators()[0];

    let validator_url = Url::parse(
        format!(
            "http://{}",
            validator.config().user.api.backend.listen_address
        )
        .as_str(),
    )
    .unwrap();

    let mantle_tx = MantleTx {
        ops: Vec::new(),
        storage_gas_price: 0.into(),
        execution_gas_price: 0.into(),
    };

    let signed_tx = SignedMantleTx {
        ops_proofs: Vec::new(),
        mantle_tx,
    };

    let client = CommonHttpClient::new(None);
    let res = client.post_transaction(validator_url, signed_tx).await;
    assert!(res.is_ok());
}
