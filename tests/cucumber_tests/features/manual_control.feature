Feature: Manual control of transactions

  # External command controller:
  #   1) Set CUCUMBER_MANUAL_COMMAND_FILE=/tmp/cucumber-manual-commands.txt
  #   2) Start the scenario
  #   3) Prepare the command file beforehand or add commands on-the-fly while the test is running.
  # Supported commands (one per line):
  #   COIN_SPLIT, wallet '<wallet_name>', outputs <count>, value <amount>
  #   VERIFY, wallet '<wallet_name>', outputs <count>, time_out <duration_seconds>
  #   BALANCE, wallet '<wallet_name>'
  #   EXPORT_FUNDS, wallet '<wallet_name>', value <amount>, output '<path>', include_secret true|false
  #   BALANCE_ALL_WALLETS
  #   BALANCE_ALL_USER_WALLETS
  #   BALANCE_ALL_FUNDING_WALLETS
  #   CLEAR_ENCUMBRANCES, wallet '<wallet_name>'
  #   CLEAR_ENCUMBRANCES_ALL_WALLETS
  #   SEND, num_transactions <count>, value <amount>, from '<wallet_name>', to '<wallet_name>'
  #   VERIFY_MAX, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
  #   VERIFY_MIN, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
  #   CONTINUOUS_ROUND_ROBIN_USER_WALLETS, coin_split_outputs <count>, coin_split_value <amount>, num_transactions <count>, value <amount>, cycles <count>
  #   COIN_SPLIT_ALL_USER_WALLETS, splits_per_wallet <count>, outputs <count>, value <amount>
  #   VERIFY_MIN_AVAILABLE_OUTPUTS_ALL_USER_WALLETS, min_outputs <count>, timeout_seconds <duration_seconds>
  #   CONTINUOUS_NEXT_WALLET_USER_WALLETS, cycles <count>, num_transactions <count>, value <amount>
  #   FAUCET_ALL_USER_WALLETS, rounds <count>
  #   FAUCET_ALL_FUNDING_WALLETS, rounds <count>
  #   CREATE_SNAPSHOT_ALL_NODES, snapshot_name '<snapshot_name>'
  #   CREATE_SNAPSHOT_NODE, snapshot_name '<snapshot_name>', node_name '<node_name>'
  #   RESTART_NODE, node_name '<node_name>'
  #   CRYPTARCHIA_INFO_ALL_NODES
  #   WAIT_ALL_NODES_SYNCED_TO_CHAIN    (requires `I have public cryptarchia endpoint peers:`)
  #   STOP
  #
  # Example command file content, individual steps:
  #   WAIT_ALL_NODES_SYNCED_TO_CHAIN
  #   COIN_SPLIT, wallet 'WALLET_1A', outputs 10, value 100
  #   COIN_SPLIT, wallet 'WALLET_2A', outputs 10, value 100
  #   VERIFY_MAX, wallet 'WALLET_1A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   VERIFY_MAX, wallet 'WALLET_2A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   SEND, num_transactions 5, value 100, from 'WALLET_1A', to 'WALLET_2A'
  #   BALANCE, wallet 'WALLET_1A'
  #   SEND, num_transactions 5, value 100, from 'WALLET_2A', to 'WALLET_1A'
  #   EXPORT_FUNDS, wallet 'WALLET_1A', value 1000, output '/tmp/tui-zone/funds-wallet-1a.json', include_secret true
  #   VERIFY_MAX, wallet 'WALLET_1A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   VERIFY_MAX, wallet 'WALLET_2A', wallet_state_type 'encumbered', outputs 0, time_out 60
  #   STOP
  # Example command file content, continuous steps:
  #   WAIT_ALL_NODES_SYNCED_TO_CHAIN
  #   CREATE_SNAPSHOT_NODE, snapshot_name 'SNAP_TEST_01', node_name 'NODE_1'
  #   RESTART_NODE, node_name 'NODE_1'
  #   CREATE_SNAPSHOT_ALL_NODES, snapshot_name 'SNAP_TEST_02'
  #   CONTINUOUS_ROUND_ROBIN_USER_WALLETS, coin_split_outputs 10, coin_split_value 100, num_transactions 10, value 100, cycles 3
  #   STOP

  @manual_control_transactions
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
    When all nodes have at least 2 blocks and converged to within 1 blocks in 300 seconds
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes

  @manual_control_transactions
  Scenario: Transactions stress manual control
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 1000000      |
      | 2             | 3           | 1000000      |
      | 3             | 3           | 1000000      |
      | 4             | 3           | 1000000      |
      | 5             | 3           | 1000000      |
      | 6             | 3           | 1000000      |
      | 7             | 3           | 1000000      |
      | 8             | 3           | 1000000      |
      | 9             | 3           | 1000000      |
      | 10            | 3           | 1000000      |
    And I have a cluster with capacity of 10 nodes
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
#    And we use IBD peers
    And all peers must be mode online after startup in 30 seconds
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_1       |
      | NODE_4    | 4             | WALLET_4A   | NODE_1       |
      | NODE_5    | 5             | WALLET_5A   | NODE_1       |
      | NODE_6    | 6             | WALLET_6A   | NODE_5       |
      | NODE_7    | 7             | WALLET_7A   | NODE_6       |
      | NODE_8    | 8             | WALLET_8A   | NODE_7       |
      | NODE_9    | 9             | WALLET_9A   | NODE_8       |
      | NODE_10   | 10            | WALLET_10A  | NODE_9       |
    When all nodes have at least 2 blocks and converged to within 1 blocks in 600 seconds
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes

  @devnet_snapshots
  Scenario: Devnet manual snapshot create
    Given I have a devnet cluster with capacity of 2 nodes
    And we join an external network
    And I have a faucet with URL "https://devnet.blockchain.logos.co/web/faucet-backend/"
    And I have initial peers:
      | initial_peer                                                                                   |
      | /ip4/65.108.203.235/udp/3000/quic-v1/p2p/12D3KooWNbZTQ86TZ9MrZ2wm6iUFFj25AFTzFLUD7i6XkZHoUzU8  |
      | /ip4/65.108.203.235/udp/3001/quic-v1/p2p/12D3KooWNhXaH4XTX6Pp66NDQZxZpXYQzeruwwraMvTxojz1QXPJ  |
      | /ip4/65.108.203.235/udp/3002/quic-v1/p2p/12D3KooWNTLPg5uYPKgZCDvzyaWNwZNcwVKmfS2bNv52E9L9P7Hf  |
      | /ip4/65.108.203.235/udp/50001/quic-v1/p2p/12D3KooWMULUG8RXC2esnfLcVzGHohf6KNPSswkCKa1mdpXz4tHH |
#    And I have IBD peers:
#      | ibd_peer                                             |
#      | 12D3KooWNbZTQ86TZ9MrZ2wm6iUFFj25AFTzFLUD7i6XkZHoUzU8 |
#      | 12D3KooWNhXaH4XTX6Pp66NDQZxZpXYQzeruwwraMvTxojz1QXPJ |
#      | 12D3KooWNTLPg5uYPKgZCDvzyaWNwZNcwVKmfS2bNv52E9L9P7Hf |
#      | 12D3KooWMULUG8RXC2esnfLcVzGHohf6KNPSswkCKa1mdpXz4tHH |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 10000 in 30000 seconds
    When I create a snapshot "SNAP_TEST_01" of node "NODE_1"
    When node "NODE_2" is at height 10000 in 30000 seconds
    When I create a snapshot "SNAP_TEST_01" of node "NODE_2"
    Then I stop all nodes

  @devnet_snapshots
  Scenario: Devnet start from snapshot and create new snapshot when synced
    Given I have a devnet cluster with capacity of 2 nodes
    And we join an external network
    And I will initialize started nodes from snapshot "SNAP_TEST_01" source node "NODE_1"
    And I will create a snapshot "SNAP_TEST_03" of all nodes when stopping
    And I have a faucet with URL "https://devnet.blockchain.logos.co/web/faucet-backend/"
    And I have initial peers:
      | initial_peer                                                                                   |
      | /ip4/65.108.203.235/udp/3000/quic-v1/p2p/12D3KooWNbZTQ86TZ9MrZ2wm6iUFFj25AFTzFLUD7i6XkZHoUzU8  |
      | /ip4/65.108.203.235/udp/3001/quic-v1/p2p/12D3KooWNhXaH4XTX6Pp66NDQZxZpXYQzeruwwraMvTxojz1QXPJ  |
      | /ip4/65.108.203.235/udp/3002/quic-v1/p2p/12D3KooWNTLPg5uYPKgZCDvzyaWNwZNcwVKmfS2bNv52E9L9P7Hf  |
      | /ip4/65.108.203.235/udp/50001/quic-v1/p2p/12D3KooWMULUG8RXC2esnfLcVzGHohf6KNPSswkCKa1mdpXz4tHH |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    And I have public cryptarchia endpoint peers:
      | public_cryptarchia_endpoint               | username               | password              |
      | https://devnet.blockchain.logos.co/node/0 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/1 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/2 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/3 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
    When I wait for all nodes to be synced to the chain
    Then I stop all nodes

  @devnet_snapshots
  Scenario: Devnet start from base snapshot advanced restart advance stop
    Given I have a devnet cluster with capacity of 2 nodes
    And we join an external network
    And I will initialize started nodes from snapshot "SNAP_TEST_01" source node "NODE_1"
    And I have a faucet with URL "https://devnet.blockchain.logos.co/web/faucet-backend/"
    And I have initial peers:
      | initial_peer                                                                                   |
      | /ip4/65.108.203.235/udp/3000/quic-v1/p2p/12D3KooWNbZTQ86TZ9MrZ2wm6iUFFj25AFTzFLUD7i6XkZHoUzU8  |
      | /ip4/65.108.203.235/udp/3001/quic-v1/p2p/12D3KooWNhXaH4XTX6Pp66NDQZxZpXYQzeruwwraMvTxojz1QXPJ  |
      | /ip4/65.108.203.235/udp/3002/quic-v1/p2p/12D3KooWNTLPg5uYPKgZCDvzyaWNwZNcwVKmfS2bNv52E9L9P7Hf  |
      | /ip4/65.108.203.235/udp/50001/quic-v1/p2p/12D3KooWMULUG8RXC2esnfLcVzGHohf6KNPSswkCKa1mdpXz4tHH |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    When node "NODE_1" is at height 10200 in 30000 seconds
    When node "NODE_2" is at height 10200 in 30000 seconds
    When I restart node "NODE_1"
    When I restart node "NODE_2"
    When node "NODE_1" is at height 10300 in 30000 seconds
    When node "NODE_2" is at height 10300 in 30000 seconds
    When I query cryptarchia info for all nodes
    Then I stop all nodes

  @devnet_manual_control_transactions
  Scenario: Devnet transactions manual control
    Given I have a devnet cluster with capacity of 2 nodes
    And we join an external network
#    And I will initialize started nodes from snapshot "SNAP_TEST_03" source node "NODE"
    And I have a faucet with URL "https://devnet.blockchain.logos.co/web/faucet-backend/"
    And I have initial peers:
      | initial_peer                                                                                   |
      | /ip4/65.108.203.235/udp/3000/quic-v1/p2p/12D3KooWNbZTQ86TZ9MrZ2wm6iUFFj25AFTzFLUD7i6XkZHoUzU8  |
      | /ip4/65.108.203.235/udp/3001/quic-v1/p2p/12D3KooWNhXaH4XTX6Pp66NDQZxZpXYQzeruwwraMvTxojz1QXPJ  |
      | /ip4/65.108.203.235/udp/3002/quic-v1/p2p/12D3KooWNTLPg5uYPKgZCDvzyaWNwZNcwVKmfS2bNv52E9L9P7Hf  |
      | /ip4/65.108.203.235/udp/50001/quic-v1/p2p/12D3KooWMULUG8RXC2esnfLcVzGHohf6KNPSswkCKa1mdpXz4tHH |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
    And I have public cryptarchia endpoint peers:
      | public_cryptarchia_endpoint               | username               | password              |
      | https://devnet.blockchain.logos.co/node/0 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/1 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/2 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/3 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
    And I request 10 rounds of faucet funds for all user wallets
    When I wait for all nodes to be synced to the chain
    When node "NODE_1" is at height 2000 in 3000 seconds
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes

  @devnet_manual_control_transactions
  Scenario: Devnet transactions stress manual control
    Given I have a devnet cluster with capacity of 10 nodes
    And we join an external network
#    And I will initialize started nodes from snapshot "000_094_856" source node "NODE"
    And I have a faucet with URL "https://devnet.blockchain.logos.co/web/faucet-backend/"
    And I have initial peers:
      | initial_peer                                                                                   |
      | /ip4/65.108.203.235/udp/3000/quic-v1/p2p/12D3KooWNbZTQ86TZ9MrZ2wm6iUFFj25AFTzFLUD7i6XkZHoUzU8  |
      | /ip4/65.108.203.235/udp/3001/quic-v1/p2p/12D3KooWNhXaH4XTX6Pp66NDQZxZpXYQzeruwwraMvTxojz1QXPJ  |
      | /ip4/65.108.203.235/udp/3002/quic-v1/p2p/12D3KooWNTLPg5uYPKgZCDvzyaWNwZNcwVKmfS2bNv52E9L9P7Hf  |
      | /ip4/65.108.203.235/udp/50001/quic-v1/p2p/12D3KooWMULUG8RXC2esnfLcVzGHohf6KNPSswkCKa1mdpXz4tHH |
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_2    | 2             | WALLET_2A   | NODE_1       |
      | NODE_3    | 3             | WALLET_3A   | NODE_2       |
      | NODE_4    | 4             | WALLET_4A   | NODE_3       |
      | NODE_5    | 5             | WALLET_5A   | NODE_4       |
      | NODE_6    | 6             | WALLET_6A   | NODE_5       |
      | NODE_7    | 7             | WALLET_7A   | NODE_6       |
      | NODE_8    | 8             | WALLET_8A   | NODE_7       |
      | NODE_9    | 9             | WALLET_9A   | NODE_8       |
      | NODE_10   | 10            | WALLET_10A  | NODE_9       |
    And I have public cryptarchia endpoint peers:
      | public_cryptarchia_endpoint               | username               | password              |
      | https://devnet.blockchain.logos.co/node/0 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/1 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/2 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
      | https://devnet.blockchain.logos.co/node/3 | env(CCMBR_DEVNET_USER) | env(CCMBR_DEVNET_PWD) |
    And I request 10 rounds of faucet funds for all user wallets
    When I wait for all nodes to be synced to the chain
    When I perform manual control of transactions for all wallets no time-out
    Then I stop all nodes
