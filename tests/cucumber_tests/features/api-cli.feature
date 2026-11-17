Feature: CLI

  @cli_ci
  Scenario: Join Blend via CLI declaration
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 2000         |
    And I have a cluster with capacity of 1 nodes
    And no nodes are declared as blend providers
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name | connected_to |
      | NODE_1    | 1             | WALLET_1A   |              |
    When all nodes have at least 2 blocks and converged to within 1 blocks in 180 seconds
    # The node funds the stake from its funding key, one note for the stake
    # and its genesis note for the fee, and spends a note 4 blocks deep
    And I send 1 transactions of 1000 LGO each from wallet "WALLET_1A" to wallet "NODE_1_WALLET_FUNDING"
    And wallet "WALLET_1A" has all submitted transactions settled in 240 seconds
    And wallet "NODE_1_WALLET_FUNDING" has 2 or more outputs in 120 seconds
    And I record node "NODE_1" height as "TOPUP_HEIGHT"
    Then node "NODE_1" reaches 4 blocks beyond recorded height "TOPUP_HEIGHT" in 120 seconds
    Then I declare node "NODE_1" as blend core node via the CLI binary
    And blend core SDP declaration for node "NODE_1" is included on node "NODE_1"
    And I stop all nodes
