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

  @transactions_ci
  Scenario: Wallet fund endpoint funds a payment and returns a transfer proof
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000         |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I fund a transaction paying 10 LGO from node "NODE_1" wallet to wallet "WALLET_1A" as "FUNDED_PAYMENT"
    Then transaction "FUNDED_PAYMENT" is included on node "NODE_1" in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Wallet fund endpoint leaves a feeless transaction unchanged
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000         |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I fund an inscription transaction on node "NODE_1" as "FUNDED_INSCRIPTION"
    Then transaction "FUNDED_INSCRIPTION" is included on node "NODE_1" in 120 seconds
    Then I stop all nodes
