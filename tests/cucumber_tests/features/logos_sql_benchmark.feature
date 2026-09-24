@logos_sql_benchmark @retry(0)
Feature: Logos SQL settlement benchmark
  Measure a single writer without channel conflicts. Each SQL transaction inserts
  one row. TPS counts new rows in the replica's finalized database during the
  measurement window, excluding initial finality delay. After submission stops,
  the replica must finalize every submitted row and match its original contents.

  Scenario: Measure finalized SQL throughput
    # Keep fee inputs reserved through warm-up, measurement, and draining on
    # this accelerated chain, rather than reusing them for later publications.
    Given I have user config override "wallet.pending_note_expiry_blocks" as "1000"
    And the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 1000000000   |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers   |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
    And the following zone sequencers share the signing key of "SEQ_A":
      | alias |
      | SEQ_B |
    When node "NODE_1" is at height 1 in 120 seconds
    And wallet "WALLET_1A" sends 250 notes of 10000000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
    And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
    # Storage gas can double from its genesis price while funded writes wait.
    # Budget for that increase; this benchmark measures throughput, not fee bidding.
    # Capacity configuration through the public API, not library defaults.
    And I start Logos SQL instances with a 100% fee reserve, batches of 6000 writes and a queue of 32768 writes:
      | alias | sequencer |
      | SQL_A | SEQ_A     |
    And I start read-only Logos SQL instances:
      | alias | sequencer |
      | SQL_B | SEQ_B     |
    And Logos SQL instance "SQL_A" executes write "CREATE_BENCHMARK":
      """
      CREATE TABLE benchmark_writes (
          id INTEGER PRIMARY KEY,
          payload BLOB NOT NULL
      )
      """
    Then Logos SQL instance "SQL_B" has 0 rows in table "benchmark_writes" in its finalized database in 300 seconds
    When I benchmark Logos SQL writer "SQL_A" and replica "SQL_B" on sequencer "SEQ_A" for 180 seconds with 256 byte rows
    Then I stop all nodes
