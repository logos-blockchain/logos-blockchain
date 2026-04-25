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
  Scenario: Nodes with delayed genesis start joins network
    Given I have a cluster with capacity of 3 nodes
    And I have deployment config override "time.chain_start_time" as "now_plus_seconds(60)"
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
    And I immediate start node "NODE_1"
    And I immediate start peer node "NODE_2" connected to node "NODE_1"
    And I immediate start peer node "NODE_3" connected to node "NODE_2"
    # TODO: Activate these steps when states before blockchain start are implemented and we can check that nodes are in
    # TODO: waiting for genesis state
#    When I wait for all nodes to be responsive in 45 seconds
#    When all nodes have at least 3 blocks and converged to within 1 blocks in 300 seconds
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
