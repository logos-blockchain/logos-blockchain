Feature: Testing Framework - Auto Local/Compose Deployer

  # no workloads, liveness only
  @normal_ci
  Scenario: Idle smoke
    Given we have a CLI deployer specified
    And topology has 2 validators
    And run duration is 30 seconds
    And expect consensus liveness
    When run scenario
    Then scenario should succeed

  # tx + liveness
  @normal_ci @broken
  Scenario: Simple smoke
    Given we have a CLI deployer specified
    And topology has 2 validators
    And run duration is 60 seconds
    And wallets total funds is 1000000000 split across 50 users
    And transactions rate is 1 per block
    And expect consensus liveness
    When run scenario
    Then scenario should succeed

  # tx + liveness
  # Note: This test may fail on slow computers
  Scenario: Stress smoke
    Given we have a CLI deployer specified
    And topology has 6 validators
    And run duration is 120 seconds
    And wallets total funds is 1000000000 split across 500 users
    And transactions rate is 10 per block
    And expect consensus liveness
    When run scenario
    Then scenario should succeed

  # tx
  Scenario: Stress smoke no liveness
    Given we have a CLI deployer specified
    And topology has 6 validators
    And run duration is 120 seconds
    And wallets total funds is 1000000000 split across 500 users
    And transactions rate is 10 per block
    When run scenario
    Then scenario should succeed
