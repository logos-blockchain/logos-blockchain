Feature: Transactions

  @transactions_ci
  Scenario: Funding wallets funds are managed
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 0           | 0            |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 30 tokens of 500 value each
    And I have a cluster with capacity of 2 nodes
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When I log wallet balances for all wallets
    # Funding wallets have allocated funds at scenario start, so we can send transactions to user wallets
    When wallet "NODE_1_WALLET_FUNDING" has 10000 or more LGO in 10 seconds
    When wallet "NODE_2_WALLET_FUNDING" has 10000 or more LGO in 10 seconds
    And I send 1 transactions of 1000 LGO each from wallet "NODE_1_WALLET_FUNDING" to wallet "WALLET_1A"
    And I send 1 transactions of 1000 LGO each from wallet "NODE_2_WALLET_FUNDING" to wallet "WALLET_2A"
    # Now we can split the funds in the user wallets
    When wallet "WALLET_1A" has 1000 or more LGO in 120 seconds
    And wallet "WALLET_2A" has 1000 or more LGO in 120 seconds
    And I perform 1 coin split transactions for each user wallet with 7 outputs of 100 LGO each
    When wallet "WALLET_1A" has 8 or more outputs in 120 seconds
    And wallet "WALLET_2A" has 8 or more outputs in 120 seconds
    # Now we can send multiple outputs back to the funding wallets
    When I send 5 transactions of 100 LGO each from wallet "WALLET_1A" to wallet "NODE_1_WALLET_FUNDING"
    And I send 5 transactions of 100 LGO each from wallet "WALLET_2A" to wallet "NODE_2_WALLET_FUNDING"
    Then wallet "NODE_1_WALLET_FUNDING" has 6 or more outputs in 120 seconds
    And wallet "NODE_2_WALLET_FUNDING" has 6 or more outputs in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Node wallets funds can be drained to user wallets and vice versa
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 0           | 0            |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When I log wallet balances for all wallets
    # Funding wallets have allocated funds at scenario start, so we can send transactions to user wallets
    When I drain all node "NODE_1" wallets into "WALLET_1A"
    When wallet "WALLET_1A" has 20000 or more LGO in 10 seconds
    When I do a coin split for "WALLET_1A" of 250 UTXOs valued at 1 LGO tokens each
    When wallet "WALLET_1A" has 250 or more outputs in 120 seconds
    When I do a coin split for "WALLET_1A" of 250 UTXOs valued at 1 LGO tokens each
    When wallet "WALLET_1A" has 500 or more outputs in 120 seconds
    When I log wallet balances for all wallets
    # Now we can drain all user wallets to node wallets again
    When I drain wallet "WALLET_1A" into "NODE_1_WALLET_FUNDING"
    When wallet "NODE_1_WALLET_FUNDING" has 20000 or more LGO in 120 seconds
    When I log wallet balances for all wallets
    When I drain wallet "WALLET_1A" into "NODE_1_WALLET_FUNDING"
    When I log wallet balances for all wallets
    Then I stop all nodes

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
    And we have a sponsored genesis fee account with 2 tokens of 20000 value each
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
    And wallet "WALLET_1A" has exactly 1500 LGO in 120 seconds
    Then I stop all nodes

  @local_transactions_ci
  Scenario: Two nodes two wallets verify fees multiple transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 10000        |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When I send 1 transactions of 1 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 1 or more outputs in 120 seconds
    When I log wallet balances for all wallets
    When I send 1 transactions of 10 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 120 seconds
    When I log wallet balances for all wallets
    When I send 1 transactions of 100 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 3 or more outputs in 120 seconds
    When I log wallet balances for all wallets
    Then I stop all nodes

  @transactions_ci
  Scenario: Two nodes two wallets multiple outputs one transaction
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 2 tokens of 20000 value each
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
    And wallet "WALLET_1A" has exactly 1500 LGO in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Many nodes with wallets startup
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 2000         |
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
  Scenario: Continuous coin split transactions round robin
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 100000       |
      | 2             | 1           | 100000       |
    And I have a cluster with capacity of 2 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 300 seconds
    When I perform continuous transactions on user wallets with 5 coin split outputs of 2500 LGO, 5 transactions of 900 LGO each for 3 cycles and timeout of 300 seconds
    Then I stop all nodes

  @local_transactions
  Scenario: Local continuous coin split transactions round robin
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 100000       |
      | 2             | 1           | 100000       |
      | 3             | 1           | 100000       |
      | 4             | 1           | 100000       |
      | 5             | 1           | 100000       |
      | 6             | 1           | 100000       |
      | 7             | 1           | 100000       |
      | 8             | 1           | 100000       |
      | 9             | 1           | 100000       |
      | 10            | 1           | 100000       |
    And I have a cluster with capacity of 10 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_01A  |              |
      | NODE_2    | 2             | WALLET_02A  | NODE_1       |
      | NODE_3    | 3             | WALLET_03A  | NODE_1       |
      | NODE_4    | 4             | WALLET_04A  | NODE_1       |
      | NODE_5    | 5             | WALLET_05A  | NODE_1       |
      | NODE_6    | 6             | WALLET_06A  | NODE_5       |
      | NODE_7    | 7             | WALLET_07A  | NODE_6       |
      | NODE_8    | 8             | WALLET_08A  | NODE_7       |
      | NODE_9    | 9             | WALLET_09A  | NODE_8       |
      | NODE_10   | 10            | WALLET_10A  | NODE_9       |
    When node "NODE_1" is at height 2 in 300 seconds
    When I perform continuous transactions on user wallets with 50 coin split outputs of 1000 LGO, 50 transactions of 900 LGO each for 3 cycles
    Then I stop all nodes

  @transactions_ci
  Scenario: Continuous transactions next wallet with coin split
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 50000        |
      | 2             | 2           | 50000        |
    And I have a cluster with capacity of 2 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When all nodes have at least 2 blocks and converged to within 1 blocks in 300 seconds
    And I perform 2 coin split transactions for each user wallet with 10 outputs of 4000 LGO each
    And I verify each wallet has minimum 20 outputs "available" in 300 seconds
    And I perform 3 stress continuous cycles with 20 transactions of 1000 LGO to the next user wallet
    Then I stop all nodes

  @local_transactions
  Scenario: Local continuous transactions next wallet with coin split
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 4           | 300000       |
      | 2             | 4           | 300000       |
      | 3             | 4           | 300000       |
      | 4             | 4           | 300000       |
      | 5             | 4           | 300000       |
      | 6             | 4           | 300000       |
      | 7             | 4           | 300000       |
      | 8             | 4           | 300000       |
      | 9             | 4           | 300000       |
      | 10            | 4           | 300000       |
    And I have a cluster with capacity of 11 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_01A  |              |
      | NODE_2    | 2             | WALLET_02A  | NODE_1       |
      | NODE_3    | 3             | WALLET_03A  | NODE_1       |
      | NODE_4    | 4             | WALLET_04A  | NODE_1       |
      | NODE_5    | 5             | WALLET_05A  | NODE_1       |
      | NODE_6    | 6             | WALLET_06A  | NODE_5       |
      | NODE_7    | 7             | WALLET_07A  | NODE_6       |
      | NODE_8    | 8             | WALLET_08A  | NODE_7       |
      | NODE_9    | 9             | WALLET_09A  | NODE_8       |
      | NODE_10   | 10            | WALLET_10A  | NODE_9       |
    When all nodes have at least 2 blocks and converged to within 0 blocks in 300 seconds
    And I perform 4 coin split transactions for each user wallet with 250 outputs of 1000 LGO each
    And I verify each wallet has minimum 1000 outputs "available" in 3000 seconds
    When I log wallet balances for all wallets
    And I perform 1 stress continuous cycles with 250 transactions of 1 LGO to the next user wallet
    When I log wallet balances for all wallets
    When I start peer node "NODE_LATE" connected to node "NODE_1"
    When all nodes converged to within 0 blocks in 3000 seconds
    When I log wallet balances for all wallets
    Then I stop all nodes

  @transactions_ci
  Scenario: Coin split with many transfers to other
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 4           | 30000        |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
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
    When wallet "WALLET_1A" has 70000 or less LGO in 180 seconds
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Two fork chains join later and preserve persisted state wallet balances
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 2500         |
      | 2             | 2           | 2500         |
      | 4             | 2           | 2500         |
      | 5             | 2           | 2500         |
      | 7             | 0           | 0            |
      | 8             | 0           | 0            |
    And I have a cluster with capacity of 5 nodes
    And no nodes are declared as blend providers
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And we will have distinct node groups to query wallet balances:
      | group_name | node_name       |
      | FORK_A     | NODE_1          |
      | FORK_A     | NODE_2          |
      | FORK_B     | NODE_4          |
      | FORK_B     | NODE_5          |
      | FORK_BURN  | NODE_BURN       |
      | FORK_BURN  | NODE_BURN_BUDDY |
    And I start nodes with wallet resources:
      | node_name       | account_index | wallet_name       | connected_to |
      | NODE_1          | 1             | WALLET_1A         |              |
      | NODE_2          | 2             | WALLET_2A         | NODE_1       |
      | NODE_4          | 4             | WALLET_4A         |              |
      | NODE_5          | 5             | WALLET_5A         | NODE_4       |
      | NODE_BURN       | 7             | WALLET_BURN       |              |
      | NODE_BURN_BUDDY | 8             | WALLET_BURN_BUDDY | NODE_BURN    |
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
    When wallet "WALLET_1A" has all submitted transactions settled in 240 seconds
    And wallet "WALLET_2A" has all submitted transactions settled in 240 seconds
    And wallet "WALLET_4A" has all submitted transactions settled in 240 seconds
    And wallet "WALLET_5A" has all submitted transactions settled in 240 seconds
    # Each wallet should now have 3 outputs: one received + two change (allow for fees)
    When wallet "WALLET_1A" has 3 or more outputs in 120 seconds
    And wallet "WALLET_2A" has 3 or more outputs in 60 seconds
    And wallet "WALLET_4A" has 3 or more outputs in 60 seconds
    And wallet "WALLET_5A" has 3 or more outputs in 60 seconds
    # Wait for more blocks to be mined on each fork to ensure the chains are well established
    When node "NODE_1" is at height 5 in 180 seconds
    And node "NODE_4" is at height 5 in 180 seconds
    # Bridge the two forks
    When I start peer node "NODE_JOIN" connected to node "NODE_1" and node "NODE_4"
    # Wait for all nodes to converge on the same chain and for the transactions to be mined in the new combined chain
    When node "NODE_JOIN" is at height 8 in 180 seconds
    # Previous wallet state should be restored for the re-orged chain.
    When wallet "WALLET_1A" has 3 or more outputs in 120 seconds
    And wallet "WALLET_2A" has 3 or more outputs in 10 seconds
    And wallet "WALLET_4A" has 3 or more outputs in 10 seconds
    And wallet "WALLET_5A" has 3 or more outputs in 10 seconds
    When wallet "WALLET_1A" has 4300 or less LGO in 10 seconds
    And wallet "WALLET_2A" has 4300 or less LGO in 10 seconds
    And wallet "WALLET_4A" has 4300 or less LGO in 10 seconds
    And wallet "WALLET_5A" has 4300 or less LGO in 10 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Large coin split followed by large coin join transaction succeed
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 10          | 25000        |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 4 tokens of 20000 value each
    And I have a cluster with capacity of 2 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    # Coin split (This will use multiple inputs to create many outputs)
    # Maximum number of outputs per transaction is 255 (as per encoding limits)
    And I do a coin split for "WALLET_1A" of 250 UTXOs valued at 1000 LGO tokens each
    # Do a transfer to the other wallet
    When wallet "WALLET_1A" has 250 or more outputs in 180 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 1 or more outputs in 180 seconds
    When wallet "WALLET_1A" has 1 or more outputs and 1000 or more LGO in 10 seconds
    # Coin join all outputs back to one output
    # Maximum number of inputs per transaction is 255 (as per encoding limits)
    And I send 1 transactions of 249000 LGO each from wallet "WALLET_1A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    When wallet "WALLET_1A" has exactly 1 outputs and 249000 LGO in 20 seconds
    And tracked wallet fees equal sponsored fee account spent fees
    Then I stop all nodes

  @transactions_ci
  Scenario: Transaction with multiple inputs and multiple outputs
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
      | 2             | 0           | 0            |
    And we have a sponsored genesis fee account with 2 tokens of 20000 value each
    And I have a cluster with capacity of 2 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    # Maximum number of outputs per transaction is 255 (as per encoding limits)
    # The wallet has 3 x 100000 = 300000 LGO outputs, so this transaction is valid and should be mined; it
    # should use all 3x its outputs and create 250 new outputs of 1000 LGO each.
    And I send one transaction with 250 outputs of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 250 or more outputs in 60 seconds
    And wallet "WALLET_1A" has exactly 50000 LGO in 20 seconds
    And tracked wallet fees equal sponsored fee account spent fees
    Then I stop all nodes

  @transactions_ci
  Scenario: Preverification rejects a structurally invalid transaction
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I submit a stateless-invalid transfer transaction "BAD_TX" to node "NODE_1"
    Then transaction "BAD_TX" is rejected during preverification
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

  Scenario: Continuous wallet funding survives natural forks without reusing in-flight notes
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 250000       |
      | 2             | 2           | 250000       |
      | 3             | 2           | 250000       |
    And I have a cluster with capacity of 3 nodes
    And no nodes are declared as blend providers
    And we use IBD peers
    And I have deployment config override "time.slot_duration" as "seconds(1)"
    And I have deployment config override "cryptarchia.slot_activation_coeff.numerator" as "1"
    And I have deployment config override "cryptarchia.slot_activation_coeff.denominator" as "2"
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_1       |
    When node "NODE_1" is at height 2 in 240 seconds
    And wallet "WALLET_1A" sends 10 notes of 22000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
    And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 240 seconds
    And I fund 200 transactions paying 100 LGO from node "NODE_1" wallet to wallet "WALLET_2A" retrying every 1 seconds for 600 seconds each as prefix "SOAK"
    Then all transactions with prefix "SOAK" are finalized on node "NODE_1" in 900 seconds
    And I stop all nodes

  Scenario: Wallet double-hands a note after restart
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 25000        |
      | 4             | 2           | 25000        |
    And I have a cluster with capacity of 2 nodes
    And no nodes are declared as blend providers
    And we use IBD peers
    And I have deployment config override "time.slot_duration" as "seconds(1)"
    And I have deployment config override "cryptarchia.slot_activation_coeff.numerator" as "1"
    And I have deployment config override "cryptarchia.slot_activation_coeff.denominator" as "2"
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   | NODE_4       |
      | NODE_4    | 4             | WALLET_4A   |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And wallet "WALLET_1A" sends 1 notes of 9000 LGO to node "NODE_1" funding wallet as "TOPUP_A"
    And wallet "WALLET_1A" sends 1 notes of 8000 LGO to node "NODE_1" funding wallet as "TOPUP_B"
    And transaction "TOPUP_A" is included on node "NODE_1" in 240 seconds
    And transaction "TOPUP_B" is included on node "NODE_1" in 240 seconds
    When I stop node "NODE_4"
    And I fund a transaction paying 100 LGO from node "NODE_1" wallet to wallet "WALLET_4A" as "DH_1"
    And transaction "DH_1" is included on node "NODE_1" in 240 seconds
    And node "NODE_1" alone reaches height 8 in 240 seconds
    When I stop node "NODE_1"
    And I restart node "NODE_4"
    And node "NODE_4" alone reaches height 14 in 300 seconds
    When I restart node "NODE_1"
    And node "NODE_1" is at height 14 in 240 seconds
    And I fund a transaction paying 150 LGO from node "NODE_1" wallet to wallet "WALLET_4A" as "DH_2"
    Then all transactions with prefix "DH" are finalized on node "NODE_1" in 300 seconds
    And I stop all nodes
