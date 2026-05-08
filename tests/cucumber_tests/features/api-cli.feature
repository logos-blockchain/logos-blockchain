Feature: CLI

  @cli_ci
  Scenario: Join Blend via CLI declaration
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 2000         |
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When all nodes have at least 2 blocks and converged to within 1 blocks in 180 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to blend core zk key of node "NODE_1"
    Then I declare node "NODE_1" as blend core node via the CLI binary
    And blend core SDP declaration for node "NODE_1" is included on node "NODE_1"
    And I stop all nodes
