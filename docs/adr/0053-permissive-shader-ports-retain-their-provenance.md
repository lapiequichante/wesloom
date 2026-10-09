# 0053. Permissive shader ports retain their provenance

Date: 2026-10-09

Status: Accepted

Implements plan4 S1. Supersedes the blanket originality rule of
[0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md); its rejection
of the non-permissive LYGIA port and crate/feature decisions remain in force.

## Context

S2–S4 need broader shader coverage. Explicitly licensed ports can accelerate
that work without copying a foreign engine's architecture or hiding provenance.

## Decision

Original implementations remain welcome. Ports from MIT, Apache-2.0 and
BSD-2/3-Clause sources are allowed after checking the exact file, its pinned
revision, dependency/provenance chain and applicable notices. Other licenses
require an explicit review before copying; a checkout is not permission to port.
Copyleft, noncommercial, custom or unclear sources are technique-only references.

A port is re-expressed in WXSL idiom: one node function per file, documented
socket defaults, explicit macros, named imports and guarded degenerate inputs.
Its leading comment states SPDX license, original copyright, repository/path,
full commit, original symbol and changes. Preserve upstream notices/license text
and any applicable Apache NOTICE. Add an entry to root `NOTICE` and the owning
crate's packaged `NOTICE`; binary applications can obtain the stdlib's complete
attributions through `wxsl_stdlib::THIRD_PARTY_NOTICES`.

Workspace-owned code stays MIT OR Apache-2.0. Upstream portions retain their
own licenses: attribution does not relicense them. Reassess the package's SPDX
expression before introducing Apache-only or other differently licensed code;
an imported Apache component cannot be advertised as freely MIT-only. Ship
required notices with source packages and downstream binaries, not only refs/.

`refs/` contains ignored, non-submodule research checkouts, never build inputs.
Their commit/path inventory is recorded in the S2–S4 guide. Engine assets and
third-party folders are not implicitly covered by an engine's root license.

## Alternatives considered

- Keep every function original: slower coverage growth, without a licensing
  benefit for audited permissive sources.
- Paste engine shader chunks: hides dependencies, bindings and graph contracts.

## Consequences

The first worked port is three.js's `IBLSheenBRDF`, exposed as the granular
`lighting.sheen_ibl_response` node. It is a bounded fit, not a complete sheen
lighting model. Existing functions are not retroactively relabelled as ports.
Corpus/graph compilation, node derivation and rendered backend parity remain
mandatory gates. S2–S4 guides are proposals; new model/IBL/pyramid architectural
choices still need their own ADR before implementation.
