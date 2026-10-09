# V1 attack invariants

V1 lets a legitimately elected leader produce additional protocol-valid block
candidates with the ordinary proposal's parent and slot, after that proposal
has been applied locally and enqueued to Blend.

## Implemented primitive

- Audit behavior is compiled behind `security-audit`; per-node
  `cryptarchia.leader.security_audit.sibling_blocks_per_leadership` defaults to
  zero, which disables sibling production. See the [node feature wiring](../nodes/node/binary/Cargo.toml),
  [user configuration](../nodes/node/binary/src/config/cryptarchia/serde/leader.rs),
  and [settings](../services/chain/chain-leader/src/security_audit.rs).
- The ordinary PoS selection and proof construction must first yield a winning
  leader. The leader applies the ordinary block locally and attempts its Blend
  enqueue before invoking sibling generation; a failed apply or enqueue does
  not start sibling construction. See the [leader loop and submission](../services/chain/chain-leader/src/lib.rs)
  and [sibling producer](../services/chain/chain-leader/src/security_audit.rs).
- Each sibling copies the ordinary parent, slot, uncle headers and complete
  transaction body. Fresh voucher commitments must differ from the ordinary
  commitment and previously constructed siblings, producing distinct block
  IDs. The completed leadership proof is reused with the new voucher, and
  `Block::create` signs the sibling with the leader's signing key. See
  [sibling construction](../services/chain/chain-leader/src/security_audit.rs),
  [proof voucher replacement](../core/src/proofs/leader_proof.rs), and
  [block creation](../core/src/block/mod.rs).
- Siblings are published through the normal [Blend adapter](../services/chain/chain-leader/src/blend.rs).
  The attack generator does not directly apply them to its local chain. Honest
  nodes use the ordinary proposal reconstruction and consensus application
  path in [Chain Network](../services/chain/chain-network/src/lib.rs).
- A sibling construction or relay failure is recorded without undoing or
  changing the completed ordinary proposal. The primitive does not bypass
  production fork choice, finality, transaction validation or consensus checks.

## Cucumber assertions

The [audit scenarios](../tests/cucumber_tests/features/security_audit_v1.feature)
specify the checks on observed executions. Those checks do not establish every
implementation invariant for every possible execution.

## Observability limits

A successful local enqueue does not prove transmission or receipt. Missing log
stages or a block unavailable through the API do not establish rejection.
Acceptance or storage does not prove canonical selection or finalization.
Sampling can miss intervening reorganizations, and unavailable ancestry leaves
the corresponding safety comparison unverified.

## Out of scope and unproven properties

V1 does not force leadership wins, create intentionally invalid blocks,
implement withholding strategies or network partitions, manipulate adversarial
stake, or schedule attacks deterministically. These checks do not prove general
consensus safety, liveness, or production security thresholds under all network
conditions or sustained adversarial strategies.
