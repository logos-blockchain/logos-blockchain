Feature: Mempool diagnostics

  @local_transactions
  Scenario: Local continuous coin split transactions round robin
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 6000000      |
      | 2             | 3           | 6000000      |
      | 3             | 3           | 6000000      |
      | 4             | 3           | 6000000      |
      | 5             | 3           | 6000000      |
      | 6             | 3           | 6000000      |
      | 7             | 3           | 6000000      |
      | 8             | 3           | 6000000      |
      | 9             | 3           | 6000000      |
      | 10            | 3           | 6000000      |
    And I will have tokio console profile nodes:
      | node_name | record_raw |
      | NODE_LATE | true       |
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
    And I log diagnostic identities
    When node "NODE_1" is at height 2 in 300 seconds
    When I perform continuous transactions on user wallets with 250 coin split outputs of 20000 LGO, 250 transactions of 8000 LGO each for 3 cycles with 3 epochs headroom
    When I log wallet balances for all wallets
    When I start peer node "NODE_LATE" connected to node "NODE_1"
    When all nodes converged to within 0 blocks in 3000 seconds
    When I log wallet balances for all wallets
    Then I stop all nodes

  @local_transactions
  Scenario: Local continuous transactions next wallet with coin split
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 4           | 6000000      |
      | 2             | 4           | 6000000      |
      | 3             | 4           | 6000000      |
      | 4             | 4           | 6000000      |
      | 5             | 4           | 6000000      |
      | 6             | 4           | 6000000      |
      | 7             | 4           | 6000000      |
      | 8             | 4           | 6000000      |
      | 9             | 4           | 6000000      |
      | 10            | 4           | 6000000      |
    And I will have tokio console profile nodes:
      | node_name | record_raw |
      | NODE_LATE | true       |
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
    And I log diagnostic identities
    When all nodes have at least 2 blocks and converged to within 0 blocks in 300 seconds
    And I perform 4 coin split transactions for each user wallet with 250 outputs of 20000 LGO each
    And I verify each wallet has minimum 1000 outputs "available" in 3000 seconds
    When I log wallet balances for all wallets
    And I perform 3 stress continuous cycles with 250 transactions of 1 LGO to the next user wallet with 3 epochs headroom
    When I log wallet balances for all wallets
    When I start peer node "NODE_LATE" connected to node "NODE_1"
    When all nodes converged to within 0 blocks in 3000 seconds
    When I log wallet balances for all wallets
    Then I stop all nodes

  @mempool_diagnostic @local_transactions
  Scenario Outline: Mempool loading independent transactions parameter_set=<parameter_set>
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 110000000    |
      | 2             | 1           | 110000000    |
      | 3             | 1           | 110000000    |
      | 4             | 1           | 110000000    |
      | 5             | 1           | 110000000    |
      | 6             | 1           | 110000000    |
      | 7             | 1           | 110000000    |
      | 8             | 1           | 110000000    |
      | 9             | 1           | 110000000    |
      | 10            | 1           | 110000000    |
    And I have a cluster with capacity of 12 nodes
    And no nodes are declared as blend providers
    And the cluster uses diagnostic parameter set "<parameter_set>"
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
    And I start peer node "NODE_11" connected to node "NODE_10"
    And I start peer node "NODE_12" connected to node "NODE_11"
    And I log diagnostic identities
    When all nodes have at least 2 blocks and converged to within 0 blocks in 300 seconds
    When I split available funds in each user wallet into <transactions_per_wallet> approximately equal outputs
    And I verify each wallet has minimum <transactions_per_wallet> outputs "available" in 300 seconds
    And I record mempool pending counts for "independent" workload at "before_load"
    When I perform <rounds> independent next-wallet rounds with <transactions_per_wallet> transactions per wallet at 1 LGO each and <epochs_headroom> epochs fee headroom
    And I record mempool pending counts for "independent" workload at "after_load"
    And I observe the "independent" mempool drain for <drain_epochs> epochs
    Then I stop all nodes

    Examples:
      | parameter_set          | transactions_per_wallet | rounds | epochs_headroom | drain_epochs |
      | fast_repro             | 10                      | 3      | 1               | 2            |
      | testnet_representative | 250                     | 31     | 3               | 2            |

  @mempool_diagnostic @local_transactions
  Scenario Outline: Mempool loading dependent transactions parameter_set=<parameter_set>
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 110000000    |
      | 2             | 1           | 110000000    |
      | 3             | 1           | 110000000    |
      | 4             | 1           | 110000000    |
      | 5             | 1           | 110000000    |
      | 6             | 1           | 110000000    |
      | 7             | 1           | 110000000    |
      | 8             | 1           | 110000000    |
      | 9             | 1           | 110000000    |
      | 10            | 1           | 110000000    |
    And I have a cluster with capacity of 12 nodes
    And no nodes are declared as blend providers
    And the cluster uses diagnostic parameter set "<parameter_set>"
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
    And I start peer node "NODE_11" connected to node "NODE_10"
    And I start peer node "NODE_12" connected to node "NODE_11"
    And I log diagnostic identities
    When all nodes have at least 2 blocks and converged to within 0 blocks in 300 seconds
    When I split available funds in each user wallet into <transactions_per_wallet> approximately equal outputs
    And I verify each wallet has minimum <transactions_per_wallet> outputs "available" in 300 seconds
    And I record mempool pending counts for "dependent" workload at "before_load"
    When I perform <rounds> dependent next-wallet rounds with <transactions_per_wallet> transactions per wallet at 1 LGO each and <epochs_headroom> epochs fee headroom
    And I record mempool pending counts for "dependent" workload at "after_load"
    And I observe the "dependent" mempool drain for <drain_epochs> epochs
    Then I stop all nodes

    Examples:
      | parameter_set          | transactions_per_wallet | rounds | epochs_headroom | drain_epochs |
      | fast_repro             | 10                      | 3      | 1               | 2            |
      | testnet_representative | 250                     | 25     | 3               | 2            |
