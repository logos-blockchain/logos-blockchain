Feature: Cryptarchia

  @cryptarchia
  Scenario: Two nodes happy path
    Given I have a cluster with capacity of 2 nodes
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then all nodes have at least 20 blocks and converged to within 1 blocks in 240 seconds
    Then I stop all nodes

  @cryptarchia
  Scenario: Orphan staggered start
    Given I have a cluster with capacity of 5 nodes
    And I start node "NODE_1"
    When node "NODE_1" is at height 10 in 300 seconds
    And I start peer node "NODE_2" connected to node "NODE_1"
    And I start peer node "NODE_3" connected to node "NODE_2"
    And I start peer node "NODE_4" connected to node "NODE_3"
    And I start peer node "NODE_5" connected to node "NODE_4"
    Then all nodes have at least 20 blocks and converged to within 1 blocks in 180 seconds
    Then I stop all nodes

  @cryptarchia @flaky
  Scenario: Orphan staggered start with fork 1
    Given I have a cluster with capacity of 9 nodes
    And I start node "NODE_A1"
    When node "NODE_A1" is at height 10 in 300 seconds
    And I start peer node "NODE_A2" connected to node "NODE_A1"
    And I start peer node "NODE_A3" connected to node "NODE_A2"
    And I start peer node "NODE_A4" connected to node "NODE_A3"
    When all nodes have at least 10 blocks and converged to within 1 blocks in 180 seconds
    And I start node "NODE_B1"
    And I start peer node "NODE_B2" connected to node "NODE_B1"
    And I start peer node "NODE_B3" connected to node "NODE_B2"
    And I start peer node "NODE_B4" connected to node "NODE_B3"
    When node "NODE_A1" is at height 20 in 180 seconds
    And I start peer node "NODE_JOIN" connected to node "NODE_A4" and node "NODE_B4"
    Then all nodes have at least 30 blocks and converged to within 1 blocks in 180 seconds
    Then I stop all nodes

  @cryptarchia @undefined_behaviour
  Scenario: Orphan staggered start with fork 2
    Given I have a cluster with capacity of 11 nodes
    And I start node "NODE_A1"
    When node "NODE_A1" is at height 10 in 300 seconds
    And I start peer node "NODE_A2" connected to node "NODE_A1"
    And I start peer node "NODE_A3" connected to node "NODE_A2"
    And I start peer node "NODE_A4" connected to node "NODE_A3"
    And I start node "NODE_B1"
    And I start peer node "NODE_B2" connected to node "NODE_B1"
    And I start peer node "NODE_B3" connected to node "NODE_B2"
    And I start peer node "NODE_B4" connected to node "NODE_B3"
    And I start peer node "NODE_B5" connected to node "NODE_B4"
    When node "NODE_A1" is at height 20 in 180 seconds
    And I start peer node "NODE_A5" connected to node "NODE_A4"
    And I start peer node "NODE_JOIN" connected to node "NODE_A5" and node "NODE_B5"
    Then all nodes have at least 30 blocks and converged to within 1 blocks in 180 seconds
    Then I stop all nodes
