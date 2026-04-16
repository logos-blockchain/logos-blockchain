Feature: Blend

  @blend_ci
  Scenario: Blend core mode reaches consensus
    Given I have a cluster with capacity of 4 nodes
    And the first 4 nodes are declared as blend providers
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    And I start peer node "NODE_3" connected to node "NODE_1"
    And I start peer node "NODE_4" connected to node "NODE_1"
    Then all nodes have at least 10 blocks and converged to within 1 blocks in 360 seconds
    And all nodes agree on LIB in 300 seconds
    Then I stop all nodes

  @blend_ci
  Scenario: Blend edge mode reaches consensus
    Given I have a cluster with capacity of 4 nodes
    And the first 2 nodes are declared as blend providers
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    And I start peer node "NODE_3" connected to node "NODE_1"
    And I start peer node "NODE_4" connected to node "NODE_1"
    Then all nodes have at least 10 blocks and converged to within 1 blocks in 360 seconds
    And all nodes agree on LIB in 300 seconds
    Then I stop all nodes

  @blend_ci
  Scenario: Blend broadcast mode reaches consensus
    Given I have a cluster with capacity of 4 nodes
    And no nodes are declared as blend providers
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    And I start peer node "NODE_3" connected to node "NODE_1"
    And I start peer node "NODE_4" connected to node "NODE_1"
    Then all nodes have at least 10 blocks and converged to within 1 blocks in 360 seconds
    And all nodes agree on LIB in 300 seconds
    Then I stop all nodes
