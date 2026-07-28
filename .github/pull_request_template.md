<!-- Keep it short. What changed, and why. -->

## What

## Why

Closes #

## Checks

- [ ] `cargo fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings` pass
- [ ] `cargo test` passes; new behaviour has a test, bug fixes have a regression test
- [ ] `cargo run --example eval` reports no regression — and if a number improved,
      `evals/baseline.json` is updated **in this PR**
- [ ] `cargo check --no-default-features` still builds
- [ ] Any new constant carries a comment showing what was measured to choose it,
      or says plainly that the suites could not price it

## Notes for the reviewer

<!--
Rationale for non-obvious decisions. If you touched any of these, say so here:

- the schema (there is no migration path — existing brains become unreadable)
- norm::key, or anything that decides entity identity
- the isolation rules in src/store/mod.rs
- a ranking weight or threshold, with the sweep that justifies the new value
-->
