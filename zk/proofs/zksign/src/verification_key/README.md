The verification key is copied from the `signature/verification_key.json`
artifact in logos-blockchain-circuits **v0.5.7**, matching the workspace's
`lbc-signature-sys` pin. It is embedded so verification does not require the
native witness generator or a circuit artifact download.

When upgrading `lbc-signature-sys`, update the circuits tags and Nix inputs,
then regenerate both the embedded verification key and the valid proof fixture:

```sh
cargo test --locked -p logos-blockchain-zksign --features prove --lib \
  proving::tests::regenerate_signature_fixture -- --ignored --exact
```

The generator uses secret keys 1 through 32 and message 42, validates the proof
against the newly pinned native verification key, and writes both assets. It
does not rely on the old embedded key, so it works after a circuit upgrade.
Groth16 proving is randomized; regenerated proof bytes can differ for the same inputs.

Update the version recorded above and rerun the regular tests afterward. The
key-parity test checks that the embedded key matches the selected native circuit;
verification-only tests check the saved proof without a prover. The generator
is ignored during ordinary test runs and only compiled with `prove`.

In `.github/workflows/code-check.yml`, the pull-request `tests` job runs
`embedded_key_matches_pinned_circuit` through workspace nextest with
`--all-features`, which enables `prove`. The verification-only tests run
separately in the `lints` job.
