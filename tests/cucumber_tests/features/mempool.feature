Feature: Mempool lifecycle

  @mempool_ci @mempool_manual
  Scenario: Submitted transaction becomes visible in current-tip mempool view
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_A    |              |
      | NODE_1    | 2             | WALLET_B    |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I prepare transfer transaction "TX_VISIBLE" of 100 LGO from wallet "WALLET_A" to wallet "WALLET_B"
    And I submit prepared transaction "TX_VISIBLE" to nodes:
      | node_name |
      | NODE_1    |
    Then transaction "TX_VISIBLE" is pending in mempool of nodes in 30 seconds:
      | node_name |
      | NODE_1    |
    Then I stop all nodes

  @mempool_ci @mempool_manual
  Scenario: Included transaction is removed from mempool view
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_A    |              |
      | NODE_1    | 2             | WALLET_B    |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I submit funded transfer transaction "TX_INCLUDED" of 100 LGO from wallet "WALLET_A" to wallet "WALLET_B"
    Then transaction "TX_INCLUDED" is included on node "NODE_1" in 240 seconds
    And transaction "TX_INCLUDED" is not pending in mempool of all nodes in 30 seconds
    And transaction "TX_INCLUDED" remains not pending in mempool of all nodes for 2 blocks in 180 seconds
    Then I stop all nodes

  @mempool_ci @mempool_manual
  Scenario: Pending transaction survives restart before inclusion
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    # Keep block production slow enough that the tx stays pending across restart.
    And I have deployment config override "time.slot_duration" as "seconds(60)"
    And I have deployment config override "cryptarchia.slot_activation_coeff.numerator" as "9"
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_A    |              |
      | NODE_1    | 2             | WALLET_B    |              |
    When node "NODE_1" is at height 2 in 360 seconds
    And I prepare transfer transaction "TX_PENDING_RESTART" of 100 LGO from wallet "WALLET_A" to wallet "WALLET_B"
    And I submit prepared transaction "TX_PENDING_RESTART" to nodes:
      | node_name |
      | NODE_1    |
    Then transaction "TX_PENDING_RESTART" is pending in mempool of nodes in 30 seconds:
      | node_name |
      | NODE_1    |
    And mempool recovery for node "NODE_1" contains transaction "TX_PENDING_RESTART"
    When I restart node "NODE_1"
    # TF only keeps the tx hash here; the node must recover the pending mempool entry.
    Then transaction "TX_PENDING_RESTART" is pending in mempool of nodes in 30 seconds:
      | node_name |
      | NODE_1    |
    Then I stop all nodes

  @mempool_ci @mempool_manual
  Scenario: Included transaction stays absent from mempool after restart
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_A    |              |
      | NODE_1    | 2             | WALLET_B    |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I submit funded transfer transaction "TX_RESTART" of 100 LGO from wallet "WALLET_A" to wallet "WALLET_B"
    Then transaction "TX_RESTART" is included on node "NODE_1" in 240 seconds
    When I restart node "NODE_1"
    And node "NODE_1" is at height 3 in 240 seconds
    # Included txs must not be recovered back into the pending mempool after restart.
    Then transaction "TX_RESTART" is not pending in mempool of all nodes in 30 seconds
    Then I stop all nodes

  @mempool_ci @mempool_manual
  Scenario: Invalid transaction is not retained in mempool
    Given I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start node "NODE_1"
    When node "NODE_1" is at height 2 in 240 seconds
    And I try to submit invalid transaction "TX_INVALID" to node "NODE_1"
    Then transaction "TX_INVALID" is not included in 30 seconds
    And transaction "TX_INVALID" is not pending in mempool of all nodes in 30 seconds
    Then I stop all nodes

  @mempool_ci @mempool_manual
  Scenario: Same transaction from competing forks is not included again after convergence
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 3 nodes
    And no nodes are declared as blend providers
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And we will have distinct node groups to query wallet balances:
      | group_name | node_name |
      | FORK_A     | NODE_A1   |
      | FORK_B     | NODE_B1   |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_A1   | 1             | WALLET_A    |              |
      | NODE_A1   | 2             | WALLET_B    |              |
    And I start node "NODE_B1"
    When node "NODE_A1" is at height 2 in 240 seconds
    And node "NODE_B1" is at height 2 in 240 seconds
    And I prepare transfer transaction "TX_SHARED" of 100 LGO from wallet "WALLET_A" to wallet "WALLET_B"
    And I submit prepared transaction "TX_SHARED" to nodes:
      | node_name |
      | NODE_A1   |
      | NODE_B1   |
    Then transaction "TX_SHARED" is pending in mempool of nodes in 60 seconds:
      | node_name |
      | NODE_A1   |
      | NODE_B1   |
    Then transaction "TX_SHARED" is included on node "NODE_A1" in 240 seconds
    And transaction "TX_SHARED" is included on node "NODE_B1" in 240 seconds
    When node "NODE_A1" is at height 5 in 180 seconds
    And node "NODE_B1" is at height 5 in 180 seconds
    And I start peer node "NODE_JOIN" connected to node "NODE_A1" and node "NODE_B1"
    Then all nodes have at least 8 blocks and converged to within 1 blocks in 240 seconds
    And transaction "TX_SHARED" is not pending in mempool of all nodes in 180 seconds
    And transaction "TX_SHARED" remains not pending in mempool of all nodes for 3 blocks in 240 seconds
    Then I stop all nodes
