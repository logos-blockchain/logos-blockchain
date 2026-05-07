Feature: API CLI

  @api_cli_ci
  Scenario: Join Blend core via CLI declaration
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 2000         |
    And I have a cluster with capacity of 2 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    And I start peer node "NODE_2" connected to node "NODE_1"
    When all nodes have at least 2 blocks and converged to within 1 blocks in 180 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to blend core zk key of node "NODE_2"
    Then I run blend core SDP declaration CLI for node "NODE_2" against node "NODE_2"
    And I stop all nodes
