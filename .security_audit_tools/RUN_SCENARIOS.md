# Run security-audit scenarios

Run these commands from the repository root. `security_audit_v1.feature` covers
invariant 1. Future individual invariants use `security_audit_v2.feature`,
`security_audit_v3.feature`, and so on; combined-invariant experiments use
`security_audit_mixed_1.feature` and subsequent numbered names. The
`@security_audit` tag selects the audit suite, and `@security_audit_v1` selects
invariant 1.

## Invariant index

- [Invariant 1: Same-parent, same-slot sibling proposals](#invariant-1-same-parent-same-slot-sibling-proposals)

## Invariant 1: Same-parent, same-slot sibling proposals

The [V1 scenarios](../tests/cucumber_tests/features/security_audit_v1.feature)
exercise the [attack invariants](ATTACK_INVARIANTS.md).

### Select the node executable

The test framework resolves a node executable for each scenario. Locally, it
builds a release node with `testing,security-audit` when no executable is
provided. To use an existing executable, set `LOGOS_BLOCKCHAIN_NODE_BIN` to its
path; that binary must already have compatible compiled features because test
crate features cannot change it. The per-scenario evidence records the selected
path and SHA-256. No separate build or provenance comparison between scenarios
is required.

For a serial run with retries disabled, set:

```bash
export CUCUMBER_RETRIES=0 MAX_CUCUMBER_CONCURRENT_SCENARIOS=1
```

### Run the scenarios

Use `--features cucumber,security-audit` for the test crate. The Cargo options
come before `--`; Cucumber selection options come after it. Run any scenario
individually with the command below. `--name` is a Cucumber name filter; verify
that each invocation actually executed the intended single scenario.

```bash
cargo test --locked -p logos-blockchain-tests --test cucumber \
  --features cucumber,security-audit -- \
  --name 'Honest peers validate same-slot sibling blocks'
```

1. `Honest peers validate same-slot sibling blocks`
2. `Honest peers validate nonempty same-slot sibling blocks`
3. `Honest nodes converge after a brief nonempty sibling attack`
4. `Honest nodes converge after a sustained nonempty sibling attack`
5. `Honest nodes converge without sibling attacks`

Replace the name in the command for each invocation.

For other invariant or mixed features, Cucumber's `--tags` filter can select
their tags; follow any dependencies explicitly declared by those features.

### Preserve and interpret evidence

The runner writes scenario artifacts under
`tests/cucumber_tests/temp/cucumber_artefacts/<feature>/<scenario>/attempt_1/`.
They include node logs, generated configuration and audit JSON reports. Keep
the reports and complete node runtime directories for passes and failures. Set
no `CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL` value when evidence is needed.

The runner appends JUnit output to
`tests/cucumber_tests/temp/cucumber-output-junit.xml`. After **each** invocation,
move that file to a uniquely named file under the security-audit artifacts
directory before starting the next one. Otherwise multiple XML documents may
be appended to the same file. Preserve any existing JUnit file before the
first run. Before retrying a scenario, move its existing scenario artifact
directory to a unique archive path so the next attempt cannot mix old logs
with new evidence. Retain the original report paths and hashes when archiving;
paths embedded in a moved report refer to their original locations.

Record the exit status, individual JUnit result, source revision, executable
hash, effective configuration and report paths. A pass requires the evidence
and acceptance checks in the relevant scenario, not merely a process that
started. Insufficient stochastic exposure, missing ancestry or body evidence,
failure to reach the canonical target, and observed convergence failures have
distinct classifications. Preserve and report failures
and unverified evidence; do not infer rejection, canonical selection or
security guarantees from missing observations.
