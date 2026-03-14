Feature: Transactions

  @transactions_ci
  Scenario: Two nodes two wallets multiple transactions
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    And I send 2 transactions of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 120 seconds
    And I send 2 transactions of 250 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 1300 or more LGO in 120 seconds
    Then I stop all nodes

  @transactions_ci
  Scenario: Two nodes two wallets multiple outputs one transaction
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 2           | 1000         |
      | 2             | 0           | 0            |
    And I have a cluster with capacity of 2 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    And I send one transaction with 2 outputs of 500 LGO each from wallet "WALLET_1A" to wallet "WALLET_2A"
    When wallet "WALLET_2A" has 2 or more outputs in 120 seconds
    And I send 2 transactions of 250 LGO each from wallet "WALLET_2A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 1300 or more LGO in 120 seconds
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
    And I have a cluster with capacity of 2 nodes
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
    When node "NODE_1" is at height 2 in 180 seconds
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
  Scenario: Coin join transaction never mined
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
    When node "NODE_1" is at height 2 in 180 seconds
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
    When wallet "WALLET_1A" has 56000 or less LGO in 180 seconds
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    # Breaks here - the transaction that includes more than one outputs is nevert mined
    And I send 1 transactions of 2000 LGO each from wallet "WALLET_1A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    And I send 1 transactions of 47000 LGO each from wallet "WALLET_1A" to wallet "WALLET_1A"
    When wallet "WALLET_1A" has 0 or less encumbered outputs in 60 seconds
    Then I stop all nodes

  @local_host @undefined_behaviour
  Scenario: Coin split transaction not mined
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
#    And all peers must be mode online after startup in 30 seconds
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

  # External command controller:
  #   1) Set CUCUMBER_MANUAL_COMMAND_FILE=/tmp/cucumber-manual-commands.txt
  #   2) Start the scenario
  #   3) Prepare the command file beforehand or add commands on-the-fly while the test is running.
  # Supported commands (one per line):
  #   COIN_SPLIT, wallet '<wallet_name>', outputs <count>, value <amount>
  #   VERIFY, wallet '<wallet_name>', outputs <count>, time_out <duration_seconds>
  #   BALANCE, wallet '<wallet_name>'
  #   BALANCE_ALL_WALLETS
  #   BALANCE_ALL_USER_WALLETS
  #   BALANCE_ALL_FUNDING_WALLETS
  #   CLEAR_ENCUMBRANCES, wallet '<wallet_name>'
  #   CLEAR_ENCUMBRANCES_ALL_WALLETS
  #   SEND, transactions <count>, value <amount>, from '<wallet_name>', to '<wallet_name>'
  #   VERIFY_MAX, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
  #   VERIFY_MIN, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
  #   CONTINUOUS_USER_WALLETS, coin_split_outputs <count>, coin_split_value <amount>, transactions <count>, value <amount>, cycles <count>
  #   CONTINUOUS_FUNDING_WALLETS, coin_split_outputs <count>, coin_split_value <amount>, transactions <count>, value <amount>, cycles <count>
  #   FAUCET_ALL_USER_WALLETS, rounds <count>
  #   FAUCET_ALL_FUNDING_WALLETS, rounds <count>
  #   CRYPTARCHIA_INFO_ALL_NODES
  #   WAIT_ALL_NODES_SYNCED_TO_CHAIN    (requires `I have public cryptarchia endpoint peers:`)
  #   STOP
  #
  # Example command file content, individual steps:
  #   COIN_SPLIT, wallet 'WALLET_1A', outputs 10, value 5000
  #   COIN_SPLIT, wallet 'WALLET_2A', outputs 10, value 5000
  #   VERIFY_MAX, wallet 'WALLET_1A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   VERIFY_MAX, wallet 'WALLET_2A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   SEND, transactions 5, value 2000, from 'WALLET_1A', to 'WALLET_2A'
  #   BALANCE, wallet 'WALLET_1A'
  #   SEND, transactions 5, value 2000, from 'WALLET_2A', to 'WALLET_1A'
  #   VERIFY_MAX, wallet 'WALLET_1A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   VERIFY_MAX, wallet 'WALLET_2A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   STOP
  # Example command file content, continuous steps:
  #   CONTINUOUS_USER_WALLETS, coin_split_outputs 20, coin_split_value 1000, transactions 10, value 900, cycles 3
  #   STOP

  @transactions_manual_control
  Scenario: Transactions manual control
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 1000000      |
      | 2             | 3           | 1000000      |
    And I have a cluster with capacity of 2 nodes
#    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 2 in 180 seconds
    When I perform manual control of transactions for all wallets
    Then I stop all nodes

  @transactions_manual_control
  Scenario: Transactions stress manual control
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 1000000      |
      | 2             | 3           | 1000000      |
      | 3             | 3           | 1000000      |
      | 4             | 3           | 1000000      |
      | 5             | 3           | 1000000      |
    And I have a cluster with capacity of 20 nodes
    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_1       |
      | NODE_4    | 4             | WALLET_4A   | NODE_1       |
      | NODE_5    | 5             | WALLET_5A   | NODE_1       |
#      | NODE_6    | 6             | WALLET_6A   | NODE_5       |
#      | NODE_7    | 7             | WALLET_7A   | NODE_6       |
#      | NODE_8    | 8             | WALLET_8A   | NODE_7       |
#      | NODE_9    | 9             | WALLET_9A   | NODE_8       |
#      | NODE_10   | 10            | WALLET_10A  | NODE_9       |
#      | NODE_11   | 11            | WALLET_11A  | NODE_10      |
#      | NODE_12   | 12            | WALLET_12A  | NODE_11      |
#      | NODE_13   | 13            | WALLET_13A  | NODE_12      |
#      | NODE_14   | 14            | WALLET_14A  | NODE_13      |
#      | NODE_15   | 15            | WALLET_15A  | NODE_14      |
#      | NODE_16   | 16            | WALLET_16A  | NODE_15      |
#      | NODE_17   | 17            | WALLET_17A  | NODE_16      |
#      | NODE_18   | 18            | WALLET_18A  | NODE_17      |
#      | NODE_19   | 19            | WALLET_19A  | NODE_18      |
#      | NODE_20   | 20            | WALLET_20A  | NODE_19      |
#    And I request 3 rounds of faucet funds for all wallets
    When all nodes have at least 2 blocks and converged to within 1 blocks in 300 seconds
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes

  @transactions_manual_control
  Scenario: Transactions devnet manual control
    Given I have a devnet cluster with capacity of 2 nodes
    And we join an external network
    And I have a faucet with URL "https://devnet.blockchain.logos.co" username "strode" and password "SzH3RP7zdVQs8LCb"
    And I have initial peers:
      | initial_peer                                                                                |
      | /ip4/65.109.51.37/udp/3000/quic-v1/p2p/12D3KooWL7a8LBbLRYnabptHPFBCmAs49Y7cVMqvzuSdd43tAJk8 |
      | /ip4/65.109.51.37/udp/3001/quic-v1/p2p/12D3KooWPLeAcachoUm68NXGD7tmNziZkVeMmeBS5NofyukuMRJh |
      | /ip4/65.109.51.37/udp/3002/quic-v1/p2p/12D3KooWKFNe4gS5DcCcRUVGdMjZp3fUWu6q6gG5R846Ui1pccHD |
      | /ip4/65.109.51.37/udp/3003/quic-v1/p2p/12D3KooWAnriLgXyQnGTYz1zPWPkQL3rthTKYLzuAP7MMnbgsxzR |
#    And I have IBD peers:
#      | ibd_peer                                             |
#      | 12D3KooWL7a8LBbLRYnabptHPFBCmAs49Y7cVMqvzuSdd43tAJk8 |
#      | 12D3KooWPLeAcachoUm68NXGD7tmNziZkVeMmeBS5NofyukuMRJh |
#      | 12D3KooWKFNe4gS5DcCcRUVGdMjZp3fUWu6q6gG5R846Ui1pccHD |
#      | 12D3KooWAnriLgXyQnGTYz1zPWPkQL3rthTKYLzuAP7MMnbgsxzR |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    And I request 3 rounds of faucet funds for all user wallets
    And I have public cryptarchia endpoint peers:
      | public_cryptarchia_endpoint               | username | password         |
      | https://devnet.blockchain.logos.co/node/0 | strode   | SzH3RP7zdVQs8LCb |
      | https://devnet.blockchain.logos.co/node/1 | strode   | SzH3RP7zdVQs8LCb |
      | https://devnet.blockchain.logos.co/node/2 | strode   | SzH3RP7zdVQs8LCb |
      | https://devnet.blockchain.logos.co/node/3 | strode   | SzH3RP7zdVQs8LCb |
    When I wait for all nodes to be synced to the chain
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes

  @transactions_manual_control
  Scenario: Transactions stress devnet manual control
    Given I have a devnet cluster with capacity of 20 nodes
    And we join an external network
    And I have a faucet with URL "https://devnet.blockchain.logos.co" username "strode" and password "SzH3RP7zdVQs8LCb"
    And I have initial peers:
      | initial_peer                                                                                |
      | /ip4/65.109.51.37/udp/3000/quic-v1/p2p/12D3KooWL7a8LBbLRYnabptHPFBCmAs49Y7cVMqvzuSdd43tAJk8 |
      | /ip4/65.109.51.37/udp/3001/quic-v1/p2p/12D3KooWPLeAcachoUm68NXGD7tmNziZkVeMmeBS5NofyukuMRJh |
      | /ip4/65.109.51.37/udp/3002/quic-v1/p2p/12D3KooWKFNe4gS5DcCcRUVGdMjZp3fUWu6q6gG5R846Ui1pccHD |
      | /ip4/65.109.51.37/udp/3003/quic-v1/p2p/12D3KooWAnriLgXyQnGTYz1zPWPkQL3rthTKYLzuAP7MMnbgsxzR |
#    And I have IBD peers:
#      | ibd_peer                                             |
#      | 12D3KooWL7a8LBbLRYnabptHPFBCmAs49Y7cVMqvzuSdd43tAJk8 |
#      | 12D3KooWPLeAcachoUm68NXGD7tmNziZkVeMmeBS5NofyukuMRJh |
#      | 12D3KooWKFNe4gS5DcCcRUVGdMjZp3fUWu6q6gG5R846Ui1pccHD |
#      | 12D3KooWAnriLgXyQnGTYz1zPWPkQL3rthTKYLzuAP7MMnbgsxzR |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_2       |
      | NODE_4    | 4             | WALLET_4A   | NODE_3       |
      | NODE_5    | 5             | WALLET_5A   | NODE_4       |
#      | NODE_6    | 6             | WALLET_6A   | NODE_5       |
#      | NODE_7    | 7             | WALLET_7A   | NODE_6       |
#      | NODE_8    | 8             | WALLET_8A   | NODE_7       |
#      | NODE_9    | 9             | WALLET_9A   | NODE_8       |
#      | NODE_10   | 10            | WALLET_10A  | NODE_9       |
#      | NODE_11   | 11            | WALLET_11A  | NODE_10      |
#      | NODE_12   | 12            | WALLET_12A  | NODE_11      |
#      | NODE_13   | 13            | WALLET_13A  | NODE_12      |
#      | NODE_14   | 14            | WALLET_14A  | NODE_13      |
#      | NODE_15   | 15            | WALLET_15A  | NODE_14      |
#      | NODE_16   | 16            | WALLET_16A  | NODE_15      |
#      | NODE_17   | 17            | WALLET_17A  | NODE_16      |
#      | NODE_18   | 18            | WALLET_18A  | NODE_17      |
#      | NODE_19   | 19            | WALLET_19A  | NODE_18      |
#      | NODE_20   | 20            | WALLET_20A  | NODE_19      |
#    And I request 3 rounds of faucet funds for all wallets
    And I have public cryptarchia endpoint peers:
      | public_cryptarchia_endpoint               | username | password         |
      | https://devnet.blockchain.logos.co/node/0 | strode   | SzH3RP7zdVQs8LCb |
      | https://devnet.blockchain.logos.co/node/1 | strode   | SzH3RP7zdVQs8LCb |
      | https://devnet.blockchain.logos.co/node/2 | strode   | SzH3RP7zdVQs8LCb |
      | https://devnet.blockchain.logos.co/node/3 | strode   | SzH3RP7zdVQs8LCb |
    When I wait for all nodes to be synced to the chain
    And I request 2 rounds of faucet funds for all user wallets
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes
