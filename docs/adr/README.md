# Architecture Decision Records

An ADR captures one architecturally significant decision: the context that
forced it, what we decided, and what it costs. We keep them because this
project is built collaboratively with AI agents across many sessions with
no shared memory between them — the ADR log is how a decision made in one
session survives into the next one, instead of getting silently re-litigated
or reversed.

Numbers are sequential and never reused. Superseded ADRs stay in the repo
with an updated `Status` line pointing at whatever replaced them — history
is signal, don't delete it.

## Read these first

The ten decisions the rest of the log builds on:

* [0002](0002-cargo-workspace-crate-boundaries.md): the crate boundaries.
* [0011](0011-own-the-shading-language.md): WXSL, our own shading language.
* [0020](0020-a-node-definition-is-derived-from-its-wxsl-source.md): a node is its `.wxsl` file.
* [0022](0022-material-stages-replace-the-render-path-enum.md): one material, many stages.
* [0025](0025-a-material-graph-spans-shader-stages.md): a material spans the vertex and fragment stages.
* [0028](0028-lighting-models-dispatched-by-a-g-buffer-id.md): lighting models and generated shading.
* [0033](0033-pipelines-are-documents.md): pipelines are documents.
* [0034](0034-effects-are-first-class-units.md): effects are declared units.
* [0038](0038-a-materials-configuration-is-one-value.md): a material's configuration is one value.
* [0049](0049-share-the-core-through-a-data-only-c-abi.md): the data-only C ABI behind the second backend.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-cargo-workspace-crate-boundaries.md) | Cargo workspace layout and crate boundaries | Accepted, amended by [0048](0048-the-frame-plan-is-device-free.md), [0049](0049-share-the-core-through-a-data-only-c-abi.md) |
| [0003](0003-wesl-as-the-shading-language.md) | WESL as the shading language, `wesl`/`wesl-cli` as the compiler | Superseded by [0011](0011-own-the-shading-language.md) |
| [0004](0004-node-editor-is-an-optional-additive-ui-layer.md) | Visual node editor is an optional, additive UI layer | Accepted, amended by [0013](0013-the-editor-draws-itself-with-wxsl-render.md), [0019](0019-node-colour-and-name-are-instance-metadata.md) |
| [0005](0005-render-pipeline-abstraction-and-shader-switching.md) | Render pipeline abstraction with automatic forward/deferred shader switching | Accepted, amended by [0021](0021-a-declarative-render-graph-and-a-scene-document.md), [0022](0022-material-stages-replace-the-render-path-enum.md) |
| [0006](0006-lygia-port-licensing-and-isolation.md) | LYGIA port: licensing and crate isolation | Superseded by [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md) |
| [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md) | Original shader standard library instead of a LYGIA port | Originality rule superseded by 0053; other decisions Accepted |
| [0008](0008-surface-graphs-and-a-named-shader-abi.md) | A material graph describes a surface, against a named shader ABI | Accepted, amended by [0020](0020-a-node-definition-is-derived-from-its-wxsl-source.md), [0021](0021-a-declarative-render-graph-and-a-scene-document.md), [0023](0023-a-material-declares-its-resources.md), [0024](0024-a-material-declares-the-geometry-it-requires.md), [0025](0025-a-material-graph-spans-shader-stages.md), [0040](0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md), [0050](0050-host-shared-layouts-are-generated.md) |
| [0009](0009-the-application-supplies-the-shader-library.md) | The application supplies the renderer's shader library | Accepted |
| [0010](0010-four-bind-groups-allocated-by-update-frequency.md) | Four bind groups, allocated by update frequency | Accepted, amended by [0021](0021-a-declarative-render-graph-and-a-scene-document.md), [0023](0023-a-material-declares-its-resources.md), [0024](0024-a-material-declares-the-geometry-it-requires.md) |
| [0011](0011-own-the-shading-language.md) | Own the shading language: WXSL replaces WESL | Accepted |
| [0012](0012-monomorphize-templates-on-the-flat-module.md) | Monomorphize templates on the flat module, with shallow inference | Accepted |
| [0013](0013-the-editor-draws-itself-with-wxsl-render.md) | The editor draws itself with `wxsl-render` | Accepted |
| [0014](0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md) | MSDF text, generated in-tree, from fonts the application supplies | Accepted |
| [0015](0015-generic-sockets-for-arithmetic-nodes.md) | Generic sockets, resolved per node instance, for arithmetic nodes | Accepted, amended by [0018](0018-one-generic-node-per-operation.md) |
| [0016](0016-syntax-highlighting-reuses-wxsl-langs-lexer.md) | The editor's code-panel syntax highlighting reuses `wxsl-lang`'s lexer | Accepted |
| [0017](0017-broadcast-generics-and-disconnect-cleanup.md) | Broadcast generic parameters, and forgetting a resolution on disconnect | Accepted, amended by [0018](0018-one-generic-node-per-operation.md) |
| [0018](0018-one-generic-node-per-operation.md) | One generic node per operation, with WGSL's own operand rules | Accepted |
| [0019](0019-node-colour-and-name-are-instance-metadata.md) | A node's colour and name belong to the node, not to its kind | Accepted, amended by [0023](0023-a-material-declares-its-resources.md) |
| [0020](0020-a-node-definition-is-derived-from-its-wxsl-source.md) | A node definition is derived from its WXSL source | Accepted |
| [0021](0021-a-declarative-render-graph-and-a-scene-document.md) | A declarative render graph, and a scene document the pipeline is not in | Accepted, amended by [0022](0022-material-stages-replace-the-render-path-enum.md), [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md), [0033](0033-pipelines-are-documents.md), [0048](0048-the-frame-plan-is-device-free.md), [0050](0050-host-shared-layouts-are-generated.md) |
| [0022](0022-material-stages-replace-the-render-path-enum.md) | Material stages replace the render-path enum | Accepted, amended by [0025](0025-a-material-graph-spans-shader-stages.md), [0033](0033-pipelines-are-documents.md), [0047](0047-dual-depth-peeling-and-the-baseline-native-split-for-blendable-float-targets.md) |
| [0023](0023-a-material-declares-its-resources.md) | A material declares its resources: uniforms, textures, and the application's slot | Accepted |
| [0024](0024-a-material-declares-the-geometry-it-requires.md) | A material declares the vertex and instance attributes it requires | Accepted, amended by [0027](0027-a-graph-computes-its-own-interpolants.md) |
| [0025](0025-a-material-graph-spans-shader-stages.md) | A material graph spans shader stages, and compiles per pass stage | Accepted, amended by [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md), [0027](0027-a-graph-computes-its-own-interpolants.md), [0032](0032-stage-analysis-computes-the-cut-between-stages.md) |
| [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md) | Shadows: a view per light, a lookup in the shading function, and two flags on the material | Accepted |
| [0027](0027-a-graph-computes-its-own-interpolants.md) | A graph computes its own interpolants | Accepted, amended by [0032](0032-stage-analysis-computes-the-cut-between-stages.md) |
| [0028](0028-lighting-models-dispatched-by-a-g-buffer-id.md) | Lighting models dispatched by a G-buffer id, with a composable G-buffer layout | Accepted, amended by [0031](0031-shader-text-lives-in-templates-generators-fill-holes.md) |
| [0029](0029-a-corpus-gate-and-error-scopes-for-generated-shaders.md) | A corpus gate for generated shaders, error scopes in GPU tests, and the budget numbers from the spec table | Accepted |
| [0030](0030-a-pipeline-config-instead-of-threaded-parameters.md) | One `PipelineConfig` instead of parameters threaded by hand | Accepted, amended by [0048](0048-the-frame-plan-is-device-free.md) |
| [0031](0031-shader-text-lives-in-templates-generators-fill-holes.md) | Shader text lives in template files; generators fill holes | Accepted |
| [0032](0032-stage-analysis-computes-the-cut-between-stages.md) | Stage analysis computes the cut between stages | Accepted |
| [0033](0033-pipelines-are-documents.md) | Pipelines are documents, and the stock ones are preset files | Accepted, amended by [0034](0034-effects-are-first-class-units.md), [0040](0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md), [0048](0048-the-frame-plan-is-device-free.md) |
| [0034](0034-effects-are-first-class-units.md) | Effects are first-class units: declared inputs, a registry, and chains as documents | Accepted, amended by [0035](0035-execution-policies.md), [0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md), [0040](0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md), [0048](0048-the-frame-plan-is-device-free.md) |
| [0035](0035-execution-policies.md) | Execution policies: once, on resize, on demand — and compute effects | Accepted, amended by [0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md) |
| [0036](0036-buffers-are-graph-resources.md) | Buffers are graph resources, with storage bindings in the pass group | Accepted |
| [0037](0037-semantic-channels.md) | Semantic channels: G-buffer requests collected with sources, and the material feature | Accepted, amended by [0038](0038-a-materials-configuration-is-one-value.md) |
| [0038](0038-a-materials-configuration-is-one-value.md) | A material's configuration is one value, resolved once | Accepted |
| [0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md) | Tonemap is an effect, and ambient reads the LUT | Accepted, amended by [0040](0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md) |
| [0040](0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md) | Screen-domain graphs: postprocess is a material over the frame | Accepted |
| [0041](0041-compute-and-buffers-join-the-document-vocabulary.md) | Compute and buffers join the document vocabulary, with sockets derived from the effect declaration | Accepted |
| [0042](0042-effect-parameters-are-uniforms-the-descriptor-declares-them.md) | Effect parameters are uniforms, and the descriptor declares them | Accepted |
| [0043](0043-the-pipeline-canvas-is-a-mode-and-compiles-on-every-edit.md) | The pipeline canvas is a mode, and it compiles on every edit | Accepted |
| [0044](0044-identity-versions-and-the-capability-check.md) | Identity is namespaced, documents pin versions, and the capability check runs before anything is built | Accepted |
| [0045](0045-a-bake-is-an-effect-over-a-material-subgraph.md) | A bake is an effect over a material subgraph, and the table is the material's | Accepted |
| [0046](0046-velocity-is-a-stage-taa-is-a-policyd-chain.md) | Velocity is a stage; TAA is a policy'd chain | Accepted |
| [0047](0047-dual-depth-peeling-and-the-baseline-native-split-for-blendable-float-targets.md) | Dual depth peeling, and the baseline/native split for blendable float targets | Accepted |
| [0048](0048-the-frame-plan-is-device-free.md) | The frame plan is device-free | Accepted |
| [0049](0049-share-the-core-through-a-data-only-c-abi.md) | Share the core through a data-only C ABI | Accepted |
| [0050](0050-host-shared-layouts-are-generated.md) | Host-shared layouts are computed or generated, never mirrored | Accepted |
| [0051](0051-dawn-implements-only-the-device-half.md) | Dawn implements only the device half, offline first | Accepted |
| [0052](0052-runtime-compilation-stays-behind-the-data-abi.md) | Runtime compilation stays behind the data ABI | Accepted |
| [0053](0053-permissive-shader-ports-retain-their-provenance.md) | Permissive shader ports retain their provenance | Accepted |
| [0054](0054-fragment-only-is-a-node-constraint-the-stage-analysis-checks.md) | Fragment-only is a node constraint the stage analysis checks | Accepted |
| [0055](0055-sorting-is-opt-in-per-pass-and-it-sorts-draws-not-passes.md) | Sorting is opt-in per pass, and it sorts draws, not passes | Accepted |
| [0056](0056-a-material-may-shade-forward-under-either-pipeline.md) | A material may shade forward under either pipeline, routed by the preshaded model | Accepted, amends [0022](0022-material-stages-replace-the-render-path-enum.md), [0028](0028-lighting-models-dispatched-by-a-g-buffer-id.md) |
| [0057](0057-transparency-is-tiered-sorted-by-default-peeled-on-request.md) | Transparency is tiered: sorted by default, peeled on request | Accepted, extends [0047](0047-dual-depth-peeling-and-the-baseline-native-split-for-blendable-float-targets.md) |
| [0058](0058-iridescence-is-authored-on-the-surface-and-carried-by-a-model-channel.md) | Iridescence is authored on the surface and carried by a model channel | Accepted, extends 0008 and 0028 |
| [0059](0059-sheen-is-a-surface-layer-with-model-owned-ambient.md) | Sheen is a surface layer with model-owned ambient | Accepted, extends 0008 and 0028 |
| [0060](0060-integrate-single-scattering-as-a-pure-sky-node.md) | Integrate single scattering as a pure sky node | Accepted |
| [0061](0061-texture-mips-and-attachment-subresources-are-frame-data.md) | Texture mips and attachment subresources are frame data | Accepted |
| [0062](0062-prefilter-environments-through-parameterized-face-passes.md) | Prefilter environments through parameterized face passes | Accepted |
| [0063](0063-environment-cubes-are-explicit-frame-resources.md) | Environment cubes are explicit frame resources | Accepted |
| [0064](0064-author-environment-bakes-and-depth-aware-backgrounds.md) | Author environment bakes and depth-aware backgrounds | Accepted |
| [0065](0065-a-pyramid-is-one-document-node-with-scoped-names.md) | A multi-pass recipe is one document node with scoped names | Accepted |
| [0066](0066-pack-shadow-slots-into-a-depth-atlas.md) | Pack shadow slots into a depth atlas | Accepted |
| [0067](0067-trace-refraction-through-peeled-surface-layers.md) | Trace refraction through peeled surface layers | Superseded by 0068 |
| [0068](0068-compose-optical-peels-back-to-front-by-default.md) | Compose optical peels back to front by default | Accepted |
| [0069](0069-cache-local-colour-and-distance-probes-in-the-glass-demo.md) | Cache local colour and distance probes in the glass demo | Superseded by 0070 |
| [0070](0070-render-local-probe-glass-as-opaque-surfaces.md) | Render local-probe glass as opaque surfaces | Accepted |

| [0071](0071-fit-cascades-and-pack-point-shadow-faces.md) | Fit cascades and pack point-shadow faces into shared tile runs | Accepted |
| [0072](0072-integrate-rectangular-area-lights-with-shadowed-quadrature.md) | Integrate rectangular area lights with shadowed quadrature | Accepted |

| [0073](0073-project-spotlight-cookies-through-the-frame-group.md) | Project spotlight cookies through the frame group | Accepted |
| [0074](0074-rebase-each-frame-onto-an-f64-floating-origin.md) | Rebase each frame onto an f64 floating origin | Accepted, amends 0008 and 0050 |
| [0075](0075-webgl2-and-gl-are-a-fallback-profile.md) | WebGL2 and GL are a fallback profile; compute stays | Accepted, amends 0010, 0021 and 0039 |
| [0076](0076-bin-ranged-lights-into-a-host-built-light-grid.md) | Bin ranged lights into a host-built light grid | Accepted |
| [0077](0077-gate-a-release-on-every-shipped-shader-through-the-c-abi.md) | Gate a release on every shipped shader, compiled through the C ABI | Accepted |
| [0078](0078-three-api-tiers-pinned-by-public-api-snapshots.md) | Three API tiers, pinned by public-API snapshots | Accepted |
| [0079](0079-scene-document-v2-textures-bindings-nodes-environment.md) | Scene document v2: textures, bindings, a node hierarchy and an optional environment | Accepted |
| [0080](0080-gltf-import-produces-a-scene-document-with-generated-graphs.md) | glTF import produces a scene document with generated graphs, in a device-free crate | Accepted |
| [0081](0081-commands-are-the-editors-single-mutation-path.md) | Commands are the editor's single mutation path, undone by snapshot | Accepted |
| [0082](0082-the-editors-scene-mode-and-scene-resources-in-the-renderer-crate.md) | The editor's scene mode, with `SceneResources` in the renderer crate | Accepted |
| [0083](0083-an-mcp-server-over-the-editors-commands.md) | An MCP server over the editor's commands, hand-written on `std` | Accepted |
| [0084](0084-wxsl-an-agent-can-write.md) | A generated node reference, a checked authoring guide, and shaders refused instead of panicking | Accepted |
| [0085](0085-scene-debugging-views-a-generic-probe-and-the-frame-panel.md) | Scene debugging: debug views on both paths, frame views as editor passes, a generic probe | Accepted |
| [0086](0086-large-scenes-instanced-runs-per-view-culling-shared-materials.md) | Large scenes: instanced runs, per-view culling by draw bounds, shared materials | Accepted |
| [0087](0087-one-name-per-type-across-frame-and-render.md) | One name per type: the device-free originals take their own names | Accepted |
| [0088](0088-one-shared-model-channel-and-the-id-in-the-normal-target.md) | One model channel shared by every model, the id in the normal target | Accepted |
| [0089](0089-the-surface-output-hands-its-shaded-colour-to-a-fragment-output.md) | The surface output hands its shaded colour to a fragment output | Accepted |
| [0090](0090-a-double-sided-material-turns-off-its-passes-culling.md) | A double-sided material turns off its passes' culling | Accepted |
| [0091](0091-the-gltf-clear-coat-is-a-model-of-its-own-authored-on-the-surface.md) | The glTF clear coat is a model of its own, authored on the surface | Accepted |
| [0092](0092-precomputed-radiance-transfer-is-baked-offline-and-applied-as-environment-occlusion.md) | Precomputed radiance transfer is baked offline and applied as environment occlusion | Accepted |
| [0093](0093-thin-walled-glass-and-the-transmission-factor-are-optical-layer-data.md) | Thin-walled glass and the transmission factor are optical-layer data | Accepted |
| [0094](0094-a-normal-maps-mips-keep-their-spread-and-widen-the-roughness.md) | A normal map's mips keep their spread, and it widens the roughness | Accepted |
| [0095](0095-unshadowed-area-lights-sample-where-the-lobe-and-the-falloff-are.md) | Unshadowed area lights sample where the lobe and the falloff are | Accepted |
| [0096](0096-area-lights-are-integrated-with-ltc-by-the-models-that-can.md) | Area lights are integrated with LTC by the models that can | Accepted |
| [0097](0097-ies-profiles-are-baked-into-a-cookie-layer.md) | IES profiles are baked into a cookie layer | Accepted |
| [0098](0098-contact-hardening-shadows-search-blockers-through-the-comparison-sampler.md) | Contact-hardening shadows search blockers through the comparison sampler | Accepted |

To add one: copy `template.md` to `NNNN-short-title.md` (next number), fill
it in, add a row here. See `AGENTS.md` at the repo root for when an ADR is
warranted.
