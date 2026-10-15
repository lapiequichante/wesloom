//! Turns a validated graph into WXSL source, ready for the WXSL compiler to
//! resolve and lower to WGSL.
//!
//! The emitted module has five parts:
//!
//! 1. **Imports** — the shader ABI ([`crate::abi`]) plus whatever each node's
//!    definition asks for. Node implementations are *imported*, never pasted:
//!    that is the whole reason graphs compile to WXSL rather than to WGSL
//!    ([ADR 0003](../../../docs/adr/0003-wesl-as-the-shading-language.md)).
//! 2. **Macro declarations** — every macro variable in effect, declared
//!    `@macro const` at its effective value, so the module is
//!    self-contained (ADR 0011). Flag macros are not
//!    imported: they are bound as WXSL conditional-translation features by
//!    the caller ([`GeneratedShader::macros`]).
//! 3. **Declarations** — the material's own bind group, the block it
//!    expects from the application, and what it requires of the geometry:
//!    a uniform struct whose fields and offsets `wxsl-core` computed
//!    ([`crate::resources::MaterialInterface`]), one `var` per declared
//!    texture and sampler, and — when the graph declares per-instance
//!    attributes — a *widened* view of the frame's instance row. A graph
//!    does not only compute; it says what must be bound before it can run
//!    ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md))
//!    and what the geometry must carry
//!    ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
//! 4. **The material function** — one `let` per node output, in dependency
//!    order, ending in the [`crate::abi::SURFACE_STRUCT`] the graph produces.
//! 5. **Entry points** — a vertex entry, shared by every stage, and the one
//!    fragment entry [`CodegenOptions::stage`] calls for: a colour, a
//!    G-buffer, or none at all. One module *per stage* is what
//!    [ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)
//!    asks for — the graph author writes no stage-specific nodes. A
//!    material declaring no attributes emits exactly the two lines it
//!    always did; one that declares some emits wider IO structs beside
//!    the ABI's, never instead of them.
//!
//! Only the nodes the output node actually depends on are emitted, so a
//! half-finished branch parked on the editor canvas costs nothing — and
//! the interface is computed over that same reachable set, so a parked
//! `param.value` declares no uniform either.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::abi;
use crate::error::{CodegenError, GraphError, GraphErrors};
use crate::graph::{Graph, GraphOutputs, NodeId, ShaderStage, SocketRef};
use crate::macros::{MacroSet, MacroValue};
use crate::node::{self, FunctionReturn, NodeBody, NodeDefinition, NodeRegistry, Value, ValueType};
use crate::resources::{GeometryInterface, MaterialInterface};
use crate::wxsl::{stable_hash, ModulePath, WxslIdent};

mod emit;
#[cfg(test)]
mod tests;
mod write;

use write::*;

/// Module path the generated material module is mounted at.
///
/// `wxsl-render` adds the generated source to its resolver under this
/// path and compiles it as the root module.
pub const MATERIAL_MODULE: &str = "package::material";

/// Knobs for [`generate`]. The defaults match [`crate::abi`], and a caller
/// that changes them is responsible for the renderer agreeing.
#[derive(Clone, Debug, PartialEq)]
pub struct CodegenOptions {
    /// Name of the generated material function.
    pub material_fn: String,
    /// Name of the generated vertex entry point.
    pub vertex_entry: String,
    /// Which stage to emit entry points for.
    ///
    /// One module per stage, rather than one module with every stage's
    /// entry points `@if`-gated inside it: a stage will soon need only
    /// *part* of the graph (M5's partitioning), and a module that is
    /// already per-stage has somewhere to put that. It also means the
    /// stage is in the source, so the variant cache's source hash
    /// distinguishes stages before its `stage` field even looks
    /// ([ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
    pub stage: abi::MaterialStage,
    /// Whether to emit entry points at all. Turning this off yields a module
    /// with just the material function, useful for testing codegen and for
    /// importing a graph's material into hand-written WXSL.
    pub emit_entry_points: bool,
    /// Macro values to sit *beneath* the graph's own: the graph's node
    /// declarations and pins override these.
    ///
    /// This is how the macros the ABI honours ([`abi::abi_macros`]) get their
    /// defaults into a shader. They are not declared by any node — they
    /// switch behaviour inside the hand-written ABI modules — so without this
    /// they would compile as "unspecified", i.e. silently off.
    pub base_macros: MacroSet,
    /// The material's configuration, resolved against the lighting set the
    /// surrounding pipeline enables
    /// ([ADR 0038](../../../docs/adr/0038-a-materials-configuration-is-one-value.md)).
    ///
    /// Two of its fields are what codegen reads. Its `macros` sit *above*
    /// the graph's own, overriding what it pins — for values the
    /// application decides rather than the graph author: a runtime toggle,
    /// a quality setting, a debug view. Keeping them here rather than
    /// writing them into the graph means the graph still holds what it was
    /// authored with. Its `lighting` decides whether the G-buffer stage
    /// writes a dispatch id and which targets the G-buffer struct carries,
    /// and which shading function the generated code calls.
    ///
    /// The other two — the cast-shadow flag and the tags — ride along
    /// because a material's configuration is *one* value: they are a
    /// selection the renderer makes, not code, and codegen ignores them.
    /// The default is the library's default model in a set of one, which
    /// needs neither.
    pub material: crate::material::ResolvedMaterialConfig,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        CodegenOptions {
            material_fn: abi::MATERIAL_FN.to_string(),
            vertex_entry: abi::VERTEX_ENTRY.to_string(),
            stage: abi::MaterialStage::default(),
            emit_entry_points: true,
            base_macros: abi::abi_macros()
                .into_iter()
                .map(|decl| (decl.name.as_str().to_string(), decl.default))
                .collect(),
            material: crate::material::ResolvedMaterialConfig::default(),
        }
    }
}

/// WXSL source generated from a graph, plus everything needed to compile it.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedShader {
    /// The WXSL source of the root module.
    pub source: String,
    /// The macro values this source was generated with.
    ///
    /// The flag macros in here must be bound as conditional-translation
    /// features when compiling, and the whole set is part of the variant
    /// cache key: numeric macros are baked into [`Self::source`], flags are
    /// not, so source alone does not identify the compiled shader.
    pub macros: MacroSet,
    /// Name of the material function in [`Self::source`].
    pub material_fn: String,
    /// Name of the fragment entry point in [`Self::source`], or `None`
    /// when this module has none.
    ///
    /// Per *material*, not per stage: a depth or shadow stage emits one
    /// only for a material that discards, and a pipeline built for one
    /// that does not has no fragment state at all
    /// ([ADR 0025](../../../docs/adr/0025-a-material-graph-spans-shader-stages.md)).
    pub fragment_entry: Option<String>,
    /// What must be bound before this module can run: the material's own
    /// uniform parameters, its textures and samplers, and the block it
    /// expects the application to supply.
    ///
    /// Emitted into [`Self::source`] *and* handed out here, because the
    /// renderer has to build bind groups matching what was emitted, and
    /// re-deriving them from the graph a second time is exactly how the
    /// two halves would come to disagree.
    pub interface: MaterialInterface,
    /// Stable hash of the source. See [`GeneratedShader::variant_key`].
    pub source_hash: u64,
}

impl GeneratedShader {
    /// A stable identity for "this source compiled with these macro values".
    ///
    /// The source alone does not identify the compiled shader: the bindings
    /// also reach *imported* modules, whose own macro defaults do not show
    /// up in the
    /// root module. `wxsl-render` combines this with the render path to key
    /// its variant cache.
    pub fn variant_key(&self) -> u64 {
        let mut text = self.source_hash.to_string();
        let _ = write!(text, ";{}", self.macros.signature());
        stable_hash(text.as_bytes())
    }
}

/// Generate WXSL for `graph`.
///
/// The graph is validated first: emitting WXSL from an invalid graph would
/// only move the error into the shader compiler, where it is far harder to
/// explain.
pub fn generate(
    graph: &Graph,
    registry: &NodeRegistry,
    options: &CodegenOptions,
) -> Result<GeneratedShader, CodegenError> {
    graph.validate(registry)?;

    let found = graph.surface_outputs(registry);
    let outputs = match found.len() {
        0 => return Err(CodegenError::NoOutputNode),
        1 => graph.outputs(registry).expect("exactly one surface output"),
        _ => return Err(CodegenError::MultipleOutputNodes(found)),
    };

    // Precedence, weakest first: the caller's defaults, each node's declared
    // default, what the graph pins, the caller's overrides.
    let mut macros = options.base_macros.clone();
    // A feature channel in the plan brings its macro into the same
    // precedence chain: the generated pack's `@if` names it, a WXSL module
    // declares the knobs it uses, and the graph or the caller pins the
    // value that turns the feature on (plan2 P12).
    for feature in crate::lighting::FEATURES {
        if options
            .material
            .lighting
            .features()
            .iter()
            .any(|request| request.source.name() == feature.name)
        {
            macros.set(feature.macro_name, crate::macros::MacroValue::Flag(false));
        }
    }
    macros.overlay(&graph.effective_macros(registry)?);
    macros.overlay(&options.material.macros);

    // Where every node runs, and where the stages hand values over
    // ([`crate::stages`], plan2 P9). An empty plan — no node shared
    // across the stage boundary — generates exactly what per-terminal
    // compilation always did.
    let plan = crate::stages::analyze(graph, registry, &outputs).map_err(CodegenError::Invalid)?;

    // Over the *reachable* set, not the whole graph: a parked branch
    // declares no uniform, exactly as it emits no code. Over all
    // terminals, not one stage's: see `Graph::reachable_from`. The cut
    // subtrees are reachable too — the vertex stage compiles them — and
    // what they read must be bound exactly as if a hand-wired
    // interpolant's subgraph had declared it, because that is what a cut
    // is.
    let mut reachable = graph.reachable_from(&outputs);
    for socket in plan.cuts.keys() {
        reachable.extend(graph.dependencies_of(socket.node));
    }
    let mut interface = graph.interface_of(registry, &reachable);
    // The synthesized interpolants join the declared ones in the same
    // accountant; the analysis already spent within its budget, and this
    // is the same arithmetic keeping the plan and the struct honest.
    for (socket, (name, ty)) in &plan.cuts {
        assert!(
            interface.geometry.add_computed(name.clone(), *ty),
            "stage analysis cut `{socket}` but the inter-stage budget is spent"
        );
    }
    let stage = options.stage;
    // The bake plan, over the reachable set: each declaration whose node
    // the surface (or a discard test) actually reaches becomes a stand-in,
    // and its cone leaves the module — unless the material evaluates its
    // bakes inline, which is the toggle and not an edit.
    let baked = if options.material.bakes {
        bake_terms(graph, registry, &reachable, &outputs, &plan)?
    } else {
        BTreeMap::new()
    };
    let mut emitter = Emitter {
        graph,
        registry,
        options,
        interface,
        stage: ShaderStage::Fragment,
        baked,
        bake_domain: false,
        bindings: BTreeMap::new(),
        imports: BTreeMap::new(),
        lighting_source: String::new(),
        body: String::new(),
    };

    // One partition per terminal, each compiled from its own roots and
    // into its own function. A node feeding two of them is emitted twice
    // — in different functions, so the `let` names cannot collide, and a
    // shader compiler's common-subexpression pass removes the duplicate
    // work where it can (ADR 0025).
    let vertex = match outputs.vertex {
        Some(node) => Some(emitter.emit_partition(node, ShaderStage::Vertex)?),
        None => None,
    };
    let discard = match outputs.discard {
        Some(node) => Some(emitter.emit_fragment_partition(node, &plan.cuts)?),
        None => None,
    };
    // One partition per computed interpolant, each a vertex-stage root of
    // its own: two interpolants from unrelated subgraphs cost only what
    // each of them reads.
    //
    // And none at all in a stage with no fragment program: an interpolant
    // exists to be read per fragment, so a plain depth prepass computes
    // nothing for it and leaves its location zeroed. The *struct* keeps
    // the location either way — the interface is per material — which is
    // what makes this a saving in the vertex stage rather than a
    // different pipeline layout.
    let has_fragment = stage.always_has_fragment() || outputs.discard.is_some();
    let mut varyings = Vec::with_capacity(outputs.varyings.len() + plan.cuts.len());
    if has_fragment {
        for (name, node) in &outputs.varyings {
            let part = emitter.emit_partition(*node, ShaderStage::Vertex)?;
            varyings.push((name.clone(), part));
        }
        // The stage cuts, each a vertex-stage partition of its own —
        // exactly what a hand-wired interpolant is, minus the hand
        // wiring. The fragment side reads them as `attrs.<name>`, which
        // is why the fragment partitions below stop at cut nodes.
        for (socket, (name, _)) in &plan.cuts {
            let part = emitter.emit_cut(socket)?;
            varyings.push((name.clone(), part));
        }
    }
    // The one partition a stage may not need at all: a depth or shadow
    // pass wants the vertex offset and the alpha test, and nothing else.
    let surface = stage
        .needs_surface()
        .then(|| emitter.emit_fragment_partition(outputs.surface, &plan.cuts))
        .transpose()?;

    emitter.request_abi_imports(vertex.is_some(), surface.is_some(), !varyings.is_empty());
    let parts = Partitions {
        vertex,
        surface,
        discard,
        varyings,
    };
    // A stage with nothing to write has a fragment entry only when the
    // material discards; otherwise it has no fragment state at all,
    // which is what makes a depth prepass cheap.
    let fragment_entry = has_fragment.then(|| stage.fragment_entry().to_string());

    let interface = emitter.interface.clone();
    // The velocity stage interpolates two more values than the graph's
    // location budget accounted for — both frames' clip positions, which
    // ride after the material's own extras. A material that spent every
    // location but one has none left for the pair, and that is a fact
    // about *this stage*, which no graph-level check could have seen.
    if stage.output() == abi::StageOutput::Velocity
        && !interface.geometry.is_empty()
        && previous_clip_location(&interface.geometry) + 1 >= abi::MAX_VARYING_LOCATIONS as u32
    {
        let output = outputs.surface;
        return Err(CodegenError::Invalid(GraphErrors(vec![
            GraphError::WrongStage {
                node: output,
                def: graph
                    .node(output)
                    .map(|n| n.def.clone())
                    .unwrap_or_default(),
                output,
                reason: format!(
                    "the velocity stage needs two more inter-stage locations than \
                 this material's geometry leaves ({} of {} spent), and the \
                 two clip positions have nowhere to ride",
                    previous_clip_location(&interface.geometry),
                    abi::MAX_VARYING_LOCATIONS,
                ),
            },
        ])));
    }
    let source = emitter.finish(graph, &macros, &parts);
    let source_hash = stable_hash(source.as_bytes());
    Ok(GeneratedShader {
        source,
        macros,
        material_fn: options.material_fn.clone(),
        fragment_entry,
        interface,
        source_hash,
    })
}

/// Module path a generated *screen* module is mounted at.
///
/// The screen domain's twin of [`MATERIAL_MODULE`]: `wxsl-render` adds the
/// generated source to its resolver under this path and compiles it as the
/// root module of one effect
/// ([ADR 0040](../../../docs/adr/0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md)).
pub const SCREEN_MODULE: &str = "package::screen";

/// Knobs for [`generate_screen`].
///
/// Much smaller than [`CodegenOptions`], and that is the point: a screen
/// graph has one stage, one entry-point pair, no lighting model, no
/// G-buffer and no configuration a material's would recognise. What it
/// shares is the macro precedence chain, because that belongs to graphs
/// rather than to materials.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScreenOptions {
    /// Macro values to sit *beneath* the graph's own, as
    /// [`CodegenOptions::base_macros`] does.
    pub base_macros: MacroSet,
    /// Macro values to sit *above* the graph's own — what the application
    /// decides rather than the effect's author.
    pub macros: MacroSet,
}

/// Generate WXSL for a screen graph: one fullscreen pass over one image.
///
/// The screen half of [`generate`], and deliberately a separate function
/// rather than a branch inside it. A material's generation is a
/// stage-by-stage affair — a vertex partition, computed interpolants, a
/// discard test, a surface, and a lighting model wrapped around the lot —
/// and none of that exists here: an effect is one fragment function and the
/// triangle that runs it. The parts they *do* share — validation, node
/// emission, the macro chain, the module header — are shared as code, which
/// is what keeps the two domains one vocabulary.
pub fn generate_screen(
    graph: &Graph,
    registry: &NodeRegistry,
    options: &ScreenOptions,
) -> Result<GeneratedShader, CodegenError> {
    if graph.domain() != node::GraphDomain::Screen {
        return Err(CodegenError::WrongDomain {
            expected: node::GraphDomain::Screen,
            found: graph.domain(),
        });
    }
    graph.validate(registry)?;

    let found = graph.screen_outputs(registry);
    let terminal = match found.as_slice() {
        [] => return Err(CodegenError::NoOutputNode),
        [only] => *only,
        _ => return Err(CodegenError::MultipleOutputNodes(found)),
    };

    // The same precedence chain a material's macros go through, weakest
    // first: the caller's defaults, each node's declared default, what the
    // graph pins, the caller's overrides. No feature channels — those are a
    // G-buffer's business, and an effect has none.
    let mut macros = options.base_macros.clone();
    macros.overlay(&graph.effective_macros(registry)?);
    macros.overlay(&options.macros);

    // A screen graph declares nothing bindable: `param.value`,
    // `texture.texture_2d`, `input.user` and `input.attribute` are all
    // surface-domain nodes, so validation has already refused them by the
    // time we get here. The interface is therefore empty by construction
    // rather than by a check of its own — and it is still *computed*,
    // because that is the assertion.
    let reachable = graph.dependencies_of(terminal);
    let interface = graph.interface_of(registry, &reachable);
    debug_assert!(
        interface.material_group_is_empty()
            && interface.user.is_none()
            && interface.geometry.is_empty(),
        "a screen graph cannot reach a declaring node: they are surface-domain"
    );

    let material_options = CodegenOptions {
        emit_entry_points: false,
        ..CodegenOptions::default()
    };
    let mut emitter = Emitter {
        graph,
        registry,
        options: &material_options,
        interface: interface.clone(),
        stage: ShaderStage::Fragment,
        baked: BTreeMap::new(),
        bake_domain: false,
        bindings: BTreeMap::new(),
        imports: BTreeMap::new(),
        lighting_source: String::new(),
        body: String::new(),
    };
    emitter.request_import(abi::SCREEN_MODULE, abi::SCREEN_CONTEXT_STRUCT);
    emitter.request_import(abi::SCREEN_MODULE, abi::SCREEN_CONTEXT_FN);
    let part = emitter.emit_partition(terminal, ShaderStage::Fragment)?;
    let source = screen_module(graph, &macros, &emitter.imports, &part);

    let source_hash = stable_hash(source.as_bytes());
    Ok(GeneratedShader {
        source,
        macros,
        material_fn: abi::SCREEN_FN.to_string(),
        fragment_entry: Some(abi::SCREEN_FRAGMENT_ENTRY.to_string()),
        interface,
        source_hash,
    })
}

/// Module path a generated *bake* module is mounted at — the shader an
/// effect generated from a material subgraph compiles as. Nominal, like
/// the other mounts: nothing imports a root module.
pub use crate::abi::BAKE_MODULE;

/// Knobs for [`generate_bake`]: the same macro chain a screen graph goes
/// through, because the cone's nodes declare macros the same way.
///
/// One rule the caller owns: the bake module must be generated at the same
/// macro values the *material* compiles under when it evaluates the cone
/// inline — an octave count pinned differently on one side is a bake that
/// disagrees with its own subgraph. Pin the shared knobs on the graph, and
/// both sides read them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BakeOptions {
    /// Macro values to sit *beneath* the graph's own.
    pub base_macros: MacroSet,
    /// Macro values to sit *above* the graph's own — what the application
    /// decides rather than the graph's author.
    pub macros: MacroSet,
}

/// What generating a bake produces: the module, and everything the effect
/// descriptor and the pass that runs it need to know about the dispatch.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedBake {
    /// The WXSL source of the root module.
    pub source: String,
    /// The fragment arm (ADR 0075): the same header and value function,
    /// entered from [`abi::BAKE_VERTEX_ENTRY`] and
    /// [`abi::BAKE_FRAGMENT_ENTRY`] and writing the table as a colour
    /// attachment, for a device without compute.
    pub fragment_source: String,
    /// The macro values this source was generated at — the flag macros go
    /// to the compiler as conditional-translation bindings, as for any
    /// generated module.
    pub macros: MacroSet,
    /// The compute entry point in [`Self::source`].
    pub entry: &'static str,
    /// Workgroups in x, y, z: the bake table's extent, rounded up to whole
    /// [`abi::BAKE_WORKGROUP_SIZE`] squares.
    pub workgroups: [u32; 3],
    /// The bake table's size, in pixels — what the target must be.
    pub size: [u32; 2],
    /// The name the table's write-only storage binding goes by, and the
    /// one output the effect declares.
    pub target: &'static str,
    /// Stable hash of the source, for the variant cache.
    pub source_hash: u64,
}

/// Generate WXSL for one bake: a material subgraph, evaluated per texel of
/// a small 2D table and written to it
/// ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
///
/// The material-side twin of this — the subgraph skipped and one sample
/// emitted instead — is [`generate`] with the configuration's `bakes` on;
/// both are generated from the same declaration, which is why the two arms
/// can agree.
///
/// The cone's purity is checked here, at generation, so a bake that cannot
/// be computed is a failure at registration rather than at the first frame
/// that wanted it — the same rule `Effect::from_graph` keeps. The cone is a
/// function of the bake domain alone: `input.uv` is allowed (it *is* the
/// domain, and becomes the function's parameter); any other context read,
/// any attribute, parameter, application field or texture is refused by
/// name. A bake is a pure function of where it is evaluated — that is the
/// whole of what makes sampling it back equal computing it.
pub fn generate_bake(
    graph: &Graph,
    decl: &crate::graph::BakeDecl,
    registry: &NodeRegistry,
    options: &BakeOptions,
) -> Result<GeneratedBake, CodegenError> {
    graph.validate(registry)?;
    let invalid = |reason: String| {
        CodegenError::Invalid(GraphErrors(vec![GraphError::InvalidBake {
            texture: decl.texture.trim().to_string(),
            reason,
        }]))
    };

    // Everything the baked node's output depends on — the node itself
    // included: its own body computes the value, whatever feeds it computes
    // the inputs.
    let cone = graph.dependencies_of(decl.node);
    let order = graph
        .topological_order(Some(&cone))
        .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;

    // Purity: the cone is a function of the domain and nothing else. One
    // refusal per offending node, naming what it reads.
    for &member in &order {
        let Some(instance) = graph.node(member) else {
            continue;
        };
        let Some(def) = registry.get(&instance.def) else {
            continue;
        };
        let what = match &def.body {
            NodeBody::ContextRead(field) if field.as_str() == "uv" => continue,
            NodeBody::ContextRead(field) => format!("the surface context field `{field}`"),
            NodeBody::VertexContextRead(field) => format!("the vertex context field `{field}`"),
            NodeBody::AttributeRead => "a geometry attribute".to_string(),
            NodeBody::Param => "a material parameter".to_string(),
            NodeBody::UserRead => "the application block".to_string(),
            NodeBody::Resource => "a bound texture".to_string(),
            _ if def.fragment_only => {
                return Err(invalid(format!(
                    "the baked subgraph computes a screen-space derivative at \
                     node {member}; a bake is a compute pass over the bake \
                     domain, and no derivative exists there (plan5 D1)"
                )))
            }
            _ => continue,
        };
        return Err(invalid(format!(
            "the baked subgraph reads {what} at node {member}, and a bake \
             evaluates over the bake domain (`input.uv`) alone — a bake is a \
             pure function of where it is sampled"
        )));
    }

    // The same macro chain a screen graph goes through.
    let mut macros = options.base_macros.clone();
    macros.overlay(&graph.effective_macros(registry)?);
    macros.overlay(&options.macros);

    // Which output socket, and therefore which expression the table
    // stores.
    let instance = graph
        .node(decl.node)
        .ok_or_else(|| invalid("names a node the graph does not contain".to_string()))?;
    let def = registry
        .get(&instance.def)
        .ok_or_else(|| invalid("names a node whose definition is not registered".to_string()))?;
    let socket_name = decl
        .socket
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            def.outputs
                .first()
                .map(|socket| socket.name.as_str().to_string())
                .expect("check_bakes rejected a node with no outputs")
        });
    let socket = def
        .outputs
        .iter()
        .find(|socket| socket.name.as_str() == socket_name)
        .ok_or_else(|| {
            invalid(format!(
                "node {} has no output socket `{socket_name}` to bake",
                decl.node
            ))
        })?;
    let value_ty = socket.ty;

    // Emit the cone into the value function, under `bake_domain`, so the
    // uv reads come out as the parameter.
    let material_options = CodegenOptions {
        emit_entry_points: false,
        ..CodegenOptions::default()
    };
    let mut emitter = Emitter {
        graph,
        registry,
        options: &material_options,
        interface: MaterialInterface::default(),
        stage: ShaderStage::Fragment,
        baked: BTreeMap::new(),
        bake_domain: true,
        bindings: BTreeMap::new(),
        imports: BTreeMap::new(),
        lighting_source: String::new(),
        body: String::new(),
    };
    for node in &order {
        emitter.emit_node(*node)?;
    }
    let value = emitter
        .bindings
        .get(&SocketRef::new(decl.node, &socket_name))
        .cloned()
        .ok_or_else(|| invalid("its own value never computed".to_string()))?;
    let cone_body = core::mem::take(&mut emitter.body);
    let imports = emitter.imports;

    let size = decl.size;
    let workgroups = [
        size[0].div_ceil(abi::BAKE_WORKGROUP_SIZE),
        size[1].div_ceil(abi::BAKE_WORKGROUP_SIZE),
        1,
    ];
    let (source, fragment_source) = bake_module(
        graph,
        decl,
        &macros,
        &imports,
        BakedValue {
            body: &cone_body,
            expr: &value,
            ty: value_ty,
            size,
        },
    );
    let source_hash = stable_hash(source.as_bytes());
    Ok(GeneratedBake {
        source,
        fragment_source,
        macros,
        entry: abi::BAKE_ENTRY,
        workgroups,
        size,
        target: abi::BAKE_TARGET_VAR,
        source_hash,
    })
}

/// The value a bake stores: the cone's emitted statements, the expression
/// that is the baked node's output, its type, and the table it lands in.
struct BakedValue<'a> {
    body: &'a str,
    expr: &'a str,
    ty: ValueType,
    size: [u32; 2],
}

/// The whole generated bake module: header, imports, macros, the write-only
/// target, the value function over the bake domain, and the dispatch that
/// fills the table — and beside it the fragment arm, which shares all but
/// the target and the entry points (ADR 0075).
fn bake_module(
    graph: &Graph,
    decl: &crate::graph::BakeDecl,
    macros: &MacroSet,
    imports: &BTreeMap<ModulePath, Vec<WxslIdent>>,
    value: BakedValue<'_>,
) -> (String, String) {
    let BakedValue {
        body: cone_body,
        expr: value,
        ty: value_ty,
        size,
    } = value;
    // The stored value, padded out to the table's four channels. The
    // padding is constant, and the reading side's swizzle takes exactly
    // the value's width back.
    let padded = match value_ty {
        ValueType::F32 => format!("vec4f({value}, 0.0, 0.0, 1.0)"),
        ValueType::Vec2 => format!("vec4f({value}, 0.0, 1.0)"),
        ValueType::Vec3 => format!("vec4f({value}, 1.0)"),
        ValueType::Vec4 => value.to_string(),
        _ => unreachable!("check_bakes rejected a non-float bake"),
    };
    // The two precisions a bake may declare are the two that can be
    // written through storage (the check is `check_bakes`'s); this is
    // their WGSL.
    let format = match decl.precision {
        crate::abi::GBufferPrecision::Normalized => "rgba8unorm",
        _ => "rgba16float",
    };
    let (width, height) = (size[0], size[1]);

    let mut shared = String::with_capacity(1024);
    write_header(&mut shared, graph, macros, imports);
    let _ = writeln!(
        shared,
        "// Bake `{texture}`: {width}x{height}, {format}.",
        texture = decl.texture.trim(),
    );
    let _ = writeln!(shared, "\nfn {}(uv: vec2f) -> vec4f {{", abi::BAKE_VALUE_FN,);
    // The cone's statements, already emitted in dependency order.
    shared.push_str(cone_body);
    let _ = writeln!(shared, "    return {padded};");
    shared.push_str("}\n");

    let mut fragment = shared.clone();
    // A fragment's position is its texel's centre, so `uv` is the compute
    // arm's to the bit.
    let _ = write!(
        fragment,
        "
@vertex
fn {}(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {{
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index >> 1u) * 4 - 1);
    return vec4f(x, y, 0.0, 1.0);
}}

@fragment
fn {}(@builtin(position) position: vec4f) -> @location(0) vec4f {{
    return {}(position.xy / vec2f({width}.0, {height}.0));
}}
",
        abi::BAKE_VERTEX_ENTRY,
        abi::BAKE_FRAGMENT_ENTRY,
        abi::BAKE_VALUE_FN,
    );

    let mut out = shared;
    let _ = writeln!(
        out,
        "\n@group({}) @binding(0) var {}: texture_storage_2d<{format}, write>;",
        abi::GROUP_PASS,
        abi::BAKE_TARGET_VAR,
    );
    let _ = write!(
        out,
        "
@compute @workgroup_size({}, {})
fn {}(@builtin(global_invocation_id) id: vec3u) {{
    if (id.x >= {width}u || id.y >= {height}u) {{
        return;
    }}
    let uv = (vec2f(id.xy) + vec2f(0.5, 0.5)) / vec2f({width}.0, {height}.0);
    textureStore({}, vec2i(id.xy), {}(uv));
}}
",
        abi::BAKE_WORKGROUP_SIZE,
        abi::BAKE_WORKGROUP_SIZE,
        abi::BAKE_ENTRY,
        abi::BAKE_TARGET_VAR,
        abi::BAKE_VALUE_FN,
    );
    (out, fragment)
}

/// The whole generated screen module: header, imports, macros, the graph's
/// function, and the two entry points that run it.
fn screen_module(
    graph: &Graph,
    macros: &MacroSet,
    imports: &BTreeMap<ModulePath, Vec<WxslIdent>>,
    part: &Partition,
) -> String {
    let mut out = String::with_capacity(1024);
    write_header(&mut out, graph, macros, imports);

    let _ = write!(
        out,
        "\nfn {}({}: {}) -> vec4f {{\n",
        abi::SCREEN_FN,
        abi::CONTEXT_VAR,
        abi::SCREEN_CONTEXT_STRUCT,
    );
    out.push_str(&part.body);
    let _ = writeln!(
        out,
        "    return vec4f({}, {});",
        part.input(abi::SOCKET_SCREEN_COLOR, "vec3f(0.0, 0.0, 0.0)"),
        part.input(abi::SOCKET_SCREEN_ALPHA, "1.0"),
    );
    out.push_str("}\n");

    // The fullscreen triangle, written here rather than imported: an entry
    // point belongs to the module that declares it, so every screen effect
    // in this repo — the shipped `.wxsl` ones included — carries its own
    // copy of these three lines. Three vertices, no vertex buffer:
    // (-1,-1), (3,-1), (-1,3).
    let _ = write!(
        out,
        "
@vertex
fn {}(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {{
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index >> 1u) * 4 - 1);
    return vec4f(x, y, 0.0, 1.0);
}}

@fragment
fn {}(@builtin(position) position: vec4f) -> @location(0) vec4f {{
    return {}({}(position));
}}
",
        abi::SCREEN_VERTEX_ENTRY,
        abi::SCREEN_FRAGMENT_ENTRY,
        abi::SCREEN_FN,
        abi::SCREEN_CONTEXT_FN,
    );
    out
}

/// One compiled partition: the statements leading up to a terminal, and
/// what that terminal's inputs came out as.
struct Partition {
    /// `let` statements, in dependency order.
    body: String,
    /// The terminal's fed inputs, as `(socket, expression)`.
    inputs: Vec<(String, String)>,
}

impl Partition {
    /// The expression feeding `socket`, or `fallback` when nothing does.
    fn input<'a>(&'a self, socket: &str, fallback: &'a str) -> &'a str {
        self.inputs
            .iter()
            .find(|(name, _)| name == socket)
            .map(|(_, expr)| expr.as_str())
            .unwrap_or(fallback)
    }
}

/// The partitions one stage's module is built from.
struct Partitions {
    /// One per computed interpolant, in the interface's location order.
    varyings: Vec<(WxslIdent, Partition)>,
    /// The vertex-stage offset, when the graph has a vertex output.
    vertex: Option<Partition>,
    /// The surface, when this stage writes one.
    surface: Option<Partition>,
    /// The discard test, when the graph has a discard output.
    discard: Option<Partition>,
}

struct Emitter<'a> {
    graph: &'a Graph,
    registry: &'a NodeRegistry,
    options: &'a CodegenOptions,
    /// What the module declares it needs bound, computed once from the
    /// reachable set and then both *emitted* and handed back.
    interface: MaterialInterface,
    /// Which shader stage the partition being emitted compiles into.
    ///
    /// Only two things read it — a context read and an attribute read —
    /// because those are the only expressions that are spelled
    /// differently on the two sides of the interpolator.
    stage: ShaderStage,
    /// The bakes this module consumes: each is a node the partitions stop
    /// at, binding its output socket to one sample of its table instead
    /// of to the subgraph that feeds it
    /// ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
    /// Empty when the material evaluates its bakes inline.
    baked: BTreeMap<NodeId, BakedTerm>,
    /// Whether this module is a *bake*: the generated value function of
    /// one subgraph over the bake domain, where `input.uv` is the
    /// function's parameter and every other context read is the purity
    /// check's to have refused already.
    bake_domain: bool,
    /// Expression that reads each already-emitted node output. Cleared
    /// between partitions: a node emitted into two of them is two `let`
    /// bindings in two functions.
    bindings: BTreeMap<SocketRef, String>,
    /// Items to import, grouped by module and deduplicated.
    imports: BTreeMap<ModulePath, Vec<WxslIdent>>,
    /// The lighting model's dispatch and shading function, or the G-buffer
    /// struct and pack — whichever this stage's entry point needs. Empty
    /// for a stage that needs neither (`crate::lighting` generates it; the
    /// imports are requested up front so they land in the one list).
    lighting_source: String,
    /// The partition's statements.
    body: String,
}

/// One bake declaration resolved for emission: the names the generated
/// sample reads, and the swizzle from the table's `vec4f` to the value's
/// type.
#[derive(Clone, Debug)]
struct BakedTerm {
    /// The node the bake stands in for.
    node: NodeId,
    /// The output socket the bake replaces.
    socket: String,
    /// The table texture, as the `@group(1)` variable is named.
    texture: String,
    /// The sampler beside it, `{texture}_sampler` by the declaration's
    /// convention.
    sampler: String,
    /// What to read off the sampled `vec4f` — `.x` for a scalar bake, an
    /// empty string for a `vec4f` one.
    swizzle: &'static str,
}

impl BakedTerm {
    /// The expression that stands in for the baked node's output: one
    /// filtered sample of the table at the surface's uv — the same domain
    /// the bake evaluated the subgraph over.
    fn sample_expr(&self) -> String {
        format!(
            "textureSample({}, {}, {}.uv){}",
            self.texture,
            self.sampler,
            abi::CONTEXT_VAR,
            self.swizzle,
        )
    }
}

/// Resolve the graph's bake declarations into what emission needs: one
/// term per declaration whose node is reachable, with the cone checks that
/// make the stand-in sound — the node runs per fragment, and nothing
/// outside the bake reads into the subgraph being replaced.
fn bake_terms(
    graph: &Graph,
    registry: &NodeRegistry,
    reachable: &BTreeSet<NodeId>,
    outputs: &GraphOutputs,
    plan: &crate::stages::StagePlan,
) -> Result<BTreeMap<NodeId, BakedTerm>, CodegenError> {
    let mut terms = BTreeMap::new();
    for decl in graph.bakes() {
        if !reachable.contains(&decl.node) {
            continue;
        }
        let invalid = |reason: String| {
            CodegenError::Invalid(GraphErrors(vec![GraphError::InvalidBake {
                texture: decl.texture.trim().to_string(),
                reason,
            }]))
        };
        // A bake is a per-fragment value — the material samples it, and
        // the vertex stage has no business paying for it. (The purity of
        // the *cone* is `generate_bake`'s check, at registration; these
        // are the things about the node itself.) Either half of the vertex
        // stage reaching it — the displacement or an interpolant — is the
        // same refusal: `textureSample` does not run there, so there is no
        // stand-in to emit.
        if plan.stage_of.get(&decl.node) == Some(&ShaderStage::Vertex) {
            return Err(invalid(
                "the baked node is placed in the vertex stage, and a bake \
                 is a per-fragment sample — pin it `fragment` or bake a node \
                 the fragment stage computes"
                    .to_string(),
            ));
        }
        let vertex_roots = [outputs.vertex]
            .into_iter()
            .flatten()
            .chain(outputs.varyings.iter().map(|(_, node)| *node));
        for root in vertex_roots {
            if graph.dependencies_of(root).contains(&decl.node) {
                return Err(invalid(
                    "the baked node also feeds the vertex stage — a bake is \
                     sampled per fragment, so nothing it stands in for may \
                     be read on the vertex side"
                        .to_string(),
                ));
            }
        }
        // The subgraph being replaced must belong to the bake alone: a
        // node whose value reaches the surface by another road as well
        // would go dark the moment the bake stood in.
        let cone = graph.dependencies_of(decl.node);
        for &member in &cone {
            if member == decl.node {
                continue;
            }
            for edge in graph.edges_from(member) {
                if edge.to.node != decl.node && !cone.contains(&edge.to.node) {
                    return Err(invalid(format!(
                        "the baked subgraph also feeds node {} — a bake \
                         stands in for its cone alone, and that value would \
                         be lost when it is skipped",
                        edge.to.node
                    )));
                }
            }
        }
        let instance = graph.node(decl.node).expect("reachable node exists");
        let def = registry
            .get(&instance.def)
            .expect("a validated graph's definitions are known");
        let socket_name = decl
            .socket
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                def.outputs
                    .first()
                    .map(|socket| socket.name.as_str().to_string())
                    .expect("check_bakes rejected a node with no outputs")
            });
        let socket = def
            .outputs
            .iter()
            .find(|socket| socket.name.as_str() == socket_name)
            .expect("check_bakes rejected an unknown socket");
        let swizzle = match socket.ty {
            ValueType::F32 => ".x",
            ValueType::Vec2 => ".xy",
            ValueType::Vec3 => ".xyz",
            ValueType::Vec4 => "",
            _ => unreachable!("check_bakes rejected a non-float bake"),
        };
        let texture = decl.texture.trim().to_string();
        terms.insert(
            decl.node,
            BakedTerm {
                node: decl.node,
                socket: socket_name,
                sampler: format!("{texture}_sampler"),
                texture,
                swizzle,
            },
        );
    }
    Ok(terms)
}

/// The `let` name binding node `node`'s output `socket`.
fn binding_name(node: NodeId, socket: &str) -> String {
    format!("n{}_{socket}", node.0)
}

/// Parenthesize `expr` unless it already parses as a single atom, so
/// substituting it into a template cannot change how the surrounding
/// expression groups (`-{a}` with `a = -1.0` must not become `--1.0`).
fn as_atom(expr: &str) -> String {
    if is_atomic(expr) {
        expr.to_string()
    } else {
        format!("({expr})")
    }
}

/// Whether `expr` binds tighter than any operator, i.e. needs no parentheses.
///
/// Two shapes qualify: a name, field access or plain number (`n1_out`,
/// `ctx.uv`, `0.5`), and a single call whose argument list closes at the very
/// end (`vec3f(1.0, 0.0, 0.0)`). Anything else — a signed literal, an
/// operator expression — gets wrapped.
fn is_atomic(expr: &str) -> bool {
    if expr.is_empty() {
        return false;
    }
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    if expr.chars().all(|c| is_name_char(c) || c == '.') {
        return true;
    }

    let Some(open) = expr.find('(') else {
        return false;
    };
    let (name, arguments) = expr.split_at(open);
    if name.is_empty() || !name.chars().all(is_name_char) || !arguments.ends_with(')') {
        return false;
    }
    // The first `(` must stay open until the last character, or the
    // expression is a call followed by something else (`f(x) * 2`).
    let mut depth = 0usize;
    for (index, character) in arguments.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && index + character.len_utf8() != arguments.len() {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}
