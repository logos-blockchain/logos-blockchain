Feature: Happy-path era boundaries

  @era_boundaries
  Scenario: Connected nodes progress through era activation
    Given the cluster uses parameter set "cucumber_fixture"
    And no nodes are declared as blend providers
    And the cluster uses era schedule from era 0 to era 1 starting at epoch 1
    And I have a cluster with capacity of 2 nodes
    And the cluster uses prolonged bootstrap period of 0 seconds
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then the running nodes use the same effective deployment
    And the source and target eras have the same ruleset and distinct fork digests
    And all nodes share the same LIB at or above height 5 in 120 seconds
    When I capture a non-genesis finalized block before the era boundary on node "NODE_1"
    And all nodes enter the target era within 260 seconds
    Then all nodes report the target-era fork digest within 30 seconds
    And all nodes have at least 3 canonical target-era blocks and at least 1 canonical source-era blocks within 180 seconds
    And all nodes have at least 12 blocks and converged to within 1 blocks in 180 seconds
    And all nodes preserve the captured finalized block on their LIB ancestry after the era boundary within 120 seconds
    And I stop all nodes

  @era_boundaries
  Scenario: A node joining in era 1 synchronizes historical era-0 blocks
    Given the cluster uses parameter set "cucumber_fixture"
    And no nodes are declared as blend providers
    And the cluster uses era schedule from era 0 to era 1 starting at epoch 1
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And the cluster uses prolonged bootstrap period of 0 seconds
    And I start node "NODE_1"
    Then the running nodes use the same effective deployment
    And the source and target eras have the same ruleset and distinct fork digests
    And all nodes share the same LIB at or above height 5 in 120 seconds
    When I capture a non-genesis finalized block before the era boundary on node "NODE_1"
    And all nodes enter the target era within 260 seconds
    Then all nodes report the target-era fork digest within 30 seconds
    Then all nodes have at least 3 canonical target-era blocks and at least 1 canonical source-era blocks within 180 seconds
    When I prepare node "NODE_2" to join with fresh state from node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    Then node "NODE_2" started in the target era
    And the running nodes use the same effective deployment
    And the source and target eras have the same ruleset and distinct fork digests
    And all nodes have at least 3 canonical target-era blocks and at least 1 canonical source-era blocks within 240 seconds
    And all nodes have at least 12 blocks and converged to within 1 blocks in 240 seconds
    And node "NODE_2" acquires the captured source-era finalized block in canonical history and all nodes preserve it on their LIB ancestry within 120 seconds
    And I stop all nodes
