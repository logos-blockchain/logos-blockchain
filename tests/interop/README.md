# Blockchain test interoperability

This crate connects other node implementations to the existing Logos tests.
It contains implementation-specific preparation, runner entry points and the
mixed-cluster test. The normal Logos runner does not depend on this crate.

The Cucumber scenarios and steps remain in `logos-blockchain-tests`. An
integration selects `LocalImplementation::External` and supplies an
`ExternalDeploymentFactory`. The factory receives a `DeploymentInput` for
each scenario. Other implementations obtain `SharedDeployment` through
`shared_inputs()` and prepare their native configuration. Logos callers use
`deploy_logos()` to reuse the original plan, including its keys and overrides.
The factory deploys the resulting TF app. Shared steps use TF node control and
`NodeRuntimeInfo`; operations that require typed Logos configuration remain
Logos-specific and fail explicitly on other applications.

`SharedDeployment` currently carries Logos deployment YAML and network
identities. This is an initial data contract, not yet an independent protocol
schema. Adapters must preserve the selected network settings and reject inputs
they cannot represent. Consumers should use the public runner and integration
interfaces rather than reach into repository-private helpers.

## Ownership and direction

The adapter stays in this separate crate initially so the runner and a real
second implementation can develop together. This helps us find assumptions in
the interface and makes initial adoption easier. It does not make Logos the
permanent owner of Nimbos compatibility. Implementation-specific changes should
be maintained with the team responsible for that implementation.

The principles for this integration are:

- Each implementation owns how its nodes are configured, launched and observed.
- The Logos Cucumber suite is the single source of truth for shared scenarios,
  steps and their semantics.
- External consumers select the Logos test-suite revision they support. Current
  Logos should not accumulate compatibility logic for historical consumers.
- Configuration, CLI, key and schema changes remain the implementation team's
  responsibility. Mixed tests do not transfer that ownership to another team.
- Shared operations have consistent semantics. Unsupported operations and
  scenarios must be explicit.
- Integrations consume deliberate public contracts. The shared-data contract
  needs to become clearer as real integrations exercise it.
- Logos CI validates the reusable runner, including a real external caller using
  Logos binaries. Other implementations validate supported shared scenarios
  using their own binaries. Mixed tests can remain manual.
- Compatibility ultimately requires passing supported shared scenarios and
  demonstrating agreement on the canonical chain in a mixed network.

Longer term, a common adapter could accept a launch description and invoke an
implementation-owned configuration-preparation command. Teams could then supply
their preparation in their own language without maintaining a Rust adapter.
That interface is a possible next step, not something implemented here. Moving
the current adapter to its own repository also remains an option.

## External-runner CI smoke test

The `cucumber_external_smoke` executable uses real Logos binaries through
`LocalImplementation::External`. Its factory uses the same prepared Logos app
as the normal runner, without regenerating configuration or replacing keys and
genesis.
The selected scenario uses the common startup and control path without native
configuration patches. The separate in-memory regression test covers an
external application with no typed Logos handle.

The existing `Two nodes connect at runtime` scenario starts two nodes, connects
them through the API, checks both peer counts and stops them. This validates
the external entry point, runtime metadata and shared control. The selected
scenario does not test block production.

```sh
export LOGOS_BLOCKCHAIN_NODE_BIN=/path/to/logos-blockchain-node
cargo run -p blockchain-test-interop --bin cucumber_external_smoke -- \
  --name '^Two nodes connect at runtime$'
```

Use a Logos binary matching the checkout. In the merge queue, the Cucumber CI
workflow invokes the already-built runner through the shared Cucumber action,
using its existing Logos binary and NTP setup. No Nimbos binary is needed.
Failure artifacts use the same upload handling as the other suites.

## Running another implementation

The initial integration uses Nimbos. Set its binary and circuits directory, then
select an existing Cucumber scenario:

```sh
export NIMBOS_NODE_BIN=/path/to/logos_chain_node
export NIMBOS_CIRCUITS_DIR=/path/to/circuits
cargo run -p blockchain-test-interop --bin cucumber_nimbos -- \
  --name '^Two nodes happy path$'
```

For the mixed test, also select a compatible Logos binary:

```sh
export LOGOS_BLOCKCHAIN_NODE_BIN=/path/to/logos-blockchain-node
cargo test -p blockchain-test-interop --test logos_nimbos_mixed -- --ignored
```

`LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL` can select a release archive instead of a
local Logos binary. Configuration still comes from the current checkout and
must be compatible with the selected binary.

The mixed test deploys two nodes of each implementation, checks the expected
peer connections, stops one Nimbos node and checks the surviving connection.
It does not yet assert canonical-chain agreement. With the revisions tested so
far, genesis/protocol differences still prevent a successful mixed run. The
runner reports those failures rather than changing the scenario's meaning.
