# 0044. Identity is namespaced, documents pin versions, and the capability check runs before anything is built

Date: 2026-09-27

Status: Accepted

Implements plan3's P8, whose own first instance had already shipped: the
feature handshake ([ADR 0037](0037-semantic-channels.md)) names a material
whose feature its pipeline does not carry — for the one case whose facts
were computed. P8 generalizes it, and the plan named the three pieces:
namespaced ids, schema versions, and one device-free check over published
metadata. It was ordered before M8's second pipeline shape and before any
cross-author exchange is invited, because contracts are cheap before there
are two parties and expensive after.

## Context

Every registry id in the workspace was spelled however its crate spelled
it. Node ids were dotted (`math.add`), with the category — effectively the
package — derived from the first segment. Effect ids were bare
(`bloom`, `tonemap`). Lighting-model names were bare (`pbr`). Nothing
validated spelling anywhere: a library registering an effect named `grain`
and another doing the same could not share a registry, and a document that
said `grain` meant whichever won. Meanwhile not one serialized document —
node format, scene, pipeline — carried a version; forward compatibility was
`#[serde(default)]` and nothing told a reader that a file meant something
its vocabulary no longer says. And the handshake facts existed but lived in
three places: the plan in `PipelineConfig`, the effect names in the
document, the per-material demand in the resolve step — reachable only by
compiling, one material per attempt, after mesh upload had begun.

## Decision

* **The dot is the namespace separator, and the first segment is the
  package that owns the name.** One syntax across the registries —
  `math.add`, `wxsl.bloom`, `wxsl.pbr` — matching what node ids already
  did. The `::` of WXSL module paths stays what it was: those name shader
  imports, this names registry entries, and a second `::` layer would
  collide visually with the first. The shipped effects and models move
  into the shipped package (`wxsl.bloom`, `wxsl.pbr`); the shipped nodes
  keep their categories as their packages. `wxsl_core::identity` is the
  one module that knows the rule.
* **Registration says who owns the name; documents may not have to.** The
  registries reject an un-namespaced registration — `NodeRegistry::register`
  and `EffectRegistry::add` panic with the id in the message,
  `LightingSet::new` returns `UnnamespacedModel` — while *lookups* resolve
  a bare spelling against the shipped package: a document that says
  `bloom` means `wxsl.bloom`, exactly what it meant before namespacing
  existed, and `by_name("pbr")` means `wxsl.pbr`. Every document,
  command line and scene already written keeps working; every
  *registration* after this one carries a package. Features keep their
  bare names — there is no registration path to guard (the table is
  const) and their pinned spelling is a macro name that was never bare.
* **The rule's first customer was the shipped set**: the pipeline
  vocabulary's `present` node — the one bare node id in the workspace — is
  now `output.present`, beside the ABI's `output.surface` and
  `output.vertex`. Documents written by the pipeline canvas before this
  item named `present` and will fail validation with the definition's new
  name in the error; nothing is released, and the rename is recorded here
  rather than papered over with an alias a palette would have to hide.
* **Every serialized document carries `version` and `abi`.** The `Graph`
  wire (the node format *and* the pipeline documents — one wire shape) and
  the `Scene` wire grew the two fields; `abi` pins
  [`abi::REVISION`](../../crates/wxsl-core/src/abi.rs), today 1. Absent
  fields read as *this build's* — a file from before versioning parses
  unchanged — while a newer schema version or a foreign ABI revision is
  refused by name at parse, naming both numbers, because both are changes
  in what stored names mean and guessing is how documents reshade
  silently. The check lives in the wire types' `try_from` conversions, so
  every `serde_json::from_str` call site in the workspace gained it without
  changing, and every save stamps the current pair.
* **The setup publishes its capabilities, and the check is one
  device-free call.** `wxsl_render::setup` holds `RenderSetup` — a
  pipeline document, the effects its passes may name, and the config the
  pass lists are built under: the same three facts a `Renderer` is
  configured with, held before any device exists.
  `RenderSetup::capabilities` publishes the provided stages, the channel
  plan, the lighting set and the required effects as one value;
  `RenderSetup::check(&scene, &registry)` returns *every*
  [`Incompatibility`](../../crates/wxsl-render/src/setup.rs) between the
  scene and the setup, by name — a plan that does not build, an effect no
  registered effect provides, a model the lighting set does not enable, a
  material pinning a feature macro the plan does not carry, or a material
  that does not compile — rather than stopping at the first, because one
  error per attempt is a debug session, not a diagnostic.
* **The load runs the same check.** `check_scene` is shared, and
  `SceneResources::load_with_plan` calls it after the document's own
  validation and *before the first mesh upload*: a scene that does not fit
  is refused once, completely, in a `LoadError::Incompatible` listing, and
  nothing is built on its behalf. A plan that does not build is reported
  and stops the check there — the generated G-buffer struct *is* the plan,
  so no per-material answer can mean anything under it. The per-material
  half compiles each scene material device-free through the same resolve
  and codegen `Material::with_lighting` runs, so what the check refuses is
  exactly what the load would have refused — the point is the timing and
  the completeness.
* **The proof**: the GPU test in `semantic_channels` loads a scene whose
  material pins `wxsl_subsurface` against a plan without the channel and
  holds the named refusal at load, then loads the same document under the
  plan that carries it; `wxsl_render`'s device-free tests hold the
  capabilities value, the collect-everything rule, and the bare-spelling
  resolutions. `cargo run -p wxsl --example scene_check` runs the whole
  item from a command line: capabilities printed, the mismatch named, the
  load refusal shown, and — with `--screenshot` — the matched scene
  rendered.

## Alternatives considered

* **`package::name` with `::`.** The plan's own spelling, but the
  workspace already has a `::` identity — WXSL module paths, rooted at the
  literal keyword `package` — and a registry id spelled `wxsl::bloom`
  would read as a module path it is not. Node ids have been dotted since
  the first registry, and the category-as-package reading needed no new
  parser.
* **A reserved-namespace list** the registries check against ("nobody
  but the shipped set may register `math.*`"). Rejected: a second table
  that has to be told when stdlib grows a category, and override-by-id is
  a supported gesture — the facade's graph tonemap *replaces* the shipped
  descriptor. The rule that earns its keep is the cheaper one: say who
  owns the name, and collisions become loud instead of impossible.
* **Refusing bare ids in documents.** Strictly more forward-looking, and
  it would have broken every document in existence — the editor's saves,
  the gallery demos, every scene naming `pbr`. Resolution keeps them
  meaningful and costs one lookup.
* **A schema version only, no ABI pin.** A format can keep its shape
  while its vocabulary changes meaning — a renamed socket, a retired node
  id, a setting whose values moved. The ABI revision is the number that
  moves when that happens, and one number covers all three document kinds
  because they share the vocabulary and drift together.
* **`Renderer::capabilities`** instead of a separate setup value.
  Rejected for the same reason the check is device-free: the question is
  "will this setup take this scene", asked *before* a device builds
  anything, and the editor already reads the running renderer's parts
  (ADR 0043). The setup is the application-side twin, built from the same
  three facts the renderer is given.
* **Leaving the load's per-material errors as they were** — one
  `LoadError::Material` per attempt, after the meshes were already on the
  device. That is precisely the gap the item exists to close; the shared
  `check_scene` is the same compile the load already ran, moved to where
  the answer is complete.

## Consequences

* The shipped presets name `wxsl.tonemap` explicitly, and the derived
  compute rows grow a segment (`pass.compute.wxsl.brdf_lut`) — the prefix
  strip is positional, so nothing downstream cared. An effect id is also
  the params struct's name, so the generated block spells the package
  with an underscore (`wxsl_bloom_params`); `params_header` is the only
  place an id becomes WGSL.
* Registering without a package is now a programming error at three
  sites. In-repo, every registration already complied once the shipped
  ids moved; an external library registering `grain` today gets a named
  panic telling it what to do instead of a silent collision later.
* The editor's palette shows the resolved ids, and an effect row's
  preset writes the namespaced spelling into the document; a search for
  `bloom` finds the row the same way it did (labels did not change).
* `LoadError` grew `Incompatible`; `SceneResources::load` now runs the
  material compile twice — once device-free in the check, once to build —
  which at document scale is sub-millisecond and buys the complete,
  pre-build refusal.
* `wxsl-core` grew a `serde_json` dev-dependency, for the wire-version
  tests; the dependency matrix is unchanged (dev-deps do not ship).
* The scene document gained its first on-disk example,
  `crates/wxsl/assets/scene_check.scene.json` — the copyable scene
  document, as `pbr_cube.wxsl.json` is for the node format.
* If this changes — a second namespace syntax, a per-format ABI revision,
  capability metadata about a *running* renderer — this ADR and
  `wxsl_core::identity`'s module doc are the two texts to update first,
  and `AGENTS.md`'s registry conventions restate the rule.
