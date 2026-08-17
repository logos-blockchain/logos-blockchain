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

  @blend_ci
  Scenario: Join Blend via API declaration
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 2000         |
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When all nodes have at least 2 blocks and converged to within 1 blocks in 180 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to blend core zk key of node "NODE_1"
    Then I declare node "NODE_1" as blend core node via the API
    And blend core SDP declaration for node "NODE_1" is included on node "NODE_1"
    And I stop all nodes

  @blend_ci
  Scenario: Transactions submitted through Blend come back through the mempool
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 2           | 1000         |
      | 3             | 0           | 0            |
    # At the deployed difficulty one solution is around a minute of work, which
    # is the point of it — but not something a test should sit through. A shift
    # of 1 puts the threshold at half the field, so a solution takes a couple of
    # hashes.
    And I have deployment config override "cryptarchia.pow_config.blend.base_difficulty" as "1"
    And I have a cluster with capacity of 4 nodes
    And the first 2 nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_CORE |              |
      | NODE_1    | 2             | WALLET_EDGE |              |
      | NODE_1    | 3             | WALLET_DEST |              |
    And I start peer node "NODE_2" connected to node "NODE_1"
    And I start peer node "NODE_3" connected to node "NODE_1"
    And I start peer node "NODE_4" connected to node "NODE_1"
    When all nodes have at least 2 blocks and converged to within 1 blocks in 360 seconds

    # NODE_1 is a core node
    And I prepare transfer transaction "TX_VIA_CORE" of 100 LGO from wallet "WALLET_CORE" to wallet "WALLET_DEST"
    And I submit prepared transaction "TX_VIA_CORE" through Blend on node "NODE_1"
    Then transaction "TX_VIA_CORE" is not pending in mempool of all nodes in 10 seconds
    And transaction "TX_VIA_CORE" is pending in mempool of nodes in 180 seconds:
      | node_name |
      | NODE_1    |
      | NODE_2    |
      | NODE_3    |
      | NODE_4    |

    # NODE_4 is not a core node
    When I prepare transfer transaction "TX_VIA_EDGE" of 100 LGO from wallet "WALLET_EDGE" to wallet "WALLET_DEST"
    And I submit prepared transaction "TX_VIA_EDGE" through Blend on node "NODE_4"
    Then transaction "TX_VIA_EDGE" is not pending in mempool of all nodes in 10 seconds
    And transaction "TX_VIA_EDGE" is pending in mempool of nodes in 180 seconds:
      | node_name |
      | NODE_1    |
      | NODE_2    |
      | NODE_3    |
      | NODE_4    |
    Then I stop all nodes
