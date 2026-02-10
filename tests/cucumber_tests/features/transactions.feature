Feature: Transactions

  @transactions_ci
  Scenario: Two nodes two wallets multiple transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers muse be mode online after startup
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    And I send 2 transactions of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 60 seconds
    And I send 2 transactions of 250 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 1300 or more LGO in 60 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Two nodes two wallets multiple outputs one transaction
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers muse be mode online after startup
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    And I send one transaction with 2 outputs of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 60 seconds
    And I send 2 transactions of 250 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 1300 or more LGO in 60 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Coin split with multiple transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers muse be mode online after startup
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    And I do a coin split for "WALLET_1A" of 10 UTXOs valued at 5000 LGO tokens each
    When wallet "WALLET_1A" has 12 or more outputs in 180 seconds
    And I send 5 transactions of 2000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    And I send one transaction with 2 outputs of 2000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 7 or more outputs and 14000 or more LGO in 60 seconds
    Then I stop all nodes

  @local_host @undefined_behaviour
  Scenario: Coin split transaction not mined
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
#    And all peers muse be mode online after startup
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    And I do a coin split for "WALLET_1A" of 100 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 100 or more outputs in 180 seconds
    And I do a coin split for "WALLET_1A" of 200 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 300 or more outputs in 180 seconds
    And I do a coin split for "WALLET_1A" of 250 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 550 or more outputs in 180 seconds
    # Breaks here - 300 seems to be too many outputs (259 are still fine) for the node to handle in a single
    # transaction, causing the transaction to not be mined and the test to fail
    And I do a coin split for "WALLET_1A" of 300 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 850 or more outputs in 180 seconds
    And I do a coin split for "WALLET_1A" of 400 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 1250 or more outputs in 180 seconds
    And I do a coin split for "WALLET_1A" of 500 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 1750 or more outputs in 180 seconds

    Then I stop all nodes

  @transactions_ci @undefined_behaviour
  Scenario: Multi output transaction not mined
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
#    And all peers muse be mode online after startup
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    # Breaks here - 300 seems to be too many outputs (259 are still fine) for the node to handle in a single
    # transaction, causing the transaction to not be mined and the test to fail
    And I send one transaction with 300 outputs of 100 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 100 or more outputs in 60 seconds
    Then I stop all nodes
