# 0007. Original shader standard library instead of a LYGIA port

Date: 2026-09-08

Status: Accepted

## Context

[ADR 0006](0006-lygia-port-licensing-and-isolation.md) planned to rewrite
[LYGIA](https://lygia.xyz) into WXSL and ship it as `wxsl`'s base node
library, isolated into its own crate specifically because LYGIA's
Prosperity Public License 3.0.0 is noncommercial-by-default (a 30-day trial
for commercial use, then a paid license from the LYGIA project). Having
scaffolded that crate, the project decided this tradeoff isn't worth it:
even fully isolated behind an opt-in feature, "the base node library most
people will reach for requires a commercial license past 30 days" is a
real adoption cost for a library that otherwise wants to be ordinary
permissively-licensed Rust, and a derivative rewrite (renaming identifiers,
changing syntax) doesn't escape that license regardless of how it's
packaged.

The actual thing LYGIA offered wasn't unreplaceable code, it was a proven
*shape*: small, single-purpose, well-categorized shader functions that
compose. That shape isn't copyrightable, and other references exist for
learning specific techniques without licensing entanglement —
[Babylon.js](https://github.com/BabylonJS/Babylon.js), for instance, is
Apache-2.0 (permissive), and general graphics techniques (PBR BRDFs, SDF
primitives, noise functions, tone mapping curves) are broadly documented
across papers, GDC talks, and multiple permissively-licensed engines, not
owned by any one library.

## Decision

- Drop the LYGIA port. Delete `wxsl-lygia`; ADR 0006 stays in the repo
  marked superseded by this one (see `docs/adr/README.md`'s "don't delete
  history" rule).
- Replace it with **`wxsl-stdlib`**: an original shader function
  library, written from scratch, organized the way LYGIA (and similar
  libraries) organize themselves — one small function per file, grouped
  into categories (`math/`, `color/`, `space/`, `lighting/`, `generative/`,
  `sdf/`, `sample/`, `animation/`, `filter/`, `distort/`) — because that
  granularity is a good design independent of where the idea to use it came
  from.
- Implementations may be *informed* by how other references (LYGIA's
  category breakdown and API shapes, Babylon.js's shader techniques,
  papers, documented algorithms) approach a problem, especially where a
  more efficient formulation than the "obvious" one is known — but every
  function's code is written independently, not transcribed or lightly
  renamed from any source. `crates/wxsl-stdlib/shaders/README.md` states
  this as the authoring rule for anyone (human or agent) adding a function.
- `wxsl-stdlib` is an ordinary crate under the workspace's default
  MIT/Apache-2.0 — no special license file, no `publish = false`, no
  isolation for legal reasons. It stays a separate crate from
  `wxsl-core` purely for the compile-time/binary-size reason ADR 0002
  already gives (a large, independently-growing library shouldn't bloat
  the crate every consumer needs).
- Because there's no licensing reason to hide it anymore, `wxsl`'s
  `stdlib` feature defaults **on** (unlike the old `lygia` feature, which
  defaulted off). `render` also stays on by default; only `editor` remains
  opt-in.

## Alternatives considered

- **Keep the LYGIA port as an additional, still-opt-in option alongside an
  original stdlib.** Rejected: maintaining two node libraries with
  overlapping purpose (one encumbered, one not) is confusing for users
  ("which do I use?") and doubles the maintenance surface for no benefit
  once the original library covers the same ground.
- **Get a Patron license from the LYGIA project to clear the commercial
  restriction.** Not pursued: that's a business relationship and ongoing
  cost tied to one upstream project's terms, for functionality that can be
  written independently; it doesn't remove the constraint for anyone
  downstream who redistributes `wxsl` further without also holding that
  license.
- **Vendor Babylon.js's Apache-2.0 shader code directly.** Rejected even
  though the license would technically allow it (with attribution/NOTICE):
  the goal stated for this library is original, WXSL-native
  implementations designed around this project's node/type system, not a
  transcription of another engine's GLSL — and mixing "some functions are
  literal ports, some are original" would blur the authoring rule above
  for no real gain, since Babylon's techniques are learnable without
  copying its code.

## Consequences

- `crates/wxsl-lygia/` is gone; anything in `docs/` or `AGENTS.md`
  referencing it should point to `wxsl-stdlib` and this ADR instead
  (this ADR's own PR updates all of those).
- No `NOTICE.md`/porting-log ceremony is needed for `wxsl-stdlib` the
  way ADR 0006 required for the LYGIA port — normal PR review is enough,
  same as any other crate. The authoring rule
  (`crates/wxsl-stdlib/shaders/README.md`) is a code-review concern, not
  a legal one: catch a too-close transcription the way you'd catch any
  other quality issue, not because a license depends on it.
- Rebuilding a library as broad as LYGIA's from scratch is a materially
  larger effort than porting it would have been — this trades a licensing
  problem for a longer roadmap, deliberately.
- If a specific function turns out to need a technique whose only known
  description is inside a non-permissively-licensed source, treat that
  function the way ADR 0006 treated all of LYGIA: skip it, or isolate that
  one function's crate the same way, rather than quietly absorbing the
  constraint into `wxsl-stdlib`.
