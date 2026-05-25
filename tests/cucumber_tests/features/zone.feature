Feature: Zone SDK

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_sequencer_publish_and_indexer_read
  Scenario: Publish messages and read them from the zone indexer
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data           |
      | MSG_1 | Hello, Zone!   |
      | MSG_2 | Second message |
      | MSG_3 | Third message  |
    Then all zone messages are safe in 120 seconds
    And all zone messages are finalized in 180 seconds
    And the zone indexer returns messages in this order:
      | alias |
      | MSG_1 |
      | MSG_2 |
      | MSG_3 |
    When sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 0                 | 0               | SEQ_B                 |
    Then zone transaction "CHANNEL_CONFIG_1" is included in 180 seconds
    And zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_sequencer_checkpoint_resume
  Scenario: Resume zone sequencer from checkpoint
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A      |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data      |
      | MSG_1 | Message 1 |
      | MSG_2 | Message 2 |
    And I save current checkpoint of sequencer "SEQ_A" as "CHECKPOINT_1"
    And I restart zone sequencer "SEQ_A" from checkpoint "CHECKPOINT_1"
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data      |
      | MSG_3 | Message 3 |
      | MSG_4 | Message 4 |
    Then all zone messages are safe in 120 seconds
    And all zone messages are finalized in 180 seconds
    And the zone indexer returns messages in this order:
      | alias |
      | MSG_1 |
      | MSG_2 |
      | MSG_3 |
      | MSG_4 |
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_sequencer_stale_checkpoint_resume
  Scenario: Resume zone sequencer from stale checkpoint
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A      |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data  |
      | MSG_1 | msg-1 |
      | MSG_2 | msg-2 |
    And I save current checkpoint of sequencer "SEQ_A" as "STALE_CHECKPOINT"
    Then all zone messages are finalized in 180 seconds
    When I restart zone sequencer "SEQ_A" fresh
    And the zone LIB advances in 120 seconds
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data  |
      | MSG_3 | msg-3 |
      | MSG_4 | msg-4 |
    Then all zone messages are finalized in 180 seconds
    When I restart zone sequencer "SEQ_A" from checkpoint "STALE_CHECKPOINT"
    And the zone LIB advances in 120 seconds
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data  |
      | MSG_5 | msg-5 |
    Then all zone messages are finalized in 180 seconds
    And the zone indexer returns each of these messages exactly once in this order:
      | alias |
      | MSG_1 |
      | MSG_2 |
      | MSG_3 |
      | MSG_4 |
      | MSG_5 |
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_sequential_multi_sequencer
  Scenario: Sequential multi-sequencer publishing keeps channel order
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    And the following zone sequencers share the signing key of "SEQ_A":
      | alias |
      | SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data |
      | MSG_1 | a1   |
      | MSG_2 | a2   |
      | MSG_3 | a3   |
    Then the zone indexer returns messages in any order in 360 seconds:
      | alias |
      | MSG_1 |
      | MSG_2 |
      | MSG_3 |
    When I stop zone sequencer "SEQ_A"
    And I start zone sequencer "SEQ_B"
    And sequencer "SEQ_B" publishes the following zone messages:
      | alias | data |
      | MSG_4 | b1   |
      | MSG_5 | b2   |
      | MSG_6 | b3   |
    Then the zone indexer returns messages in any order in 360 seconds:
      | alias |
      | MSG_1 |
      | MSG_2 |
      | MSG_3 |
      | MSG_4 |
      | MSG_5 |
      | MSG_6 |
    When I stop zone sequencer "SEQ_B"
    And I start zone sequencer "SEQ_A"
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data |
      | MSG_7 | a4   |
      | MSG_8 | a5   |
      | MSG_9 | a6   |
    Then the zone indexer returns messages in this order:
      | alias |
      | MSG_1 |
      | MSG_2 |
      | MSG_3 |
      | MSG_4 |
      | MSG_5 |
      | MSG_6 |
      | MSG_7 |
      | MSG_8 |
      | MSG_9 |
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_concurrent_multi_sequencer
  Scenario: Concurrent multi-sequencer publishing converges without duplicates
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers          |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B, SEQ_C |
    And the following zone sequencers share the signing key of "SEQ_A":
      | alias |
      | SEQ_B |
      | SEQ_C |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    When I stop zone sequencer "SEQ_A"
    And each listed zone sequencer publishes 20 generated zone messages concurrently with republish policy:
      | sequencer | data_prefix |
      | SEQ_A     | a           |
      | SEQ_B     | b           |
      | SEQ_C     | c           |
    Then the zone indexer returns all zone messages exactly once in any order in 1200 seconds
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_sorted_conflict_resolution
  Scenario: Sorted conflict policy preserves per-sequencer order and converges without duplicates
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 10                | 0               | SEQ_A, SEQ_B          |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I stop zone sequencer "SEQ_A"
    And the following zone messages are published concurrently with sorted conflict policy:
      | sequencer | alias  | data |
      | SEQ_A     | MSG_1  | aa   |
      | SEQ_A     | MSG_2  | cc   |
      | SEQ_A     | MSG_3  | ee   |
      | SEQ_A     | MSG_4  | gg   |
      | SEQ_A     | MSG_5  | ii   |
      | SEQ_B     | MSG_6  | bb   |
      | SEQ_B     | MSG_7  | dd   |
      | SEQ_B     | MSG_8  | ff   |
      | SEQ_B     | MSG_9  | hh   |
      | SEQ_B     | MSG_10 | jj   |
    Then the zone indexer preserves per-sequencer order and converges without duplicates in 600 seconds
    And I stop all nodes

  @zone_ci
  Scenario: Round-robin waits for turn and submits pending messages
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_A | true    | 2                    | false                     |
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 2                 | 0               | SEQ_A, SEQ_B          |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_B | false   | 2                    | false                     |
    Then sequencer "SEQ_B" reaches sequencing state:
      | own_key_index | turn_to_write | pending_transactions | time_out |
      | 1             | NOT_OUR_TURN  | 0                    | 120      |
    # Prepare three signed pending messages while SEQ_B is not on turn — tests bounded submit depth
    When sequencer "SEQ_B" submits the following zone messages to queue immediately:
      | alias  | data         |
      | MSG_B1 | rr-queued-b1 |
      | MSG_B2 | rr-queued-b2 |
      | MSG_B3 | rr-queued-b3 |
    Then sequencer "SEQ_B" reaches sequencing state:
      | own_key_index | turn_to_write | pending_transactions | time_out |
      | 1             | NOT_OUR_TURN  | 3                    | 120      |
    # Save checkpoint with signed pending txs, restart, verify pending outbox restored
    When I save current checkpoint of sequencer "SEQ_B" as "CHECKPOINT_B_PENDING"
    And I stop zone sequencer "SEQ_B"
    And I restart zone sequencer "SEQ_B" from checkpoint "CHECKPOINT_B_PENDING"
    Then sequencer "SEQ_B" reaches sequencing state:
      | own_key_index | turn_to_write | pending_transactions | time_out |
      | 1             | NOT_OUR_TURN  | 3                    | 120      |
    # The first turn submits only the configured active depth, so two txs are posted but remain pending until finalized
    And sequencer "SEQ_B" emits published events for queued zone messages on its turn in 180 seconds:
      | alias  |
      | MSG_B1 |
      | MSG_B2 |
    Then sequencer "SEQ_B" has 3 pending publish txs in 180 seconds
    Then sequencer "SEQ_B" has 1 pending publish txs in 180 seconds
    And the zone indexer returns messages in any order in 360 seconds:
      | alias  |
      | MSG_B1 |
      | MSG_B2 |
    And I stop all nodes

  @zone_ci
  Scenario: Round-robin submits all pending messages with no active depth limit
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_A | true    | unlimited            | false                     |
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 2                 | 0               | SEQ_A, SEQ_B          |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_B | false   | unlimited            | false                     |
    Then sequencer "SEQ_B" reaches sequencing state:
      | own_key_index | turn_to_write | pending_transactions | time_out |
      | 1             | NOT_OUR_TURN  | 0                    | 120      |
    When sequencer "SEQ_B" submits the following zone messages to queue immediately:
      | alias  | data           |
      | MSG_C1 | rr-unbounded-1 |
      | MSG_C2 | rr-unbounded-2 |
      | MSG_C3 | rr-unbounded-3 |
    Then sequencer "SEQ_B" reaches sequencing state:
      | own_key_index | turn_to_write | pending_transactions | time_out |
      | 1             | NOT_OUR_TURN  | 3                    | 120      |
    When I save current checkpoint of sequencer "SEQ_B" as "CHECKPOINT_B_NO_LIMIT"
    And I stop zone sequencer "SEQ_B"
    And I restart zone sequencer "SEQ_B" from checkpoint "CHECKPOINT_B_NO_LIMIT"
    Then sequencer "SEQ_B" reaches sequencing state:
      | own_key_index | turn_to_write | pending_transactions | time_out |
      | 1             | NOT_OUR_TURN  | 3                    | 120      |
    And sequencer "SEQ_B" emits published events for queued zone messages on its turn in 180 seconds:
      | alias  |
      | MSG_C1 |
      | MSG_C2 |
      | MSG_C3 |
    Then sequencer "SEQ_B" has 3 pending publish txs in 180 seconds
    And the zone indexer returns messages in any order in 360 seconds:
      | alias  |
      | MSG_C1 |
      | MSG_C2 |
      | MSG_C3 |
    Then sequencer "SEQ_B" has 0 pending publish txs in 180 seconds
    And I stop all nodes

  @zone_ci
  Scenario: Round-robin publishes immediately when it is our turn
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 2                 | 0               | SEQ_A, SEQ_B          |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I start zone sequencer "SEQ_B"
    Then sequencer "SEQ_A" is notified it is their turn to write in 120 seconds
    And sequencer "SEQ_B" is notified it is their turn to write in 120 seconds
    And sequencer "SEQ_A" is notified it is their turn to write in 120 seconds
    When I submit zone message "MSG_A1" to sequencer "SEQ_A" with data "decentralized-immediate-publish" immediately
    Then sequencer "SEQ_A" publishes "MSG_A1" immediately while in turn in 120 seconds
    And the zone indexer returns messages in any order in 360 seconds:
      | alias  |
      | MSG_A1 |
    And I stop all nodes

  @zone_ci
  Scenario: Round-robin with multiple sequencers dynamically added
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers          |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B, SEQ_C |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_A | true    | default              | true                      |
    # Start with A-only round-robin config (single key), then prove immediate publish.
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 2                 | 0               | SEQ_A                 |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I submit zone message "MSG_A_1" to sequencer "SEQ_A" with data "seq_a-msg1" on its turn
    # Auth B without stopping A.
    When sequencer "SEQ_A" submits zone config transaction:
      | config_name | posting_timeframe | posting_timeout | authorized_sequencers |
      | CONFIG_B    | 2                 | 0               | SEQ_A, SEQ_B          |
    Then zone transaction "CONFIG_B" is finalized in 180 seconds
    When I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_B | false   | default              | true                      |
    When I submit zone message "MSG_B_1" to sequencer "SEQ_B" with data "seq_b-msg1" on its turn
    # Auth C without stopping A or B.
    When sequencer "SEQ_A" submits zone config transaction:
      | config_name | posting_timeframe | posting_timeout | authorized_sequencers |
      | CONFIG_C    | 2                 | 0               | SEQ_A, SEQ_B, SEQ_C   |
    Then zone transaction "CONFIG_C" is finalized in 180 seconds
    When I start zone sequencers:
      | alias | indexer | pending_submit_depth | passive_republish_orphans |
      | SEQ_C | false   | default              | true                      |
    When I submit zone message "MSG_C_1" to sequencer "SEQ_C" with data "seq_c-msg1" on its turn
    # Now publish more messages from all sequencers and check they are all indexed without duplicates
    When I submit zone message "MSG_A_2" to sequencer "SEQ_A" with data "seq_a-msg2" immediately
    When I submit zone message "MSG_B_2" to sequencer "SEQ_B" with data "seq_b-msg2" immediately
    When I submit zone message "MSG_C_2" to sequencer "SEQ_C" with data "seq_c-msg2" immediately
    # Final check: all messages on chain, exact once.
    Then the zone indexer returns messages in any order in 120 seconds:
      | alias |
      | MSG_A_1 |
      | MSG_A_2 |
      | MSG_B_1 |
      | MSG_B_2 |
      | MSG_C_1 |
      | MSG_C_2 |
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_balance_conditioned_republish
  Scenario: Balance-aware republish policy drops unaffordable zone updates
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers          |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B, SEQ_C |
    And the following zone account balances exist:
      | account | balance |
      | alice   | 10      |
      | bob     | 10      |
      | charlie | 10      |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 60                | 0               | SEQ_A, SEQ_B, SEQ_C   |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I stop zone sequencer "SEQ_A"
    And the following zone balance updates are published concurrently with balance-aware policy:
      | sequencer | alias     | account | delta |
      | SEQ_A     | a-alice   | alice   | -6    |
      | SEQ_A     | a-bob     | bob     | -3    |
      | SEQ_A     | a-charlie | charlie | -2    |
      | SEQ_B     | b-alice   | alice   | -5    |
      | SEQ_B     | b-bob     | bob     | -4    |
      | SEQ_B     | b-charlie | charlie | -8    |
      | SEQ_C     | c-alice   | alice   | -4    |
      | SEQ_C     | c-bob     | bob     | -7    |
      | SEQ_C     | c-charlie | charlie | -1    |
    Then zone balance updates keep all accounts non-negative after 60 seconds
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_concurrent_identical_payloads
  Scenario: Concurrent identical payloads converge to one inscription per publish
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers          |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B, SEQ_C |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" submits zone config transaction:
      | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
      | CHANNEL_CONFIG_1 | 60                | 0               | SEQ_A, SEQ_B, SEQ_C   |
    Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
    When I stop zone sequencer "SEQ_A"
    And each listed zone sequencer publishes 10 copies of zone message "shared-message" concurrently with republish policy:
      | sequencer |
      | SEQ_A     |
      | SEQ_B     |
      | SEQ_C     |
    Then the zone indexer returns 30 copies of zone message "shared-message" in 600 seconds
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_subscribe_to_finalized_deposit
  Scenario: Finalized deposits are returned by the zone indexer
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A      |
    When node "NODE_1" is at height 2 in 300 seconds
    And I do a coin split for "WALLET_1A" of 3 UTXOs valued at 1 LGO tokens each
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data                |
      | MSG_1 | initial inscription |
    Then all zone messages are safe in 120 seconds
    When I submit zone deposit transaction "DEPOSIT_1" into channel of "SEQ_A" of 1 with metadata "Mint 1 to Alice in Zone"
    Then zone transaction "DEPOSIT_1" is included in 120 seconds
    And zone transaction "DEPOSIT_1" is finalized in 120 seconds
    And the zone indexer returns finalized deposit "DEPOSIT_1" in 120 seconds
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_atomic_deposit_inscription
  Scenario: Atomic deposit and inscription are finalized together
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A      |
    When node "NODE_1" is at height 1 in 120 seconds
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data                |
      | MSG_1 | initial inscription |
    Then all zone messages are finalized in 120 seconds
    When sequencer "SEQ_A" submits atomic zone deposit transaction "ATOMIC_1" with inscription "MSG_2" of 1 with metadata "Mint 1 to Alice in Zone"
    Then zone transaction "ATOMIC_1" is included in 120 seconds
    And zone transaction "ATOMIC_1" is finalized in 120 seconds
    And the zone indexer returns finalized deposit "ATOMIC_1" in 120 seconds
    And the zone indexer returns messages in this order:
      | alias |
      | MSG_1 |
      | MSG_2 |
    And I stop all nodes

  @zone_ci
  # [tests/src/tests/zone_sdk/e2e.rs] test_subscribe_to_finalized_withdraw
  Scenario: Finalized withdraws are returned by the zone indexer and sequencer
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A      |
    When node "NODE_1" is at height 2 in 300 seconds
    And I do a coin split for "WALLET_1A" of 3 UTXOs valued at 3 LGO tokens each
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias | data                |
      | MSG_1 | initial inscription |
    Then all zone messages are finalized in 120 seconds
    When I submit zone deposit transaction "DEPOSIT_1" into channel of "SEQ_A" of 3 with metadata "Mint 3 to Alice in Zone"
    Then zone transaction "DEPOSIT_1" is included in 120 seconds
    And zone transaction "DEPOSIT_1" is finalized in 120 seconds
    And the zone indexer returns finalized deposit "DEPOSIT_1" in 120 seconds
    And sequencer "SEQ_A" finalizes deposit "DEPOSIT_1" in 120 seconds
    When sequencer "SEQ_A" submits zone withdraw transaction "WITHDRAW_1" with inscription "MSG_2" of 2
    Then zone transaction "WITHDRAW_1" is included in 120 seconds
    And zone transaction "WITHDRAW_1" is finalized in 120 seconds
    And the zone indexer returns finalized withdraw "WITHDRAW_1" in 120 seconds
    And sequencer "SEQ_A" finalizes withdraw "WITHDRAW_1" in 120 seconds
    And the zone indexer returns messages in this order:
      | alias |
      | MSG_1 |
      | MSG_2 |
    And I stop all nodes

  @zone_ci
  Scenario: Atomic withdraw bundle finalizes alongside multi-sequencer publishing
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    And the following zone sequencers share the signing key of "SEQ_A":
      | alias |
      | SEQ_B |
    When node "NODE_1" is at height 2 in 300 seconds
    And I do a coin split for "WALLET_1A" of 3 UTXOs valued at 5 LGO tokens each
    And I start zone sequencer "SEQ_A" with indexer
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias    | data                |
      | MSG_INIT | initial inscription |
    Then all zone messages are finalized in 120 seconds
    When I submit zone deposit transaction "DEPOSIT_1" into channel of "SEQ_A" of 5 with metadata "Mint 5 for atomic withdraw"
    Then zone transaction "DEPOSIT_1" is finalized in 120 seconds
    And the zone indexer returns finalized deposit "DEPOSIT_1" in 120 seconds
    When I start zone sequencer "SEQ_B"
    And sequencer "SEQ_A" publishes the following zone messages:
      | alias  | data |
      | MSG_A1 | a1   |
      | MSG_A2 | a2   |
    And sequencer "SEQ_B" publishes atomic withdraw "BUNDLE_1" with inscription "MSG_BURN":
      | withdraw    | outputs |
      | WITHDRAW_1A | 1       |
      | WITHDRAW_1B | 1,2     |
    Then zone transaction "BUNDLE_1" is included in 240 seconds
    And zone transaction "BUNDLE_1" is finalized in 240 seconds
    And the zone indexer returns finalized withdraw "WITHDRAW_1A" in 120 seconds
    And the zone indexer returns finalized withdraw "WITHDRAW_1B" in 120 seconds
    And the zone indexer returns messages in any order in 240 seconds:
      | alias    |
      | MSG_INIT |
      | MSG_A1   |
      | MSG_A2   |
      | MSG_BURN |
    And I stop all nodes
