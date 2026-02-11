use lb_core::mantle::genesis_tx::GenesisTx;
use lb_node::config::deployment::DeploymentSettings;
use lb_tests::topology::configs::{
    deployment::e2e_deployment_settings_with_genesis_tx,
    network::{Libp2pNetworkLayout, NetworkParams},
};
use logos_blockchain_tests::topology::configs::create_general_configs_with_blend_core_subset;

#[test]
fn test_deployment_settings_genesis_serialization() {
    // Create a genesis tx similar to the test
    let network_params = NetworkParams {
        libp2p_network_layout: Libp2pNetworkLayout::Full,
    };
    let (_, genesis_tx) = create_general_configs_with_blend_core_subset(
        2,  // n_validators
        2,  // n_initial_validators
        &network_params,
    );

    // Create deployment settings
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx.clone());

    // Serialize to YAML
    let yaml_string = serde_yaml::to_string(&deployment_settings)
        .expect("Should be able to serialize deployment settings");

    println!("Serialized deployment settings:\n{}", yaml_string);

    // Deserialize back
    let deserialized_settings: DeploymentSettings =
        serde_yaml::from_str(&yaml_string)
            .expect("Should be able to deserialize deployment settings");

    // Verify genesis_state is preserved
    assert_eq!(
        deployment_settings.cryptarchia.genesis_state,
        deserialized_settings.cryptarchia.genesis_state,
        "Genesis state should be preserved through serialization"
    );

    // Also verify it can be converted to StartingState
    let starting_state: lb_chain_service::StartingState = 
        deserialized_settings.cryptarchia.genesis_state.into();
    
    match starting_state {
        lb_chain_service::StartingState::Genesis { genesis_tx: recovered_tx } => {
            assert_eq!(genesis_tx, recovered_tx, "Genesis tx should match");
        }
        _ => panic!("Expected Genesis starting state"),
    }
}
