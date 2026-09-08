# Contributing

## Start here

[`AGENTS.md`](AGENTS.md) is the actual contributor guide — build/lint/test
commands, crate-boundary rules, and when a change needs a new
[ADR](docs/adr/README.md). It's written for AI coding agents, but it's the
same guide for humans; there's no separate process.

## Licensing of contributions

By contributing to any crate in this workspace, you agree your contribution
is licensed under this project's dual MIT/Apache-2.0 license.

If you're adding a function to `wesloom-stdlib`, it must be your own
original implementation — inspiration from how other libraries or engines
solve a problem is fine, transcribing their code is not, regardless of
their license. See
[ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
and `crates/wesloom-stdlib/shaders/README.md` for why and for the exact
rule.

## Before opening a PR

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

If your change touches crate boundaries or the feature-flag matrix, also
run `cargo check --workspace --no-default-features` and
`cargo check -p wesloom --features editor` — see
[ADR 0002](docs/adr/0002-cargo-workspace-crate-boundaries.md) for why both
matter.

## Commit / PR expectations

- Keep PRs scoped to one crate/concern where possible; cross-crate
  refactors are fine when the change genuinely spans the boundary (e.g. a
  new node trait method), but avoid bundling unrelated work.
- If a PR makes an architectural decision (new public trait boundary, new
  dependency, new feature flag), include the ADR that records it — see
  `AGENTS.md`'s "Writing an ADR" section.
