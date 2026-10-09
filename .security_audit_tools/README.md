# Security audit tools

See the [scenario runbook](RUN_SCENARIOS.md) for build, execution, and evidence
preservation instructions.

## Invariant 1: Same-parent, same-slot sibling proposals

The [V1 attack invariants](ATTACK_INVARIANTS.md) describe the primitive, its
checks and the limits of the resulting evidence. The [scenarios](../tests/cucumber_tests/features/security_audit_v1.feature)
specify the experiment criteria.

The feature-gated Cucumber steps are registered in
`tests/src/cucumber/steps/security_audit/`.

`summarize.py` reads producer and receiver logs and writes a JSON report plus a
CSV timing table. It uses only the Python standard library and does not copy or
rewrite input logs.

### Cucumber invocation

Follow the [scenario runbook](RUN_SCENARIOS.md) for feature selection, node
binary selection and artifact preservation. Each scenario can run independently.

Sustained opportunity reports expose `body_integrity.body_root` and
`body_integrity.ordered_transaction_hashes` with `matched`, `mismatched` or
`unverified` status, comparison counts and an all-recorded-siblings coverage
flag. `matched` applies only to retrieved bodies actually compared; the count
and coverage flag show whether the whole set was examined. An unavailable API
body is not a consensus rejection, and per-node receiver evidence is separate.
`retrieved_sibling_bodies` records the number of successful producer block API
responses.

`shutdown-trigger.json` distinguishes harness observation times from the event
timestamp and retains its log location. V1 verifies post-attack canonical
convergence; it does not establish post-attack finality.

Cucumber provenance keeps `binary_path` and `binary_sha256` for the resolved
executable. `feature_set` is null to avoid treating an expectation as a
verified binary property. `requested_cucumber_features` is null because the
running test cannot inspect the original Cargo flags; `compiled_test_features_known_enabled`
records features known from compilation. `expected_node_build_features` is the
framework auto-build expectation, not verified features of the selected binary.
`binary_selection_mode` describes what can be inferred about an environment
override, and `node_configs` records generated YAML settings. Observed audit
events are recorded separately when available. Neither a path/hash nor a log
event proves the executable's complete compiled feature set.

The `consensus_configuration` report reads each declared node's generated
deployment YAML and records its path, hash and settings. Unavailable or
inconsistent configurations are `unverified`. These files are not independent
queries of runtime state.

The sustained `transaction_workload` report records the declared transfer
amount and sender and receiver wallet names alongside the scheduling policy.
Generated configuration is evidence of configured topology, not an independent
measurement of all network links.

Nonempty opportunity reports include the ordinary and retrievable accepted
sibling voucher commitments. `retrieved_voucher_commitments_valid` describes
only retrieved bodies and is null when none are available. The
`accepted_bodies_all_match_ordinary` field is also null when no accepted body
was retrieved. `observation_completed_unix_ms` records when the qualifying
producer opportunity was selected, before any receiver wait or producer stop.
Selection failures distinguish no observed producer leadership,
leadership without a qualifying submitted-transaction body, incomplete
opportunity evidence, and workload preparation or submission failures.
Timeouts during preparation or submission are unverified because their
effects may be incomplete. Failure evidence can include producer log lines
with their source paths and offsets.

### Analyze logs

Pass each log as `NODE=PATH`. Supply one or more half-open UTC/offset ISO-8601
windows with `--window START END`; without windows, all parsed events are used.
Windows should not overlap when comparing sample counts. Set
`--expected-siblings` to validate the configured count. When logs include
honest nodes with a different setting, use `--producer-node` to scope that
check to the producer's `NODE=PATH` label. The output directory is created if
needed.

```bash
python3 .security_audit_tools/summarize.py \
  --log NODE_01=producer.log \
  --log NODE_02=honest-peer.log \
  --window 2026-10-11T10:00:00Z 2026-10-11T10:02:00Z \
  --expected-siblings 4 \
  --producer-node NODE_01 \
  --commit-sha 0123456789abcdef \
  --binary-sha256 SHA256 \
  --config-sha256 SHA256 \
  --feature security-audit,testing \
  --scenario 'Honest peers validate same-slot sibling blocks' \
  --node-identity NODE_01=PRODUCER_PEER_ID \
  --node-identity NODE_02=RECEIVER_PEER_ID \
  --output-dir security-audit-summary
```

The report contains each leadership opportunity, its timing fields, sibling
identity events and receiver results grouped by node. `timings.csv` contains
minimum, median and maximum values with a sample count for each timing metric.
Evidence-integrity `anomalies` produce exit status 1 unless
`--allow-anomalies` is passed. Nonfatal `incomplete_observations` describe
missing or ambiguous receiver evidence and do not change the default exit
status. Confirmed local and receiver outcomes remain in each sibling's
`outcome` and each node's `receiver_results`; failure reasons are retained
where the logs provide them.

The summary distinguishes complete Blend-to-acceptance paths, partial receiver
observations, missing receiver observations, explicit reconstruction failures,
explicit application failures, and local relay enqueue failures. Incomplete
evidence is not a rejection. A local relay enqueue failure is a confirmed
producer-side result, but it is not a receiver or consensus result.

### Evidence semantics

- `relay_enqueued` records only that the producer's local Blend relay accepted
  the enqueue request. It is never treated as proof of peer reception.
- Receiver events are correlated within the selected measurement window by
  sibling ID, receiving node, producer parent ID and slot. Each receiver has
  independent Blend reception, Chain Network receipt, reconstruction,
  application and storage fields. Parent/slot mismatches are reported and
  excluded from the matching receiver path.
- `accepted_after_blend_reception` requires one honest node to show a
  timestamp-ordered sequence of Blend decapsulation, Chain Network proposal
  receipt, reconstruction and successful consensus application. Equal
  timestamps establish order only when the events are in the same file and
  their line positions are increasing. Missing timestamps, ambiguous ties,
  reverse order, and stages split across peers cannot produce a successful
  path. These are listed under `incomplete_observations`, not evidence-
  integrity errors. Missing evidence is distinct from a confirmed rejection.
- The aggregate sibling outcome is derived from complete per-node results.
  Aggregate stage-presence fields must not be
  used alone to claim an accepted Blend delivery. `accepted_after_blend_reception`
  is also exposed in the summary.
- A stored block does not prove that it arrived through Blend. Establishing
  that path requires a matching receiver Blend-decapsulation record as well as
  later processing evidence. `accepted` does not imply canonical selection;
  canonical status is shown only if an explicit storage event records it.
- The analyzer correlates sibling identity and leadership records by producer
  node, parent ID and slot, then verifies the ordinary ID shared by the sibling
  identity records. Receiver events are correlated by window, sibling ID,
  receiving node, parent ID and slot. An accepted stage on one peer cannot be
  combined with Blend reception on another peer.
- A receiver's explicit reconstruction failure is reported as
  `reconstruction_failed`, with its logged reason when present. An
  `UnresolvedReference` reason records that reconstruction could not resolve a
  transaction reference from that peer's mempool; it does not by itself prove
  the sibling was consensus-invalid. Proposal rejection and consensus apply
  failure are separately reported with their reasons. Missing events remain
  unobserved rather than being synthesized into failures.

These logs cannot establish that an unobserved peer never received a proposal;
they establish only which stages were observed in the supplied logs and time
windows. They also do not prove network transmission from a local enqueue
boolean.

### Provenance

Pass provenance explicitly when known: commit SHA, release binary SHA-256,
configuration SHA-256, enabled feature set, scenario name and each node's
identity. Missing provenance stays `null` in `measurements.json`; the tool does
not guess it from filenames or Git state.

### Tests

```bash
python3 -m unittest discover -s .security_audit_tools/tests -v
```

The sanitized fixture covers zero, one and three sibling opportunities and a
complete ordered one-peer receiver path. Synthetic tests cover reverse event
order, split stages across peers, missing reconstruction, parent/slot
mismatches, repeated stages, measurement-window isolation, duplicate/missing
identities, failed local enqueue, incomplete records, summary statistics and
machine-readable output.
