Feature: Cryptarchia

  @cryptarchia_ci
  Scenario: One node happy path
    Given I have a cluster with capacity of 1 nodes
    And I start node "NODE_1"
    Then all nodes have at least 5 blocks and converged to within 1 blocks in 300 seconds
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: Two nodes happy path
    Given I have a cluster with capacity of 2 nodes
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then all nodes have at least 5 blocks and converged to within 1 blocks in 300 seconds
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: Nodes started before and after delayed genesis converge
    Given I have a cluster with capacity of 3 nodes
    And the chain starts 60 seconds from now
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
    And I immediate start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then the configured genesis time has not passed for 40 seconds
    When node "NODE_1" is at height 1 in 300 seconds
    And I start peer node "NODE_3" connected to node "NODE_1"
    Then all nodes have at least 2 blocks and converged to within 0 blocks in 300 seconds
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: IBD staggered start
    Given I have a cluster with capacity of 5 nodes
    And no nodes are declared as blend providers
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start node "NODE_1"
    And I start peer node "NODE_1B" connected to node "NODE_1"
    When node "NODE_1" is at height 1 in 300 seconds
    And I start peer node "NODE_2" connected to node "NODE_1"
    When node "NODE_2" is at height 2 in 300 seconds
    And I start peer node "NODE_3" connected to node "NODE_2"
    When node "NODE_3" is at height 3 in 300 seconds
    And I start peer node "NODE_4" connected to node "NODE_3"
    Then all nodes have at least 4 blocks and converged to within 1 blocks in 300 seconds
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: Orphan staggered start
    Given I have a cluster with capacity of 5 nodes
    And no nodes are declared as blend providers
    And I start node "NODE_1"
    And I start peer node "NODE_1B" connected to node "NODE_1"
    When node "NODE_1" is at height 1 in 300 seconds
    And I start peer node "NODE_2" connected to node "NODE_1"
    When node "NODE_2" is at height 2 in 300 seconds
    And I start peer node "NODE_3" connected to node "NODE_2"
    When node "NODE_3" is at height 3 in 300 seconds
    And I start peer node "NODE_4" connected to node "NODE_3"
    Then all nodes have at least 4 blocks and converged to within 1 blocks in 300 seconds
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: Two nodes immutable blocks
    Given I have a cluster with capacity of 2 nodes
    And the cluster uses cryptarchia security parameter 5
    And the cluster uses prolonged bootstrap period of 0 seconds
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then all nodes share the same LIB at or above height 5 in 300 seconds
    Then I stop all nodes

  @cryptarchia_ci @cryptarchia_recovery
  Scenario: A node recovers the canonical history after switching forks
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And no nodes are declared as blend providers
    And the cluster uses cryptarchia security parameter 20
    And the cluster uses prolonged bootstrap period of 0 seconds
    And I have deployment config override "eras.0.time.slot_duration_in_seconds" as "1"
    And I have deployment config override "eras.0.cryptarchia.slot_activation_coeff.numerator" as "9"
    And I have user config override "cryptarchia.service.bootstrap.offline_grace_period.grace_period" as "seconds(0)"
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_A    | 1             | WALLET_A    |              |
      | NODE_A    | 2             | WALLET_B    |              |
    When node "NODE_A" is at height 4 in 120 seconds
    And I submit funded transfer transaction "TRANSFER" of 10000 LGO from wallet "WALLET_A" to wallet "WALLET_B"
    Then transaction "TRANSFER" is included on node "NODE_A" in 120 seconds
    When I start node "NODE_B"
    And node "NODE_B" is at height 2 in 120 seconds
    And I connect node "NODE_B" to node "NODE_A" at runtime
    Then transaction "TRANSFER" is included on node "NODE_B" in 120 seconds
    And all nodes share the same LIB at or above height 8 in 180 seconds
    When I record node "NODE_B" height as "BEFORE_RESTART"
    And I stop node "NODE_A"
    And I restart node "NODE_B"
    Then transaction "TRANSFER" is included on node "NODE_B" in 10 seconds
    And node "NODE_B" reaches 2 blocks beyond recorded height "BEFORE_RESTART" in 120 seconds
    When I restart node "NODE_A"
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: Orphan staggered fork start 1
    Given I have a cluster with capacity of 8 nodes
    And no nodes are declared as blend providers
    And I start node "NODE_A1"
    And I start peer node "NODE_A1_BUDDY" connected to node "NODE_A1"
    When node "NODE_A1" is at height 1 in 300 seconds
    And I start peer node "NODE_A2" connected to node "NODE_A1"
    And I start peer node "NODE_A3" connected to node "NODE_A2"
    When all nodes have at least 3 blocks and converged to within 1 blocks in 300 seconds
    And I start node "NODE_B1"
    And I start peer node "NODE_B2" connected to node "NODE_B1"
    And I start peer node "NODE_B3" connected to node "NODE_B2"
    When node "NODE_B1" is at height 1 in 300 seconds
    And I start peer node "NODE_JOIN" connected to node "NODE_A3" and node "NODE_B3"
    Then all nodes have at least 5 blocks and converged to within 1 blocks in 240 seconds
    Then I stop all nodes

  @cryptarchia_ci
  Scenario: Orphan staggered fork start 2
    Given I have a cluster with capacity of 8 nodes
    And no nodes are declared as blend providers
    And I start node "NODE_A1"
    And I start peer node "NODE_A1_BUDDY" connected to node "NODE_A1"
    When node "NODE_A1" is at height 1 in 360 seconds
    And I start peer node "NODE_A2" connected to node "NODE_A1"
    And I start peer node "NODE_A3" connected to node "NODE_A2"
    And I start node "NODE_B1"
    And I start peer node "NODE_B2" connected to node "NODE_B1"
    And I start peer node "NODE_B3" connected to node "NODE_B2"
    When node "NODE_B1" is at height 2 in 300 seconds
    And I start peer node "NODE_JOIN" connected to node "NODE_A3" and node "NODE_B3"
    Then all nodes have at least 3 blocks and converged to within 1 blocks in 300 seconds
    Then I stop all nodes
