Feature: PoW mining

  # Two staking nodes start normally (as in the Cryptarchia features) and only
  # exist to establish the Blend-backed chain. A third node joins the running
  # network later and mines token-reward PoW. Its mined reward is claimed
  # through the Blend network, lands in a block, and credits its account — which
  # starts empty, so the increase is exactly the claimed reward.
  #
  # Wallet resources are only needed on the late-joining mining node: it carries
  # a mining wallet (`is_mining_wallet`), whose public key becomes the node's
  # `pow.claim_address`, plus an unrelated second wallet to show only one mining
  # wallet is configured.
  #
  # Two deployment tweaks make mining observable in a short test:
  #   * `rate_num = 1` enables the reward payout (the shipped configs disable it
  #     with `rate_num = 0`).
  #   * The reward-difficulty EMA is tuned (factor 1, huge precision and
  #     target-claims-per-block) so the difficulty target eases to the field
  #     maximum within a few blocks, making a winning ticket trivial to find.

  @blend_ci
  Scenario: A late-joining node mines PoW rewards while two nodes run consensus
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 0           | 0            |
      | 2             | 1           | 1000         |
    And I have a cluster with capacity of 3 nodes
    And the first 2 nodes are declared as blend providers
    And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(0)"
    And I have deployment config override "time.slot_duration" as "seconds(1)"
    And I have deployment config override "cryptarchia.slot_activation_coeff.numerator" as "1"
    And I have deployment config override "cryptarchia.slot_activation_coeff.denominator" as "2"
    And I have deployment config override "cryptarchia.pow_config.reward.rate_num" as "1"
    And I have deployment config override "cryptarchia.pow_config.reward.epoch_reward_genesis" as "1000000"
    # Fund the pool for exactly one claim (pool == per-claim reward): with the
    # difficulty eased to the field maximum the miner finds tickets extremely
    # fast, and the claim is capped to `reward_pool / reward` tickets — so a
    # single-ticket cap keeps the reward-claim transaction small and fast.
    And I have deployment config override "cryptarchia.pow_config.reward.reward_pool_genesis" as "1000000"
    And I have deployment config override "cryptarchia.pow_config.reward.initial_difficulty_seed" as "0"
    And I have deployment config override "cryptarchia.pow_config.reward.ema_smoothing_factor" as "1"
    And I have deployment config override "cryptarchia.pow_config.reward.ema_smoothing_precision" as "1000000000000000000"
    And I have deployment config override "cryptarchia.pow_config.reward.target_claims_per_block" as "1000000000000000000"
    # Start the network with the two staking nodes only; they drive consensus
    # and blend and hold no wallet resources.
    And I start node "NODE_1"
    And I start peer node "NODE_2" connected to node "NODE_1"
    # Let the two-node chain advance so the reward difficulty eases to a
    # mineable target before the miner joins.
    When node "NODE_1" is at height 5 in 300 seconds
    # The miner joins the running network with its wallet resources; WALLET_MINER
    # (account 1) is the mining wallet and becomes the node's pow.claim_address.
    And I start mining nodes with wallet resources:
      | node_name | account_index | wallet_name  | is_mining_wallet | connected_to |
      | NODE_3    | 1             | WALLET_MINER | true             | NODE_1       |
      | NODE_3    | 2             | OTHER_WALLET | false            | NODE_1       |
    And node "NODE_3" is at height 6 in 180 seconds
    # Record the miner's balance, then mine and claim; assert the balance grew.
    And I record the balance of wallet "WALLET_MINER" as "MINER_BASELINE"
    And I start mining on node "NODE_3"
    # Wait until at least one ticket is claimable, then stop mining before
    # claiming: at the eased difficulty the miner finds tickets very fast, so
    # halting it first frees the service loop to process the claim promptly.
    And node "NODE_3" has at least 1 claimable PoW rewards within 120 seconds
    And I stop mining on node "NODE_3"
    And I claim PoW rewards on node "NODE_3" as "POW_CLAIM_TX" within 120 seconds
    Then transaction "POW_CLAIM_TX" is included on node "NODE_1" in 120 seconds
    # The balance grows by exactly the reward the claim transaction paid to the
    # account and no more, so the increase is strictly the claimed reward.
    And wallet "WALLET_MINER" balance increased by exactly the reward from claim "POW_CLAIM_TX" over "MINER_BASELINE" in 120 seconds
    Then I stop all nodes
