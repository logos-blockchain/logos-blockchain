Feature: Logos SQL
  Applications submit writes through Logos SQL and read the resulting channel
  history from their local SQLite databases.

  Rule: Replicas converge on channel history

    @logos_sql_ci
    Scenario: Replicate and finalize a SQL write
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
      And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_A | SEQ_A     |
        | SQL_B | SEQ_B     |
      And Logos SQL instance "SQL_A" executes write "CREATE_MESSAGES":
        """
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_B" has 0 rows in table "messages" in its live database in 180 seconds
      When Logos SQL instance "SQL_A" executes write "ADD_MESSAGE":
        """
        INSERT INTO messages (id, body) VALUES (1, 'hello')
        """
      Then Logos SQL instance "SQL_A" has 1 rows in table "messages" in its live database in 30 seconds
      And Logos SQL instance "SQL_B" has 1 rows in table "messages" in its live database in 180 seconds
      And Logos SQL instance "SQL_A" has 1 rows in table "messages" in its finalized database in 180 seconds
      And Logos SQL instance "SQL_B" has 1 rows in table "messages" in its finalized database in 180 seconds
      And I stop all nodes

    @logos_sql_ci
    Scenario: Backfill a new database from channel history
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
      And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_A | SEQ_A     |
      And Logos SQL instance "SQL_A" executes write "CREATE_MESSAGES":
        """
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_A" has 0 rows in table "messages" in its finalized database in 180 seconds
      When Logos SQL instance "SQL_A" executes write "ADD_MESSAGE":
        """
        INSERT INTO messages (id, body) VALUES (1, 'from history')
        """
      Then Logos SQL instance "SQL_A" has 1 rows in table "messages" in its finalized database in 180 seconds
      When I start Logos SQL instances:
        | alias | sequencer |
        | SQL_B | SEQ_B     |
      Then Logos SQL instance "SQL_B" has 1 rows in table "messages" in its finalized database in 30 seconds
      And Logos SQL instances "SQL_A" and "SQL_B" agree on this finalized query in 30 seconds:
        """
        SELECT id, body
        FROM messages
        ORDER BY id
        """
      And I stop all nodes

    @logos_sql_ci
    Scenario: Replicate nondeterministic SQL results exactly
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
      And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_A | SEQ_A     |
        | SQL_B | SEQ_B     |
      And Logos SQL instance "SQL_A" executes write "CREATE_OBSERVATIONS":
        """
        CREATE TABLE observations (
            id INTEGER PRIMARY KEY,
            random_value INTEGER NOT NULL,
            random_bytes BLOB NOT NULL,
            created_at TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_B" has 0 rows in table "observations" in its live database in 180 seconds
      When Logos SQL instance "SQL_A" executes write "ADD_OBSERVATION":
        """
        INSERT INTO observations (id, random_value, random_bytes, created_at)
        VALUES (1, random(), randomblob(16), CURRENT_TIMESTAMP)
        """
      Then Logos SQL instance "SQL_B" has 1 rows in table "observations" in its live database in 180 seconds
      And Logos SQL instances "SQL_A" and "SQL_B" agree on this live query in 30 seconds:
        """
        SELECT id, random_value, random_bytes, created_at
        FROM observations
        ORDER BY id
        """
      And I stop all nodes

    @logos_sql_ci
    Scenario: Concurrent writes converge on accepted channel history
      Given the genesis block has the following wallet resources:
        | account_index | token_count | token_amount |
        | 1             | 3           | 100000       |
      And I have a cluster with capacity of 1 nodes
      And I start nodes with wallet and sequencer resources:
        | node_name | account_index | wallet_name | connected_to | sequencers   |
        | NODE_1    | 1             | WALLET_1A   |              | SEQ_A, SEQ_B |
      When node "NODE_1" is at height 1 in 120 seconds
      And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
      And I start zone sequencer "SEQ_A" with indexer
      And sequencer "SEQ_A" submits zone config transaction:
        | config_name      | posting_timeframe | posting_timeout | authorized_sequencers |
        | CHANNEL_CONFIG_1 | 2                 | 0               | SEQ_A, SEQ_B          |
      Then zone transaction "CHANNEL_CONFIG_1" is finalized in 180 seconds
      When I stop zone sequencer "SEQ_A"
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_A | SEQ_A     |
        | SQL_B | SEQ_B     |
      And Logos SQL instance "SQL_A" executes write "CREATE_CLAIMS":
        """
        CREATE TABLE claims (
            name TEXT PRIMARY KEY,
            owner TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_B" has 0 rows in table "claims" in its live database in 180 seconds
      When the following Logos SQL writes execute concurrently:
        | instance | write   | sql                                                    |
        | SQL_A    | CLAIM_A | INSERT INTO claims (name, owner) VALUES ('claim-a', 'A') |
        | SQL_B    | CLAIM_B | INSERT INTO claims (name, owner) VALUES ('claim-b', 'B') |
      Then Logos SQL instance "SQL_A" has 1 rows in table "claims" in its live database in 180 seconds
      And Logos SQL instance "SQL_B" has 1 rows in table "claims" in its live database in 180 seconds
      And Logos SQL instances "SQL_A" and "SQL_B" agree on this live query in 180 seconds:
        """
        SELECT name, owner FROM claims ORDER BY name
        """
      And exactly one of Logos SQL writes "CLAIM_A" and "CLAIM_B" is displaced in 180 seconds
      And I stop all nodes

    @logos_sql_ci
    Scenario: A disconnected database catches up from its checkpoint
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
      And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_A | SEQ_A     |
        | SQL_B | SEQ_B     |
      And Logos SQL instance "SQL_A" executes write "CREATE_MESSAGES":
        """
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_B" has 0 rows in table "messages" in its finalized database in 180 seconds
      When I stop Logos SQL instance "SQL_B"
      And Logos SQL instance "SQL_A" executes write "ADD_WHILE_DISCONNECTED":
        """
        INSERT INTO messages (id, body) VALUES (1, 'while disconnected')
        """
      Then Logos SQL instance "SQL_A" has 1 rows in table "messages" in its finalized database in 180 seconds
      When I start Logos SQL instances:
        | alias | sequencer |
        | SQL_B | SEQ_B     |
      Then Logos SQL instance "SQL_B" has 1 rows in table "messages" in its finalized database in 30 seconds
      And Logos SQL instances "SQL_A" and "SQL_B" agree on this finalized query in 30 seconds:
        """
        SELECT id, body
        FROM messages
        ORDER BY id
        """
      And I stop all nodes

  Rule: Only valid canonical history changes replicated state

    @logos_sql_ci
    Scenario: Foreign and malformed inscriptions do not block later writes
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
      And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_A | SEQ_A     |
        | SQL_B | SEQ_B     |
      And Logos SQL instance "SQL_A" executes write "CREATE_MESSAGES":
        """
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_B" has 0 rows in table "messages" in its live database in 180 seconds
      When I start zone sequencer "SEQ_A" with indexer
      And sequencer "SEQ_A" publishes the following zone messages:
        | alias                 | data                                  |
        | FOREIGN_TRAFFIC       | not-a-logos-sql-protocol-inscription |
        | MALFORMED_LOGOS_SQL   | LOGOS_SQLjunk                         |
      Then all zone messages are safe in 180 seconds
      When Logos SQL instance "SQL_A" executes write "ADD_AFTER_MALFORMED":
        """
        INSERT INTO messages (id, body) VALUES (1, 'still running')
        """
      Then Logos SQL instance "SQL_B" has 1 rows in table "messages" in its live database in 180 seconds
      And Logos SQL instances "SQL_A" and "SQL_B" agree on this live query in 30 seconds:
        """
        SELECT id, body
        FROM messages
        ORDER BY id
        """
      And I stop all nodes

    @logos_sql_ci
    Scenario: A reorganization removes the write from one abandoned branch
      Given the genesis block has the following wallet resources:
        | account_index | token_count | token_amount |
        | 1             | 3           | 100000       |
        | 2             | 3           | 100000       |
      And I have a cluster with capacity of 7 nodes
      And no nodes are declared as blend providers
      And I start nodes with wallet and sequencer resources:
        | node_name | account_index | wallet_name | connected_to | sequencers           |
        | NODE_A    | 1             | WALLET_A    |              | SEQ_A, SEQ_OBSERVER   |
        | NODE_B    | 2             | WALLET_B    | NODE_A       | SEQ_B                |
      And the following zone sequencers share the signing key of "SEQ_A":
        | alias        |
        | SEQ_OBSERVER |
        | SEQ_B        |
      When node "NODE_A" is at height 1 in 120 seconds
      And wallet "WALLET_A" sends 30 notes of 1000 LGO to node "NODE_A" funding wallet as "FUNDING_TOPUP"
      And transaction "FUNDING_TOPUP" is included on node "NODE_A" in 180 seconds
      And wallet "WALLET_B" sends 30 notes of 1000 LGO to node "NODE_B" funding wallet as "FUNDING_TOPUP_B"
      And transaction "FUNDING_TOPUP_B" is included on node "NODE_A" in 180 seconds
      And I start Logos SQL instances:
        | alias        | sequencer    |
        | SQL_A        | SEQ_A        |
        | SQL_OBSERVER | SEQ_OBSERVER |
      And Logos SQL instance "SQL_A" executes write "CREATE_MESSAGES":
        """
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        )
        """
      Then Logos SQL instance "SQL_OBSERVER" has 0 rows in table "messages" in its finalized database in 180 seconds
      And all nodes have at least 10 blocks and converged to within 1 blocks in 180 seconds
      When I stop node "NODE_B"
      And I start peer node "NODE_A2" connected to node "NODE_A"
      And I start peer node "NODE_A3" connected to node "NODE_A2"
      And Logos SQL instance "SQL_A" executes write "BRANCH_A_WRITE":
        """
        INSERT INTO messages (id, body) VALUES (1, 'branch A')
        """
      Then Logos SQL instance "SQL_OBSERVER" has 1 rows in table "messages" in its live database in 180 seconds
      # Branch A must stop while its write is still provisional so reconnecting
      # the branches can displace it.
      When I record node "NODE_A" height as "ABANDONED_HEIGHT"
      And I stop node "NODE_A3"
      And I stop node "NODE_A2"
      And I stop node "NODE_A"
      Then Logos SQL instance "SQL_A" has 0 rows in table "messages" in its finalized database in 5 seconds
      When I restart node "NODE_B"
      And I start peer node "NODE_B2" connected to node "NODE_B"
      And I start peer node "NODE_B3" connected to node "NODE_B2"
      And I start Logos SQL instances:
        | alias | sequencer |
        | SQL_B | SEQ_B     |
      And Logos SQL instance "SQL_B" executes write "BRANCH_B_WRITE":
        """
        INSERT INTO messages (id, body) VALUES (1, 'branch B')
        """
      And I start Logos SQL instances:
        | alias          | sequencer |
        | SQL_B_OBSERVER | SEQ_B     |
      Then Logos SQL instance "SQL_B_OBSERVER" has 1 rows in table "messages" in its live database in 180 seconds
      And node "NODE_B" reaches 1 blocks beyond recorded height "ABANDONED_HEIGHT" in 180 seconds
      When I restart node "NODE_A"
      And I restart node "NODE_A2"
      And I restart node "NODE_A3"
      And I start peer node "NODE_JOIN" connected to node "NODE_A3" and node "NODE_B3"
      Then all nodes have at least 10 blocks and converged to within 1 blocks in 300 seconds
      When I start Logos SQL instances:
        | alias         | sequencer |
        | SQL_CANONICAL | SEQ_B     |
      Then Logos SQL instance "SQL_CANONICAL" has 1 rows in table "messages" in its live database in 180 seconds
      And Logos SQL instance "SQL_A" has 1 rows in table "messages" in its live database in 180 seconds
      And Logos SQL instance "SQL_B" has 1 rows in table "messages" in its live database in 180 seconds
      And Logos SQL instances "SQL_A" and "SQL_B" agree on this live query in 180 seconds:
        """
        SELECT id, body
        FROM messages
        ORDER BY id
        """
      And exactly one of Logos SQL writes "BRANCH_A_WRITE" and "BRANCH_B_WRITE" is displaced in 180 seconds
      And I stop all nodes
