Feature: Testing Framework - Local Runner (Idle Smoke)

  @local
  Scenario: Run a local idle smoke scenario (no workloads, liveness only)
    Given deployer is "local"
    And topology has 2 validators
    And run duration is 30 seconds
    And expect consensus liveness
    When run scenario
    Then scenario should succeed
