@password_manager
Feature: Password manager
  The terminal app stores credentials using Logos SQL and reads them locally.
  All passwords in these scenarios are fake.

  Scenario: Add, edit, and remove a credential through the password manager
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 3           | 100000       |
    And I have a cluster with capacity of 1 nodes
    And I start nodes with wallet and sequencer resources:
      | node_name | account_index | wallet_name | connected_to | sequencers |
      | NODE_1    | 1             | WALLET_1A   |              | SEQ_A      |
    When node "NODE_1" is at height 1 in 120 seconds
    And wallet "WALLET_1A" sends 30 notes of 1000 LGO to node "NODE_1" funding wallet as "FUNDING_TOPUP"
    And transaction "FUNDING_TOPUP" is included on node "NODE_1" in 180 seconds
    And the password manager using sequencer "SEQ_A" runs this session in 180 seconds:
      | command                                     | output                            |
      | add email andrus@example.org fake-password   | committed locally as              |
      | show email                                  | password: fake-password            |
      | update email another-fake-password           | committed locally as              |
      | notes email recovery codes stored elsewhere | committed locally as              |
      | show email                                  | password: another-fake-password    |
      | show email                                  | notes: recovery codes stored elsewhere |
      | list                                        | email (andrus@example.org)         |
      | displacements                               | no displacements awaiting review  |
      | remove email                                | committed locally as              |
      | show email                                  | credential not found              |
    Then I stop all nodes
