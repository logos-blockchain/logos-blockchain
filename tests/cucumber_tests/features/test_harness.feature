Feature: Test harness

  @smoke_ci
  Scenario: Two nodes connect at runtime
    Given I have a cluster with capacity of 2 nodes
    And I start node "NODE_1"
    And I start node "NODE_2"
    When I connect node "NODE_2" to node "NODE_1" at runtime
    Then node "NODE_1" has at least 1 peers within 15 seconds
    And node "NODE_2" has at least 1 peers within 15 seconds
    And I stop all nodes

  @smoke_ci
  Scenario: Local snapshot restore after shutdown
    Given I have a cluster with capacity of 1 nodes
    And all peers must be mode online after startup in 30 seconds
    And I start node "NODE_1"
    When node "NODE_1" is at height 3 in 300 seconds
    And I create a snapshot "LOCAL_SNAPSHOT_SMOKE" of node "NODE_1"
    Then I stop all nodes
    Given I will initialize started nodes from snapshot "LOCAL_SNAPSHOT_SMOKE" source node "NODE_1"
    And I have a cluster with capacity of 1 nodes
    And all peers must be mode online after startup in 30 seconds
    And I start node "NODE_1"
    When node "NODE_1" is at height 2 in 5 seconds
    When node "NODE_1" is at height 3 in 300 seconds
    Then I stop all nodes

  @smoke_ci
  Scenario: Local snapshot restores wallet extension state
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000000      |
      | 2             | 0           | 0            |
      | 3             | 0           | 0            |
      | 4             | 0           | 0            |
    And I have a cluster with capacity of 4 nodes
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_2       |
      | NODE_4    | 4             | WALLET_4A   | NODE_3       |
    When I do a coin split for "WALLET_1A" of 10 UTXOs valued at 1000 LGO tokens each
    Then wallet "WALLET_1A" has exactly 11 outputs in 120 seconds
    When node "NODE_1" is at height 2 in 300 seconds
    When I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_3A"
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_4A"
    Then wallet "WALLET_2A" has exactly 1 outputs in 120 seconds
    And wallet "WALLET_3A" has exactly 1 outputs in 120 seconds
    And wallet "WALLET_4A" has exactly 1 outputs in 120 seconds
    When I will create a snapshot "LOCAL_WALLET_SNAPSHOT_SMOKE" of all nodes when stopping
    When all nodes have at least 5 blocks and converged to within 0 blocks in 300 seconds
    Then I stop all nodes
    Given I will initialize started nodes from snapshot "LOCAL_WALLET_SNAPSHOT_SMOKE" source node "NODE_1"
    And I have a cluster with capacity of 4 nodes
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_2       |
      | NODE_4    | 4             | WALLET_4A   | NODE_3       |
    When all nodes have at least 5 blocks and converged to within 0 blocks in 30 seconds
    Then wallet "WALLET_1A" has exactly 9 outputs in 2 seconds
    And wallet "WALLET_2A" has exactly 1 outputs in 2 seconds
    And wallet "WALLET_3A" has exactly 1 outputs in 2 seconds
    And wallet "WALLET_4A" has exactly 1 outputs in 2 seconds
    When I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_3A"
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_4A"
    Then wallet "WALLET_1A" has all submitted transactions included in 300 seconds
    Then wallet "WALLET_2A" has exactly 2 outputs in 120 seconds
    And wallet "WALLET_3A" has exactly 2 outputs in 120 seconds
    And wallet "WALLET_4A" has exactly 2 outputs in 120 seconds
    And I stop all nodes
