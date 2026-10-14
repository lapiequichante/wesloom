# 78. Three API tiers, pinned by public-API snapshots

Date: 2026-10-14

Status: Accepted

## Context

Plan6's C2. Every crate is at `0.0.0` and has a large public surface, with
no record of what is meant to stay: about 13,500 lines of `cargo
public-api` output across eight crates. `pub` has been the default for
anything another crate or a test wanted, so internals such as the
compiler's passes, the scheduler's pools and the editor's widgets read as
API.

Two surfaces are already versioned and checked:
- **The documents** (node format, pipeline, scene) carry `version` and
  `abi` (ADR 0044).
- **The C ABI** has `WXSL_ABI_VERSION`, and its generated header is
  test-pinned (ADR 0049).

The Rust API has neither. It also has documentation gaps: rustdoc
reported 21 broken links, and nothing ran `cargo doc` in CI. The C/C++
side has only the headers' one-line comments; the JSON requests behind the
C ABI are documented nowhere outside the Rust source.

We want the API to stabilize gradually: decide now what is meant to stay,
see every change to it, and freeze it at `0.1.0` (C4) rather than all at
once.

## Decision

**Three tiers.**

- **Stable** (the promise from `0.1.0`):
  - the document formats;
  - the C ABI (`wxsl.h`, `wxsl_host.h`);
  - in Rust, the names a crate exports **at its root**, with their
    methods and fields. This applies to `wxsl`, `wxsl-core`, `wxsl-lang`,
    `wxsl-stdlib`, `wxsl-render` and `wxsl-editor`.
  A stable change after `0.1.0` needs a CHANGELOG entry. A removal is
  deprecated for one minor version first.
- **Unstable:** anything reachable only through a module path
  (`wxsl_core::codegen::…`, `wxsl_render::graph::…`) and the whole of
  `wxsl-frame`, which hosts reach through `wxsl-render` or the C ABI. It
  is public, documented and usable, and it may change in any minor
  version. Each crate's root doc says so in a "Stability" section.
- **Internal:** not public. A `pub` item nothing outside its crate uses
  becomes private when it is found. The first case: `wxsl-lang`'s pass
  modules (`compile`, `cond`, `emit`, `mono`, `parse`, `resolve`, …)
  become private, and their entry points stay re-exported at the root.

This makes the root re-exports the stable list, so they are curated:
before `0.1.0`, each crate root names only what we mean to keep.

**Snapshots.** `api/<crate>.txt` holds the output of
`cargo public-api -s --all-features` for each library crate.
`bash scripts/public-api.sh` compares the current output with the
snapshots, and `--bless` rewrites them. CI runs the check, so every change
to a public item appears in the diff of the PR that makes it. The tool
reads rustdoc's JSON, which only nightly produces. The nightly
(`nightly-2026-10-13`) and the tool's version (`cargo-public-api 0.52.0`)
are pinned in the script and in CI, because the JSON format changes between
nightlies. The workspace itself still builds on stable.

**Documentation gates.**
- **Rust:** `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
  --all-features --no-deps` runs in CI. `missing_docs` is on in every
  library crate, and clippy's `-D warnings` already makes it an error.
- **C and C++:** `docs/cpp.md` is the guide for a C or C++ host. It covers:
  - the two ways to integrate: offline bundles, or the runtime compiler;
  - ownership and status codes;
  - the JSON request and response of each `wxsl_compile_*` call;
  - the `wxsl::Renderer` and `wxsl::Compiler` classes.
  The headers carry the reference comments.

## Alternatives considered

**`cargo-semver-checks`.** It compares against a published release, and
nothing is published yet. It is the right tool after `0.1.0`, alongside
the snapshots.

**A rustdoc-JSON test written in-house, on stable with `RUSTC_BOOTSTRAP`.**
It avoids the nightly but rewrites what `public-api` does, against a format
that changes, behind a flag meant for compiler bootstrapping.

**Marking tiers with `#[doc(hidden)]` or an `unstable` feature.** Hidden
items lose their documentation, which unstable items need. A feature flag
would make the editor, the examples and the tests depend on a flag to use
internals they legitimately use. Module path versus root is already in the
code, costs nothing, and is visible in rustdoc.

**Doxygen for C++.** It is not needed for one C header and two classes,
and it would add a tool to CI. The header comments are written so Doxygen
can read them if someone wants HTML later.

## Consequences

- A PR that changes a public item must run `bash scripts/public-api.sh
  --bless` and commit the snapshot diff. Reviewers see the API change as a
  diff.
- Bumping the pinned nightly may change the output's formatting without
  any API change. The bump is then its own commit: re-bless, with no code
  change.
- Root re-exports are now a commitment. The first curation took the
  renderer's machinery off `wxsl-render`'s root: the resource pool, the
  pipeline and variant caches, and the shipped effects' ids. It also took
  `mangle`, `Origins` and `Tok` off `wxsl-lang`'s. A type that appears in a
  stable signature stays at the root (`CacheStats`, `SwapProgress`,
  `DeviceCaps`, `Label`).
- `wxsl-editor` is an application. Its modules are private, and its
  surface is `Editor`, `EditorConfig` and what their methods return. Crates
  with private modules enable `unnameable_types`, so a public signature
  cannot leak a type nobody can name.
- `wxsl-lang`'s passes can no longer be called one by one from outside. The
  corpus test uses the root re-exports.
- The node format's and the C ABI's own version rules are unchanged;
  this ADR only adds the Rust side.
