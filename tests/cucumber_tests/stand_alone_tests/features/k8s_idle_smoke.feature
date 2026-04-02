Feature: Testing Framework - K8s Runner (Idle Smoke)

  @k8s
  Scenario: Run a k8s idle smoke scenario (no workloads, liveness only)
    Given deployer is "k8s"
    And topology has 2 validators
    And run duration is 30 seconds
    And expect consensus liveness
    When run scenario
    Then scenario should succeed
