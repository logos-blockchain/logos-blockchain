Feature: Fees

  @transactions_ci
  Scenario: Gas prices endpoint returns the genesis prices
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000         |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When node "NODE_1" is at height 1 in 180 seconds
    Then gas prices on node "NODE_1" equal the genesis gas prices
    Then I stop all nodes
