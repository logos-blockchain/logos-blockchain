Feature: Fees

  @fees_ci
  Scenario: Gas prices endpoint returns the genesis prices
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000         |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When node "NODE_1" is at height 1 in 180 seconds
    Then gas prices on node "NODE_1" at the genesis block equal the genesis gas prices
    Then I stop all nodes

  @fees_ci
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

  @fees_ci
  Scenario: Wallet fund endpoint funds an inscription with a split-signed fee transfer
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 10000        |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I fund an inscription transaction from wallet "WALLET_1A" via node "NODE_1" as "FUNDED_INSCRIPTION"
    Then transaction "FUNDED_INSCRIPTION" is included on node "NODE_1" in 120 seconds
    Then I stop all nodes

  # The execution gas price after every block must equal the spec's calculation,
  # recomputed here independently. Storage usage must also reach the pricing
  # update and move its price at an epoch boundary. Together these guard the
  # consensus math and that the storage market remains connected to network
  # usage; price viability is covered by ledger market-liveness tests. With
  # k=3 and f=1/2 below, each epoch is 60 slots long. The traffic transactions
  # use zero tip because tips do not drive prices. A separate transaction is
  # prepared through /wallet/fund with a percentage reserve before the
  # storage-price update and submitted afterwards to show that the effective
  # reserve absorbs the higher mandatory fee. The acceptance reserve is 50%
  # because genesis storage price 1 can double to 2 under integer pricing.
  @fees_ci
  Scenario: Execution and storage gas prices respond according to the fee market rules
    Given the cluster uses cryptarchia security parameter 3
    And I have deployment config override "time.slot_duration" as "seconds(1)"
    And I have deployment config override "cryptarchia.slot_activation_coeff.numerator" as "1"
    And I have deployment config override "cryptarchia.slot_activation_coeff.denominator" as "2"
    And the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 10000        |
      | 2             | 1           | 10000        |
      | 3             | 1           | 10000        |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_1    | 2             | WALLET_2A   |              |
      | NODE_1    | 3             | WALLET_3A   |              |
    When node "NODE_1" is at height 1 in 180 seconds
    And I prepare a wallet-funded self-transfer with priority fee percentage 50 from wallet "WALLET_3A" via node "NODE_1" as "BUFFERED"
    And I submit an exactly funded self-transfer from wallet "WALLET_1A" via node "NODE_1" as "TRAFFIC_A"
    And I submit an exactly funded self-transfer from wallet "WALLET_2A" via node "NODE_1" as "TRAFFIC_B"
    And I record per-block gas prices on node "NODE_1" for 125 slots in 300 seconds
    Then transaction "TRAFFIC_A" is included on node "NODE_1" in 60 seconds
    And transaction "TRAFFIC_B" is included on node "NODE_1" in 60 seconds
    And recorded gas prices cross a 60-slot epoch boundary
    And recorded execution gas prices follow the execution market spec reference
    And recorded storage gas prices respond to network usage
    And transaction "BUFFERED" prepared with a 50% priority fee reserve remains funded with a smaller reserve at current prices on node "NODE_1"
    When I submit prepared transaction "BUFFERED" via node "NODE_1"
    Then transaction "BUFFERED" is included on node "NODE_1" in 60 seconds
    Then I stop all nodes

  # The fee rule at its exact boundary: balance == fee is included and
  # debited precisely; one unit short is never included (and today is
  # silently dropped, with no rejection signal to the user).
  @fees_ci
  Scenario: Mandatory fee is charged exactly and an underfunded transaction is never included
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 10000        |
      | 2             | 1           | 10000        |
      | 3             | 1           | 10000        |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
      | NODE_1    | 2             | WALLET_2A   |              |
      | NODE_1    | 3             | WALLET_3A   |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I submit an exactly funded self-transfer from wallet "WALLET_1A" via node "NODE_1" as "EXACT"
    And I submit a self-transfer underfunded by 1 LGO from wallet "WALLET_2A" via node "NODE_1" as "UNDER"
    And I submit an exactly funded self-transfer from wallet "WALLET_3A" via node "NODE_1" as "CONTROL"
    Then transaction "EXACT" is included on node "NODE_1" in 120 seconds
    And transaction "CONTROL" is included on node "NODE_1" in 120 seconds
    And transaction "UNDER" is not included in 30 seconds
    And transaction "UNDER" is not pending in mempool of all nodes in 60 seconds
    And wallet "WALLET_1A" is debited exactly the fee of transaction "EXACT" in 120 seconds
    And wallet "WALLET_3A" is debited exactly the fee of transaction "CONTROL" in 120 seconds
    And wallet "WALLET_2A" has exactly 10000 LGO in 60 seconds
    Then I stop all nodes

  # Concurrent transfers funded from the same wallet must both land. If the
  # wallet reused a pending note, one would conflict and never be included.
  @fees_ci
  Scenario: Concurrent wallet funded transfers are all included
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 10000        |
      | 2             | 1           | 10000        |
      | 3             | 1           | 1            |
      | 4             | 1           | 2            |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | FUNDER_A    |              |
      | NODE_1    | 2             | FUNDER_B    |              |
      | NODE_1    | 3             | RECIPIENT_A |              |
      | NODE_1    | 4             | RECIPIENT_B |              |
    When node "NODE_1" is at height 2 in 240 seconds
    And I concurrently fund the following transfers:
      | amount | funding_wallets  | recipient_wallet | node   | alias |
      | 500    | FUNDER_A,FUNDER_B | RECIPIENT_A     | NODE_1 | TX_A  |
      | 500    | FUNDER_A,FUNDER_B | RECIPIENT_B     | NODE_1 | TX_B  |
    Then transaction "TX_A" is included on node "NODE_1" in 120 seconds
    And transaction "TX_B" is included on node "NODE_1" in 120 seconds
    And wallet "RECIPIENT_A" has exactly 501 LGO in 120 seconds
    And wallet "RECIPIENT_B" has exactly 502 LGO in 120 seconds
    Then I stop all nodes
