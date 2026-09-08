# 0006. LYGIA port: licensing and crate isolation

Date: 2026-09-08

Status: Superseded by [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md)

**2026-09-08 update:** kept for the record — it documents real research
(LYGIA's actual license terms) that's still worth having written down
somewhere, and the crate-isolation instinct it argues for was reused for
`wesloom-stdlib`. But the project decided against porting LYGIA at all; see
ADR 0007 for why and for what replaced this plan. Everything below reflects
the superseded plan, not the current one.

## Context

[LYGIA](https://lygia.xyz) is a large, well-established, granular shader
function library (GLSL/HLSL/MSL/WGSL/CUDA) by Patricio Gonzalez Vivo, and
the project wants to offer its functions as ready-to-use `wesloom` base
nodes, rewritten in WESL.

LYGIA is **not** MIT/Apache/BSD licensed. Its
[`LICENSE.md`](https://github.com/patriciogonzalezvivo/lygia/blob/main/LICENSE.md)
is the **Prosperity Public License 3.0.0**:

- Free for noncommercial use (personal projects, research, education,
  charitable/public institutions) with no restriction.
- Commercial use gets a single **30-day trial period** (per company, not
  per person), after which continued commercial use requires a paid
  "Patron" license from the LYGIA project.
- Contributing changes back to LYGIA under a standard permissive license
  (MIT, Apache-2.0, BSD-2-Clause, Blue Oak 1.0.0) does *not* count as
  commercial use — i.e. upstreaming a fix is always fine regardless of who
  you are.
- Every recipient of the software (or any part of it, changed or not) must
  also receive the license text and the original contributor/source-code
  attribution lines.

A WESL rewrite of a LYGIA function — porting its algorithm and structure
into another shading language — is a **derivative work** of that function,
not an independent implementation, regardless of identifier renaming or
syntax differences. It therefore inherits this license; we cannot
relicense it to MIT/Apache-2.0 just by rewriting the syntax.

This directly conflicts with the rest of `wesloom` being ordinary
permissively-licensed Rust: if the LYGIA-derived nodes lived in the same
crate as everything else, the *entire crate* — including code with nothing
to do with LYGIA — would need to carry Prosperity's terms, or we'd need to
carefully partition license notices file-by-file within one crate (fragile,
easy to get wrong, and confusing for downstream users trying to figure out
what they're allowed to do).

## Decision

- The LYGIA port lives entirely in its own crate, `wesloom-lygia` (ADR
  0002), which carries the Prosperity Public License 3.0.0
  (`crates/wesloom-lygia/LICENSE`, verbatim from upstream) instead of the
  workspace default. `crates/wesloom-lygia/Cargo.toml` sets `license-file`
  instead of inheriting `workspace.package.license`.
- `crates/wesloom-lygia/NOTICE.md` explains the license, attributes LYGIA
  and its author, and keeps a porting log (upstream path → wesloom node →
  upstream commit ported from) so provenance is always traceable per node.
- The crate is `publish = false` and excluded from the `wesloom` facade's
  default features (`lygia` feature, opt-in, ADR 0002's feature table).
  Nobody depends on `wesloom` and gets Prosperity-licensed code without
  explicitly asking for it.
- `AGENTS.md` states the hard rule directly: never move code out of
  `wesloom-lygia` into another crate, and never write a LYGIA "port" that
  isn't genuinely independent and claim it's clean-room.
- Downstream commercial users of `wesloom` have three options once the
  `lygia` feature's 30-day trial value applies to them: get a Patron
  license from the LYGIA project, don't enable the `lygia` feature and
  author/source their own node implementations for whatever functions they
  need, or (per the Contributions Back clause) contribute a permissively-
  licensed equivalent upstream to LYGIA itself, which would let a *future*
  ADR revisit this crate's license if LYGIA's own terms ever changed for
  that function. None of this is legal advice; a commercial user should
  confirm their own obligations with the LYGIA project or counsel.

## Alternatives considered

- **Don't port LYGIA at all; write original nodes inspired by common
  techniques.** Rejected for now: LYGIA's breadth and maturity is exactly
  why the user wants it, and "inspired by" still risks being a derivative
  work if it follows LYGIA's structure too closely — isolating and
  attributing honestly is safer than an unclear reimplementation that
  claims independence it doesn't have.
- **License the whole `wesloom` workspace under Prosperity to match.**
  Rejected: makes the *entire* project noncommercial-by-default, which
  contradicts the goal of `wesloom` being a usable general-purpose
  rendering library; the whole point of isolating LYGIA is that it's an
  optional ingredient, not the foundation.
- **Vendor LYGIA's original GLSL/WGSL source unchanged instead of
  rewriting in WESL.** Rejected independently of licensing: it wouldn't
  compose through WESL's import system with the rest of the project (ADR
  0003), which is the actual point of the port.

## Consequences

- `cargo publish -p wesloom` with the `lygia` feature enabled needs a
  decision this ADR doesn't make yet (an optional path dependency on a
  `publish = false` crate). Revisit when publishing is actually on the
  table; until then the whole workspace is unpublished v0.0.0 scaffolding.
- Any PR touching `crates/wesloom-lygia/shaders/` must update
  `NOTICE.md`'s porting log in the same PR.
- If LYGIA's own license ever changes (version bump, or a function's author
  moves it to a permissive license), this ADR and the crate's `LICENSE`
  need re-review — don't assume the terms captured here are permanently
  current.
