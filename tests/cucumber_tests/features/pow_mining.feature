Feature: PoW mining

  # The network is started with two staking nodes that drive consensus and
  # provide blend. A third node joins the running network later, syncs, and only
  # then mines token-reward PoW. Its mined reward is claimed through the blend
  # network, lands in a block, and credits its account — which starts from a
  # balance recorded before mining, so the increase is asserted explicitly.
  #
  # Two deployment tweaks make mining observable in a short test:
  #   * `rate_num = 1` enables the reward payout (the shipped configs disable it
  #     with `rate_num = 0`).
  #   * The reward-difficulty EMA is tuned (factor 1, huge precision and
  #     target-claims-per-block) so the difficulty target eases to the field
  #     maximum within a few blocks, making a winning ticket trivial to find.
  # The miner's `pow.claim_address` is pointed at the tracked wallet on account
  # 3, so the reward lands in a balance the wallet assertions can observe. That
  # wallet is bound to NODE_1 for querying, since the reward note is on the
  # shared chain and visible from any node.

  @pow_ci
  Scenario: A late-joining node mines PoW rewards while two nodes run consensus
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000         |
      | 2             | 1           | 1000         |
      | 3             | 1           | 1000         |
    And I have a cluster with capacity of 3 nodes
    And the first 2 nodes are declared as blend providers
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
    And I have deployment config override "time.slot_duration" as "seconds(1)"
    And I have deployment config override "cryptarchia.slot_activation_coeff.numerator" as "1"
    And I have deployment config override "cryptarchia.slot_activation_coeff.denominator" as "2"
    And I have deployment config override "cryptarchia.pow_config.reward.rate_num" as "1"
    And I have deployment config override "cryptarchia.pow_config.reward.epoch_reward_genesis" as "1000000"
    And I have deployment config override "cryptarchia.pow_config.reward.reward_pool_genesis" as "1000000000"
    And I have deployment config override "cryptarchia.pow_config.reward.initial_difficulty_seed" as "0"
    And I have deployment config override "cryptarchia.pow_config.reward.ema_smoothing_factor" as "1"
    And I have deployment config override "cryptarchia.pow_config.reward.ema_smoothing_precision" as "1000000000000000000"
    And I have deployment config override "cryptarchia.pow_config.reward.target_claims_per_block" as "1000000000000000000"
    And I have user config override "pow.claim_address" as "wallet_pk(3)"
    # Start the network with the two staking nodes only; they drive consensus
    # and blend. WALLET_MINER (account 3, the miner's reward account) is bound to
    # NODE_1 so its balance can be queried while NODE_3 does the mining.
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name  | connected_to |
      | NODE_1    | 1             | WALLET_1     |              |
      | NODE_1    | 3             | WALLET_MINER |              |
      | NODE_2    | 2             | WALLET_2     | NODE_1       |
    # Let the two-node chain advance so the reward difficulty eases to a
    # mineable target before the miner joins.
    When node "NODE_1" is at height 5 in 300 seconds
    # The miner joins the running network and syncs to the tip.
    And I start peer node "NODE_3" connected to node "NODE_1"
    And node "NODE_3" is at height 6 in 180 seconds
    # Record the miner's balance, then mine and claim; assert the balance grew.
    And I record the balance of wallet "WALLET_MINER" as "MINER_BASELINE"
    And I start mining on node "NODE_3"
    And I claim PoW rewards on node "NODE_3" as "POW_CLAIM_TX" within 180 seconds
    Then transaction "POW_CLAIM_TX" is included on node "NODE_1" in 120 seconds
    And wallet "WALLET_MINER" balance increased by at least 1000 over "MINER_BASELINE" in 120 seconds
    Then I stop all nodes
