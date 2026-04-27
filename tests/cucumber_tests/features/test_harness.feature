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
