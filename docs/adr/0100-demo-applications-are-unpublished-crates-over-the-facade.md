# 100. Demo applications are unpublished crates over the facade

Date: 2026-10-20

Status: Accepted

## Context

The demos so far are examples of the `wxsl` facade (`viewer`, `gallery`,
`cinema`). `wxsl drive` is bigger: me_car on a procedural motorway
viaduct that loops past a procedural city, at any hour. It needs its own
nodes, an effect, procedural geometry, an IES profile, a UI and tests for
all of that. As an example it would sit in a crate whose dev-dependencies
every test pays for, and its modules would have no unit tests of their
own.

## Decision

- A demo that outgrows one example becomes a crate under `crates/` with
  `publish = false` and a binary. `wxsl-drive` (binary `drive`) is the
  first.
- It depends on the `wxsl` facade with the features it needs, plus
  `wxsl-lang` to derive its own nodes. No library crate depends on it.
- Its nodes are `.wxsl` files in its `shaders/` directory, mounted at
  `package::drive::<name>` and registered as `drive.<name>` when it
  starts, the way the editor mounts `user.*` functions (ADRs 0020, 0044).
  Its sky is a screen effect of its own over
  `lighting.sky_single_scattering` (ADR 0060).
- It uses only what the libraries already offer: pipeline documents and
  pass-list edits (ADRs 0021, 0033), the light grid (ADR 0076), IES
  layers (ADR 0097), bone textures (ADR 0099) and the UI layer
  (ADR 0013). A capability it lacks goes into a library first.

## Alternatives considered

- **Another example in `crates/wxsl`.** The code fits, but its pure
  parts (the road, the terrain, the city, the time of day, the slider
  layout) could not have their own unit tests, and its dependencies would
  land on the facade's test build.
- **A crate outside the workspace.** It would drift from the libraries'
  APIs unnoticed; inside, `cargo clippy --workspace` and `cargo test
  --workspace` cover it.

## Consequences

- `cargo test -p wxsl-drive` runs its device-free tests. `cargo run -p
  wxsl-drive --release -- --screenshot out.png` is its picture.
- It is not part of the release gate (ADR 0077) and has no API snapshot:
  it exports nothing.
- `AGENTS.md` lists it under "Where things are".
