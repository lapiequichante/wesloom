# Contributing

[`AGENTS.md`](AGENTS.md) is the contributor guide, for humans and coding
agents alike. It covers the commands, which tests to run, the
crate-boundary rules, and when a change needs an
[ADR](docs/adr/README.md).

## Licensing of contributions

By contributing, you agree that your contribution is licensed under the
project's dual MIT/Apache-2.0 license.

A `wxsl-stdlib` function is either original code or a port from an audited
MIT, Apache-2.0 or BSD source. A port keeps its provenance and notices
([ADR 0053](docs/adr/0053-permissive-shader-ports-retain-their-provenance.md)).
The exact rule is in `crates/wxsl-stdlib/shaders/README.md`.

## Before opening a PR

Run the checks in `AGENTS.md`'s "Commands" section, and the tests its table
routes your change to. A change to the shader ABI, the pipelines or the
node library should also render:

```sh
cargo run -p wxsl --example pbr_cube -- --headless
```

The two pipelines' images should agree to within 8-bit rounding.

## Pull requests

- Keep each PR to one concern. Cross-crate changes are fine when the change
  really spans the boundary.
- A PR that makes an architectural decision includes its ADR.
