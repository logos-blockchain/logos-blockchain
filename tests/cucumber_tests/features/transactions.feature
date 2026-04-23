Feature: Transactions

  @transactions_ci
  Scenario: Large inscriptions are included
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 2000000      |
    And I have a cluster with capacity of 2 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    And I start peer node "NODE_2" connected to node "NODE_1"
    When all nodes have at least 2 blocks and converged to within 1 blocks in 180 seconds
    And I submit inscription transaction "INSCRIPTION_32K" of 32 KiB from wallet "WALLET_1A"
    Then transaction "INSCRIPTION_32K" is included on node "NODE_1" in 90 seconds
    When I submit inscription transaction "INSCRIPTION_128K" of 128 KiB from wallet "WALLET_1A"
    Then transaction "INSCRIPTION_128K" is included on node "NODE_1" in 90 seconds
    When I submit inscription transaction "INSCRIPTION_512K" of 512 KiB from wallet "WALLET_1A"
    Then transaction "INSCRIPTION_512K" is included on node "NODE_1" in 90 seconds
    When I submit inscription transaction "INSCRIPTION_896K" of 896 KiB from wallet "WALLET_1A"
    Then transaction "INSCRIPTION_896K" is included on node "NODE_1" in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Two nodes two wallets multiple transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 2 tokens of 997 value each
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    And I send 2 transactions of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 120 seconds
    And I send 2 transactions of 250 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    When wallet "WALLET_2A" has all submitted transactions settled in 120 seconds
    And tracked wallet fees equal sponsored fee account spent fees
    And wallet "WALLET_1A" has exact settled balance of 1500 LGO in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Two nodes two wallets multiple outputs one transaction
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 2 tokens of 997 value each
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    And I send one transaction with 2 outputs of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 120 seconds
    And I send 2 transactions of 250 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    When wallet "WALLET_2A" has all submitted transactions settled in 120 seconds
    And wallet "WALLET_1A" has exact settled balance of 1500 LGO in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Many nodes with wallets startup
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
      | 3             | 0           | 0            |
      | 4             | 0           | 0            |
      | 5             | 0           | 0            |
      | 6             | 0           | 0            |
      | 7             | 0           | 0            |
      | 8             | 0           | 0            |
      | 9             | 0           | 0            |
      | 10            | 0           | 0            |
    And I have a cluster with capacity of 10 nodes
#    And we use IBD peers
#    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_1       |
      | NODE_4    | 4             | WALLET_4A   | NODE_1       |
      | NODE_5    | 5             | WALLET_5A   | NODE_10      |
      | NODE_6    | 6             | WALLET_6A   | NODE_10      |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_7    | 7             | WALLET_7A   | NODE_4       |
      | NODE_8    | 8             | WALLET_8A   | NODE_1       |
      | NODE_9    | 9             | WALLET_9A   | NODE_5       |
      | NODE_10   | 10            | WALLET_10A  | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    And I send 2 transactions of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_10A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 120 seconds
    When wallet "WALLET_10A" has 2 or more outputs in 60 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Coin split with multiple transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 300 seconds
    And I do a coin split for "WALLET_1A" of 10 UTXOs valued at 5000 LGO tokens each
    When wallet "WALLET_1A" has 12 or more outputs in 240 seconds
    And I send 5 transactions of 2000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    And I send one transaction with 2 outputs of 2000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 7 or more outputs and 14000 or more LGO in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Coin split with many transfers to other
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 4           | 26000        |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
#    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 300 seconds
    # Coin split
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    # Many small transfers to other wallet
    When wallet "WALLET_1A" has 100 or more outputs in 240 seconds
    And I send 50 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 50 or more outputs in 240 seconds
    # All outputs accounted for
    When wallet "WALLET_1A" has 56000 or less LGO in 180 seconds
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    Then I stop all nodes

  @transactions_ci @undefined_behaviour
  Scenario: Two fork chains join later and preserve persisted state wallet balances
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1400         |
      | 2             | 2           | 1400         |
      | 4             | 2           | 1400         |
      | 5             | 2           | 1400         |
      | 7             | 0           | 0            |
    And I have a cluster with capacity of 5 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And we will have distinct node groups to query wallet balances:
      | group_name | node_name |
      | FORK_A     | NODE_1    |
      | FORK_A     | NODE_2    |
      | FORK_B     | NODE_4    |
      | FORK_B     | NODE_5    |
      | FORK_BURN  | NODE_BURN |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_4    | 4             | WALLET_4A   |              |
      | NODE_5    | 5             | WALLET_5A   | NODE_4       |
      | NODE_BURN | 7             | WALLET_BURN |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And node "NODE_4" is at height 2 in 180 seconds
    # Fork A transfer: each wallet sends half its funds
    And I send 1 transactions of 700 LGO each from wallet "WALLET_1A" to wallet "WALLET_BURN"
    And I send 1 transactions of 700 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    And I send 1 transactions of 700 LGO each from wallet "WALLET_2A" to wallet "WALLET_BURN"
    And I send 1 transactions of 700 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    # Fork B transfer: each wallet sends half its funds
    And I send 1 transactions of 700 LGO each from wallet "WALLET_4A" to wallet "WALLET_BURN"
    And I send 1 transactions of 700 LGO each from wallet "WALLET_4A" to wallet "WALLET_5A"
    And I send 1 transactions of 700 LGO each from wallet "WALLET_5A" to wallet "WALLET_BURN"
    And I send 1 transactions of 700 LGO each from wallet "WALLET_5A" to wallet "WALLET_4A"
    # Each wallet should now have 3 outputs: one received + two change (allow for fees)
    When wallet "WALLET_1A" has 3 or more outputs in 120 seconds
    And wallet "WALLET_2A" has 3 or more outputs in 60 seconds
    And wallet "WALLET_4A" has 3 or more outputs in 60 seconds
    And wallet "WALLET_5A" has 3 or more outputs in 60 seconds
    # Wait for more blocks to be mined on each fork to ensure the chains are well established
    When node "NODE_1" is at height 5 in 180 seconds
    And node "NODE_4" is at height 5 in 180 seconds
    # Bridge the two forks
    And I start peer node "NODE_JOIN" connected to node "NODE_1" and node "NODE_4"
    # Wait for all nodes to converge on the same chain and for the transactions to be mined in the new combined chain
    When node "NODE_JOIN" is at height 8 in 180 seconds
    # Query balances for all wallets after the forks join - previous state should be restored for the re-orged chain
    When I update all user wallets balances
    When wallet "WALLET_1A" has 3 or more outputs in 120 seconds
    And wallet "WALLET_2A" has 3 or more outputs in 10 seconds
    And wallet "WALLET_4A" has 3 or more outputs in 10 seconds
    And wallet "WALLET_5A" has 3 or more outputs in 10 seconds
    When wallet "WALLET_1A" has 2100 or less LGO in 10 seconds
    And wallet "WALLET_2A" has 2100 or less LGO in 10 seconds
    And wallet "WALLET_4A" has 2100 or less LGO in 10 seconds
    And wallet "WALLET_5A" has 2100 or less LGO in 10 seconds
    Then I stop all nodes

  @transactions_ci @undefined_behaviour
  Scenario: Coin join transaction never mined
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 4           | 26000        |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 20 tokens of 997 value each
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
#    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    # Coin split
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    And I do a coin split for "WALLET_1A" of 25 UTXOs valued at 1000 LGO tokens each
    # Do a transfers to other wallet
    When wallet "WALLET_1A" has 100 or more outputs in 180 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 1 or more outputs in 180 seconds
    # Coin join
    # Breaks here - the transaction that includes more than one outputs is never mined
    And I send 1 transactions of 2000 LGO each from wallet "WALLET_1A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    And I send 1 transactions of 47000 LGO each from wallet "WALLET_1A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Large coin split transactions are mined
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
#    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    And I do a coin split for "WALLET_1A" of 100 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 100 or more outputs in 180 seconds
    And I do a coin split for "WALLET_1A" of 200 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 300 or more outputs in 180 seconds
    # Maximum number of outputs per transaction is 255 (as per encoding limits)
    And I do a coin split for "WALLET_1A" of 250 UTXOs valued at 100 LGO tokens each
    When wallet "WALLET_1A" has 550 or more outputs in 180 seconds

    Then I stop all nodes

  @transactions_ci @undefined_behaviour
  Scenario: Multi output transaction not mined
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 100000       |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
#    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    # Breaks here - 300 seems to be too many outputs (259 are still fine) for the node to handle in a single
    # transaction, causing the transaction to not be mined and the test to fail
    And I send one transaction with 300 outputs of 100 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 100 or more outputs in 60 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Invalid transactions do not block valid transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    And I submit invalid transfer transaction "BAD_TX" to node "NODE_1"
    And I submit funded transfer transaction "GOOD_TX" of 1 LGO from wallet "WALLET_1A" to wallet "WALLET_2A"
    Then transaction "GOOD_TX" is included on node "NODE_1" in 120 seconds
    And transaction "BAD_TX" is not included in 30 seconds
    Then I stop all nodes
