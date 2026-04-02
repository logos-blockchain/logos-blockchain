Feature: K8s Manual Cluster

  Scenario: Start a simple k8s manual cluster
    Given I have a k8s manual cluster with capacity of 2 nodes
    When I k8s-manually start node "node-0"
    And I k8s-manually start node "node-1" connected to node "node-0"
    Then k8s manual node "node-0" has at least 1 peers within 30 seconds
    And k8s manual node "node-1" has at least 1 peers within 30 seconds
    And I stop all k8s manual nodes
