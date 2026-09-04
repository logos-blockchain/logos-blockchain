Feature: Blend diagnostics

  @blend_debug @blend_tsi_diagnostic
  Scenario Outline: Observe TSI after majority Blend provider outage and partial recovery parameter_set=<parameter_set>
    Given I have a cluster with capacity of 10 nodes
    And the first 10 nodes are declared as blend providers
    And the cluster uses Blend diagnostic parameter set "<parameter_set>"
    And the cluster uses SDP funding of 10000000 per provider split across 5 notes

    And I start node "NODE_2"
    And I start peer node "NODE_3" connected to node "NODE_2"

    And I start peer node "NODE_1" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_4" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_5" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_6" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_7" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_8" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_9" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_10" connected to node "NODE_2" and node "NODE_3"
    And I log diagnostic identities

    When I observe 3 epoch transitions on node "NODE_2"

    When I stop node "NODE_1"
    And I stop node "NODE_4"
    And I stop node "NODE_5"
    And I stop node "NODE_6"
    And I stop node "NODE_7"
    And I stop node "NODE_8"
    And I stop node "NODE_9"
    And I stop node "NODE_10"
    And I log diagnostic outage summary

    And I observe 4 epoch transitions on node "NODE_2"

    When I restart node "NODE_1"

    And I observe 8 epoch transitions on node "NODE_2"

    Then I stop all nodes

    Examples:
      | parameter_set          |
      | clean_control          |
      | testnet_representative |
      | fast_repro             |

  @blend_debug @blend_tsi_diagnostic
  Scenario Outline: Find stable accelerated Blend epoch configuration for k=<k>, phases=<p1>_<p2>_<p3>
    Given I have a cluster with capacity of 10 nodes
    And the first 10 nodes are declared as blend providers
    And the cluster uses SDP funding of 10000000 per provider split across 5 notes

    And the cluster uses cryptarchia security parameter <k>
    And I have deployment config override "time.slot_duration" as "seconds(1)"
    And I have deployment config override "cryptarchia.epoch_config.epoch_stake_distribution_stabilization" as "<p1>"
    And I have deployment config override "cryptarchia.epoch_config.epoch_period_nonce_buffer" as "<p2>"
    And I have deployment config override "cryptarchia.epoch_config.epoch_period_nonce_stabilization" as "<p3>"

    And I start node "NODE_2"
    And I start peer node "NODE_3" connected to node "NODE_2"

    And I start peer node "NODE_1" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_4" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_5" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_6" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_7" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_8" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_9" connected to node "NODE_2" and node "NODE_3"
    And I start peer node "NODE_10" connected to node "NODE_2" and node "NODE_3"
    And I log diagnostic identities

    When I observe 10 epoch transitions on node "NODE_2"

    Then I stop all nodes

    Examples:
      | k  | p1 | p2 | p3 |
      | 3  | 1  | 1  | 1  |
      | 5  | 1  | 1  | 1  |
      | 5  | 2  | 2  | 2  |
      | 10 | 1  | 1  | 1  |
      | 5  | 3  | 3  | 4  |

  @blend_debug @blend_tsi_diagnostic @blend_fork_diagnostic
  Scenario Outline: Observe EDGE fork churn after Blend provider reachability outage and partial recovery parameter_set=<parameter_set>
    Given I have a cluster with capacity of 12 nodes
    And the first 8 nodes are declared as blend providers
    And the cluster uses Blend diagnostic parameter set "<parameter_set>"
    And the cluster uses SDP funding of 10000000 per provider split across 5 notes
    And Blend provider endpoints use controllable test relays

    # Allocate all providers before the non-provider EDGE population.
    And I start node "NODE_2"
    And I start peer node "NODE_1" connected to node "NODE_2"
    And I start peer node "NODE_3" connected to node "NODE_2"
    And I start peer node "NODE_4" connected to node "NODE_2"
    And I start peer node "NODE_5" connected to node "NODE_2"
    And I start peer node "NODE_6" connected to node "NODE_2"
    And I start peer node "NODE_7" connected to node "NODE_2"
    And I start peer node "NODE_8" connected to node "NODE_2"

    # Keep the non-provider EDGE nodes directly connected outside Blend.
    And I start peer node "NODE_9" connected to node "NODE_2"
    And I start peer node "NODE_10" connected to node "NODE_2" and node "NODE_9"
    And I start peer node "NODE_11" connected to node "NODE_9" and node "NODE_10"
    And I start peer node "NODE_12" connected to node "NODE_9" and node "NODE_10"

    And I log diagnostic identities

    When I observe 3 epoch transitions on node "NODE_9"

    When I make Blend unreachable on node "NODE_1"
    And I make Blend unreachable on node "NODE_3"
    And I make Blend unreachable on node "NODE_4"
    And I make Blend unreachable on node "NODE_5"
    And I make Blend unreachable on node "NODE_6"
    And I make Blend unreachable on node "NODE_7"
    And I make Blend unreachable on node "NODE_8"
    And I log diagnostic outage summary

    And I observe 4 epoch transitions on node "NODE_9"

    When I restore Blend reachability on node "NODE_1"
    And I observe 6 epoch transitions on node "NODE_9"

    Then I stop all nodes

    Examples:
      | parameter_set          |
      | fast_repro             |
      | testnet_representative |
