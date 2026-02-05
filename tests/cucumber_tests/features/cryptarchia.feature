Feature: Cryptarchia

  @cryptarchia
  Scenario: Two nodes happy path
    Given I have a cluster with capacity of 2 nodes
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then all nodes have at least 10 blocks and converged to within 1 blocks in 300 seconds
    Then I stop all nodes

  @cryptarchia
  Scenario: Orphan staggered start
    Given I have a cluster with capacity of 4 nodes
    And I start node "NODE_1"
    When node "NODE_1" is at height 2 in 300 seconds
    And I start peer node "NODE_2" connected to node "NODE_1"
    When node "NODE_2" is at height 4 in 180 seconds
    And I start peer node "NODE_3" connected to node "NODE_2"
    When node "NODE_3" is at height 6 in 180 seconds
    And I start peer node "NODE_4" connected to node "NODE_3"
    Then all nodes have at least 8 blocks and converged to within 1 blocks in 180 seconds
    Then I stop all nodes

  @cryptarchia @flaky
  Scenario: Orphan staggered fork start 1
    Given I have a cluster with capacity of 7 nodes
    And I start node "NODE_A1"
    When node "NODE_A1" is at height 2 in 300 seconds
    And I start peer node "NODE_A2" connected to node "NODE_A1"
    And I start peer node "NODE_A3" connected to node "NODE_A2"
    When all nodes have at least 5 blocks and converged to within 1 blocks in 300 seconds
    And I start node "NODE_B1"
    And I start peer node "NODE_B2" connected to node "NODE_B1"
    And I start peer node "NODE_B3" connected to node "NODE_B2"
    When node "NODE_B1" is at height 2 in 300 seconds
    And I start peer node "NODE_JOIN" connected to node "NODE_A3" and node "NODE_B3"
    Then all nodes have at least 10 blocks and converged to within 1 blocks in 180 seconds
    Then I stop all nodes

  @cryptarchia @flaky
  Scenario: Orphan staggered fork start 2
    Given I have a cluster with capacity of 9 nodes
    And I start node "NODE_A1"
    When node "NODE_A1" is at height 2 in 300 seconds
    And I start peer node "NODE_A2" connected to node "NODE_A1"
    And I start peer node "NODE_A3" connected to node "NODE_A2"
    And I start peer node "NODE_A4" connected to node "NODE_A3"
    And I start node "NODE_B1"
    And I start peer node "NODE_B2" connected to node "NODE_B1"
    And I start peer node "NODE_B3" connected to node "NODE_B2"
    And I start peer node "NODE_B4" connected to node "NODE_B3"
    When node "NODE_B1" is at height 5 in 180 seconds
    And I start peer node "NODE_JOIN" connected to node "NODE_A4" and node "NODE_B4"
    Then all nodes have at least 10 blocks and converged to within 1 blocks in 180 seconds
    Then I stop all nodes
