Feature: K8s Manual Transactions

  Scenario: Send a simple transaction on a k8s manual cluster
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
      | 2             | 0           | 0            |
    And I have a k8s manual cluster with capacity of 2 nodes
    And I k8s-manually start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 300 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    Then wallet "WALLET_2A" has 1 or more outputs and 1000 or more LGO in 120 seconds
    And I stop all k8s manual nodes
