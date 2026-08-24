Feature: PoW mining

  # Three-node cluster where the first two nodes provide blend and drive
  # consensus, and the third node mines token-reward PoW. The mined reward is
  # claimed through the blend network, lands in a block, and credits the
  # miner's account.
  #
  # Two deployment tweaks make mining observable in a short test:
  #   * `rate_num = 1` enables the reward payout (the shipped configs disable it
  #     with `rate_num = 0`).
  #   * The reward-difficulty EMA is tuned (factor 1, huge precision and
  #     target-claims-per-block) so the difficulty target eases to the field
  #     maximum within a few blocks, making a winning ticket trivial to find.
  # The miner's `pow.claim_address` is pointed at the tracked wallet on account
  # 3, so the reward lands in a balance the wallet assertions can observe.

  @pow_ci
  Scenario: A non-staking-role miner earns PoW rewards into its account
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 1000         |
      | 2             | 1           | 1000         |
      | 3             | 1           | 1000         |
    And I have a cluster with capacity of 3 nodes
    And the first 2 nodes are declared as blend providers
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
    And I start nodes with wallet resources:
      | node_name | account_index | wallet_name  | connected_to |
      | NODE_1    | 1             | WALLET_1     |              |
      | NODE_2    | 2             | WALLET_2     | NODE_1       |
      | NODE_3    | 3             | WALLET_MINER | NODE_1       |
    When all nodes have at least 5 blocks and converged to within 2 blocks in 300 seconds
    And I start mining on node "NODE_3"
    And I claim PoW rewards on node "NODE_3" as "POW_CLAIM_TX" within 180 seconds
    Then transaction "POW_CLAIM_TX" is included on node "NODE_1" in 120 seconds
    And wallet "WALLET_MINER" has 2000 or more LGO in 120 seconds
    Then I stop all nodes
