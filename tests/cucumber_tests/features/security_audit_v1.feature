@security_audit @security_audit_v1
Feature: Security audit

  @security_audit @siblings @consensus
  Scenario: Honest peers validate same-slot sibling blocks
    Given I have a cluster with capacity of 4 nodes
    And the cluster uses diagnostic parameter set "security_audit_set_01"
    And the first 4 nodes are declared as blend providers
    And the security audit participants are:
      | node    | role     | canonical_reference |
      | NODE_01 | producer | no                  |
      | NODE_02 | honest   | yes                 |
      | NODE_03 | honest   | no                  |
      | NODE_04 | honest   | no                  |

    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "0"
    When I start node "NODE_02"
    And I start peer node "NODE_03" connected to node "NODE_02"
    And I start peer node "NODE_04" connected to node "NODE_02"
    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "4"
    And I start peer node "NODE_01" connected to node "NODE_02"

    Then all declared audit nodes establish Blend readiness within 300 seconds
    When the audit observes a genuine sibling opportunity with at least 4 valid siblings within 1500 seconds
    Then at least 1 sibling proposal is accepted by a declared honest node through an ordered Blend-to-consensus path within 150 seconds
    And I stop all nodes

  @security_audit @siblings @transactions
  Scenario: Honest peers validate nonempty same-slot sibling blocks
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 200000000    |
      | 2             | 0           | 0            |
      | 3             | 0           | 0            |
      | 4             | 0           | 0            |
    And I have a cluster with capacity of 4 nodes
    And the cluster uses diagnostic parameter set "security_audit_set_01"
    And the first 4 nodes are declared as blend providers
    And the security audit participants are:
      | node    | role     | canonical_reference |
      | NODE_01 | producer | no                  |
      | NODE_02 | honest   | yes                 |
      | NODE_03 | honest   | no                  |
      | NODE_04 | honest   | no                  |
    And the audit transaction workload is:
      | sender_wallet          | SECURITY_AUDIT_SENDER   |
      | receiver_wallet        | SECURITY_AUDIT_RECEIVER |
      | amount_lgo             | 10                      |
      | maximum_transactions   | 80                      |
      | pending_sample_seconds | 30                      |

    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "0"
    When I start nodes with wallet resources:
      | node_name | account_index | wallet_name             | connected_to |
      | NODE_02   | 1             | SECURITY_AUDIT_SENDER   |              |
      | NODE_02   | 2             | SECURITY_AUDIT_RECEIVER |              |
      | NODE_03   | 3             | SECURITY_AUDIT_PEER_3   | NODE_02      |
      | NODE_04   | 4             | SECURITY_AUDIT_PEER_4   | NODE_02      |
    When I do a coin split for "SECURITY_AUDIT_SENDER" of 250 UTXOs valued at 400000 LGO tokens each
    And wallet "SECURITY_AUDIT_SENDER" has 250 or more outputs in 900 seconds
    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "4"
    And I start peer node "NODE_01" connected to node "NODE_02"

    Then all declared audit nodes establish Blend readiness within 300 seconds
    When the audit observes a genuine sibling opportunity with at least 4 valid siblings within 900 seconds
    Then the selected ordinary block contains a submitted transaction and its siblings preserve its identity
    And at least 1 sibling proposal is accepted by a declared honest node through an ordered Blend-to-consensus path within 150 seconds
    And accepted siblings have the ordinary block's body root and ordered transactions
    And I stop all nodes

  @security_audit @siblings @convergence
  Scenario: Honest nodes converge after a brief nonempty sibling attack
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 200000000    |
      | 2             | 0           | 0            |
      | 3             | 0           | 0            |
      | 4             | 0           | 0            |
    And I have a cluster with capacity of 4 nodes
    And the cluster uses diagnostic parameter set "security_audit_set_01"
    And the first 4 nodes are declared as blend providers
    And the security audit participants are:
      | node    | role     | canonical_reference |
      | NODE_01 | producer | no                  |
      | NODE_02 | honest   | yes                 |
      | NODE_03 | honest   | no                  |
      | NODE_04 | honest   | no                  |
    And the audit transaction workload is:
      | sender_wallet          | SECURITY_AUDIT_SENDER   |
      | receiver_wallet        | SECURITY_AUDIT_RECEIVER |
      | amount_lgo             | 10                      |
      | maximum_transactions   | 80                      |
      | pending_sample_seconds | 30                      |

    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "0"
    When I start nodes with wallet resources:
      | node_name | account_index | wallet_name             | connected_to |
      | NODE_02   | 1             | SECURITY_AUDIT_SENDER   |              |
      | NODE_02   | 2             | SECURITY_AUDIT_RECEIVER |              |
      | NODE_03   | 3             | SECURITY_AUDIT_PEER_3   | NODE_02      |
      | NODE_04   | 4             | SECURITY_AUDIT_PEER_4   | NODE_02      |
    When I do a coin split for "SECURITY_AUDIT_SENDER" of 250 UTXOs valued at 400000 LGO tokens each
    And wallet "SECURITY_AUDIT_SENDER" has 250 or more outputs in 900 seconds
    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "4"
    And I start peer node "NODE_01" connected to node "NODE_02"

    Then all declared audit nodes establish Blend readiness within 300 seconds
    When the audit observes a genuine sibling opportunity with at least 4 valid siblings within 900 seconds
    Then the selected ordinary block contains a submitted transaction and its siblings preserve its identity
    When the audit producer is stopped upon the first selected sibling's Blend reception by a declared honest node within 30 seconds
    Then at least 1 sibling proposal is accepted by a declared honest node through an ordered Blend-to-consensus path within 150 seconds
    And accepted siblings have the ordinary block's body root and ordered transactions
    And all active nodes converged to within 0 blocks in 3000 seconds
    And I stop all nodes

  @security_audit @siblings @sustained @convergence
  Scenario: Honest nodes converge after a sustained nonempty sibling attack
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 200000000    |
      | 2             | 0           | 0            |
      | 3             | 0           | 0            |
      | 4             | 0           | 0            |
    And I have a cluster with capacity of 4 nodes
    And the cluster uses diagnostic parameter set "security_audit_set_01"
    And the first 4 nodes are declared as blend providers
    And the security audit participants are:
      | node    | role     | canonical_reference |
      | NODE_01 | producer | no                  |
      | NODE_02 | honest   | yes                 |
      | NODE_03 | honest   | no                  |
      | NODE_04 | honest   | no                  |
    And the audit transaction workload is:
      | sender_wallet                | SECURITY_AUDIT_SENDER   |
      | receiver_wallet              | SECURITY_AUDIT_RECEIVER |
      | amount_lgo                   | 10                      |
      | maximum_transactions         | 80                      |
      | initial_pending_transactions | 8                       |
      | submission_interval_seconds  | 25                      |

    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "0"
    When I start nodes with wallet resources:
      | node_name | account_index | wallet_name             | connected_to |
      | NODE_02   | 1             | SECURITY_AUDIT_SENDER   |              |
      | NODE_02   | 2             | SECURITY_AUDIT_RECEIVER |              |
      | NODE_03   | 3             | SECURITY_AUDIT_PEER_3   | NODE_02      |
      | NODE_04   | 4             | SECURITY_AUDIT_PEER_4   | NODE_02      |
    When I do a coin split for "SECURITY_AUDIT_SENDER" of 250 UTXOs valued at 400000 LGO tokens each
    And wallet "SECURITY_AUDIT_SENDER" has 250 or more outputs in 900 seconds
    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "4"
    And I start peer node "NODE_01" connected to node "NODE_02"

    When the sustained sibling attack meets these exposure and cooldown criteria:
      | criterion                        | value |
      | complete_opportunities           | 6     |
      | nonempty_submitted_opportunities | 3     |
      | delivered_opportunities          | 2     |
      | cooldown_canonical_blocks        | 10    |
      | minimum_active_seconds           | 300   |
      | active_timeout_seconds           | 3600  |
      | readiness_seconds                | 300   |
      | maximum_tip_height_skew          | 1     |

    When I stop node "NODE_01"
    Then all active nodes converged to within 0 blocks in 3000 seconds
    And I stop all nodes

  @security_audit @sustained_control
  Scenario: Honest nodes converge without sibling attacks
    Given the genesis block has the following wallet resources:
      | account_index | token_count | token_amount |
      | 1             | 1           | 200000000    |
      | 2             | 0           | 0            |
      | 3             | 0           | 0            |
      | 4             | 0           | 0            |
    And I have a cluster with capacity of 4 nodes
    And the cluster uses diagnostic parameter set "security_audit_set_01"
    And the first 4 nodes are declared as blend providers
    And the security audit participants are:
      | node    | role     | canonical_reference |
      | NODE_01 | producer | no                  |
      | NODE_02 | honest   | yes                 |
      | NODE_03 | honest   | no                  |
      | NODE_04 | honest   | no                  |

    And I have user config override "cryptarchia.leader.security_audit.sibling_blocks_per_leadership" as "0"
    When I start nodes with wallet resources:
      | node_name | account_index | wallet_name             | connected_to |
      | NODE_02   | 1             | SECURITY_AUDIT_SENDER   |              |
      | NODE_02   | 2             | SECURITY_AUDIT_RECEIVER |              |
      | NODE_03   | 3             | SECURITY_AUDIT_PEER_3   | NODE_02      |
      | NODE_04   | 4             | SECURITY_AUDIT_PEER_4   | NODE_02      |
    When I do a coin split for "SECURITY_AUDIT_SENDER" of 250 UTXOs valued at 400000 LGO tokens each
    And wallet "SECURITY_AUDIT_SENDER" has 250 or more outputs in 900 seconds
    And I start peer node "NODE_01" connected to node "NODE_02"

    Then all nodes have at least 40 blocks and converged to within 0 blocks in 3600 seconds
    And I stop all nodes
