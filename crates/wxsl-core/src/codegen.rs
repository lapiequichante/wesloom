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
    let source = bake_module(
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
/// fills the table.
fn bake_module(
    graph: &Graph,
    decl: &crate::graph::BakeDecl,
    macros: &MacroSet,
    imports: &BTreeMap<ModulePath, Vec<WxslIdent>>,
    value: BakedValue<'_>,
) -> String {
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

    let mut out = String::with_capacity(1024);
    write_header(&mut out, graph, macros, imports);
    let _ = writeln!(
        out,
        "// Bake `{texture}`: {width}x{height}, {format}.",
        texture = decl.texture.trim(),
    );
    let _ = writeln!(
        out,
        "@group({}) @binding(0) var {}: texture_storage_2d<{format}, write>;",
        abi::GROUP_PASS,
        abi::BAKE_TARGET_VAR,
    );

    let _ = writeln!(out, "\nfn {}(uv: vec2f) -> vec4f {{", abi::BAKE_VALUE_FN,);
    // The cone's statements, already emitted in dependency order.
    out.push_str(cone_body);
    let _ = writeln!(out, "    return {padded};");
    out.push_str("}\n");

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
    out
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

impl Emitter<'_> {
    fn request_import(&mut self, module: &str, item: &str) {
        let module = ModulePath::new(module).expect("ABI module paths are valid");
        let item = WxslIdent::new(item).expect("ABI item names are valid");
        let items = self.imports.entry(module).or_default();
        if !items.contains(&item) {
            items.push(item);
        }
    }

    /// Compile everything `terminal` depends on into one function body.
    fn emit_partition(
        &mut self,
        terminal: NodeId,
        stage: ShaderStage,
    ) -> Result<Partition, CodegenError> {
        let needed = self.graph.dependencies_of(terminal);
        let order = self
            .graph
            .topological_order(Some(&needed))
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;
        self.stage = stage;
        self.bindings.clear();
        self.body.clear();
        for node in &order {
            if *node == terminal {
                continue;
            }
            self.emit_node(*node)?;
        }
        let inputs = self.emit_terminal(terminal)?;
        Ok(Partition {
            body: core::mem::take(&mut self.body),
            inputs,
        })
    }

    /// A fragment-stage partition: like [`Emitter::emit_partition`], but
    /// a node the stage analysis assigned to the vertex stage is a *cut*
    /// — the walk stops there, and its sockets read the synthesized
    /// interpolants the vertex side writes (`attrs.<name>`). This is the
    /// whole mechanism: a cut is an interpolant with no author.
    fn emit_fragment_partition(
        &mut self,
        terminal: NodeId,
        cuts: &std::collections::BTreeMap<
            SocketRef,
            (crate::wxsl::WxslIdent, crate::node::ValueType),
        >,
    ) -> Result<Partition, CodegenError> {
        let mut stops: std::collections::BTreeSet<NodeId> =
            cuts.keys().map(|socket| socket.node).collect();
        stops.extend(self.baked.keys().copied());
        let needed = self.graph.dependencies_stopping_at(terminal, &stops);
        let order = self
            .graph
            .topological_order(Some(&needed))
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;
        self.stage = ShaderStage::Fragment;
        self.bindings.clear();
        self.body.clear();
        // The cuts are leaves as far as this partition is concerned, so
        // their expressions are the interpolant reads, bound before
        // anything that consumes them is emitted.
        for (socket, (name, _)) in cuts {
            self.bindings.insert(
                socket.clone(),
                format!("{}.{}", abi::MATERIAL_ATTRIBUTES_VAR, name),
            );
        }
        // A baked term is a leaf the same way — but its expression is one
        // sample of its table, which reads only the context uv and the
        // table's own bindings, so it can be bound up front too. Its cone
        // is not walked at all: that is the whole point of the bake, and
        // the reason the baked arm's module is smaller by the subgraph.
        for term in self.baked.values() {
            let reference = SocketRef::new(term.node, term.socket.clone());
            if self.graph.edge_from(&reference).is_some() {
                self.bindings.insert(reference, term.sample_expr());
            }
        }
        for node in &order {
            if *node == terminal || stops.contains(node) {
                continue;
            }
            self.emit_node(*node)?;
        }
        let inputs = self.emit_terminal(terminal)?;
        Ok(Partition {
            body: core::mem::take(&mut self.body),
            inputs,
        })
    }

    /// One cut, as a vertex-stage partition of its own — the same shape a
    /// hand-wired `output.varying` compiles into, rooted at the node the
    /// analysis chose instead of at a terminal. The partition "returns"
    /// the cut socket's emitted expression, carried in
    /// `inputs` under the varying socket's name so `finish` reads it the
    /// same way it reads a manual interpolant's.
    fn emit_cut(&mut self, socket: &SocketRef) -> Result<Partition, CodegenError> {
        let needed = self.graph.dependencies_of(socket.node);
        let order = self
            .graph
            .topological_order(Some(&needed))
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;
        self.stage = ShaderStage::Vertex;
        self.bindings.clear();
        self.body.clear();
        for node in &order {
            if *node == socket.node {
                continue;
            }
            self.emit_node(*node)?;
        }
        self.emit_node(socket.node)?;
        let expr = self.bindings.get(socket).cloned().ok_or_else(|| {
            CodegenError::Invalid(GraphErrors(vec![GraphError::UnknownNode(socket.node)]))
        })?;
        Ok(Partition {
            body: core::mem::take(&mut self.body),
            inputs: vec![(abi::SOCKET_VARYING.to_string(), expr)],
        })
    }

    /// The struct a context read reads through in the stage being
    /// emitted.
    fn context_var(&self) -> &'static str {
        match self.stage {
            ShaderStage::Vertex => abi::VERTEX_CONTEXT_VAR,
            ShaderStage::Fragment => abi::CONTEXT_VAR,
        }
    }

    /// Import the fixed vocabulary every generated module uses.
    ///
    /// The deferred-path items are imported unconditionally; when the
    /// deferred fragment entry is dropped by conditional translation they
    /// become unused, and the compiler strips them.
    fn request_abi_imports(&mut self, displaces: bool, shades: bool, interpolates: bool) {
        // Only what this stage's module actually mentions. A shadow
        // module for an alpha-tested material imports the context and
        // nothing else — no `Surface`, no shading function, no G-buffer.
        if shades {
            self.request_import(abi::SURFACE_MODULE, abi::SURFACE_STRUCT);
            self.request_import(abi::SURFACE_MODULE, abi::DEFAULT_SURFACE_FN);
        }
        self.request_import(abi::SURFACE_MODULE, abi::CONTEXT_STRUCT);
        // Both halves of the vertex stage read it: the displacement and
        // every computed interpolant.
        if displaces || interpolates {
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_CONTEXT_STRUCT);
        }
        if self.options.emit_entry_points {
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_IN_STRUCT);
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_OUT_STRUCT);
            if displaces {
                self.request_import(abi::VERTEX_MODULE, abi::VERTEX_CONTEXT_FN);
                self.request_import(abi::VERTEX_MODULE, abi::TRANSFORM_VERTEX_OFFSET_FN);
            } else {
                self.request_import(abi::VERTEX_MODULE, abi::TRANSFORM_VERTEX_FN);
            }
            if interpolates {
                self.request_import(abi::VERTEX_MODULE, abi::VERTEX_CONTEXT_FN);
            }
            self.request_import(abi::VERTEX_MODULE, abi::SURFACE_CONTEXT_FN);
            match self.options.stage.output() {
                abi::StageOutput::Color | abi::StageOutput::PeelResolve => {
                    // The dispatch and the whole shading function come from
                    // the material's lighting model, generated here rather
                    // than imported from a fixed module: the call inside
                    // the light loop names the model.
                    let generated = crate::lighting::shade_surface_with(
                        &crate::lighting::Dispatch::Direct(*self.options.material.lighting.model()),
                        self.options.material.lighting.features(),
                        // This module declares the macros in effect itself;
                        // a second declaration would not compile.
                        false,
                    );
                    for (module, item) in &generated.imports {
                        self.request_import(module, item);
                    }
                    self.lighting_source = generated.source;
                }
                abi::StageOutput::GBuffer => {
                    // The struct's fields are the plan's layout — the set's
                    // requests plus any feature channels — so the pack is
                    // generated beside it instead of imported from a fixed
                    // module.
                    let forward_shaded = self.options.material.forward_shaded;
                    let mut generated = crate::lighting::pack_gbuffer(
                        self.options.material.lighting.model(),
                        self.options.material.lighting.set(),
                        self.options.material.lighting.features(),
                        forward_shaded,
                    );
                    if forward_shaded {
                        // The same shading function a forward module gets:
                        // a forward-shaded material runs its own model in
                        // this stage, and the pack stores its output
                        // (plan5 D2). The macro set the material resolved
                        // with has already dropped the shadow lookup.
                        let shaded = crate::lighting::shade_surface_with(
                            &crate::lighting::Dispatch::Direct(
                                *self.options.material.lighting.model(),
                            ),
                            self.options.material.lighting.features(),
                            false,
                        );
                        for (module, item) in &shaded.imports {
                            self.request_import(module, item);
                        }
                        generated.source.push('\n');
                        generated.source.push_str(&shaded.source);
                    }
                    for (module, item) in &generated.imports {
                        self.request_import(module, item);
                    }
                    self.lighting_source = generated.source;
                }
                abi::StageOutput::Velocity => {
                    // The whole velocity ABI comes from one module, and the
                    // fragment is position arithmetic — no surface, no
                    // shading function. The previous-frame context is only
                    // called when the graph displaces, but an unused import
                    // is stripped, and one arm per stage reads better than
                    // import accounting per shape.
                    self.request_import(abi::VELOCITY_MODULE, abi::VELOCITY_OUT_STRUCT);
                    self.request_import(abi::VELOCITY_MODULE, abi::TRANSFORM_VERTEX_VELOCITY_FN);
                    self.request_import(abi::VELOCITY_MODULE, abi::PREVIOUS_CLIP_POSITION_FN);
                    self.request_import(abi::VELOCITY_MODULE, abi::VELOCITY_CONTEXT_FN);
                    self.request_import(abi::VELOCITY_MODULE, abi::SCREEN_MOTION_FN);
                    if displaces {
                        self.request_import(abi::VELOCITY_MODULE, abi::PREVIOUS_CONTEXT_FN);
                    }
                }
                abi::StageOutput::Nothing | abi::StageOutput::PeelDepth => {}
            }
        }
    }

    /// The trimmed value of a declaring node's `setting`.
    ///
    /// `Graph::validate` has already rejected an empty or malformed one, so
    /// reaching the error here means codegen ran on an unvalidated graph.
    fn declared_name(&self, node: NodeId, setting: &str) -> Result<String, CodegenError> {
        let value = self
            .graph
            .setting(self.registry, node, setting)
            .unwrap_or_default()
            .trim();
        if WxslIdent::new(value).is_none() {
            return Err(CodegenError::Invalid(GraphErrors(vec![
                GraphError::InvalidSetting {
                    node,
                    setting: setting.to_string(),
                    value: value.to_string(),
                    reason: "must be a valid WXSL identifier".to_string(),
                },
            ])));
        }
        Ok(value.to_string())
    }

    fn definition(&self, node: NodeId) -> Result<&NodeDefinition, CodegenError> {
        self.graph
            .definition(self.registry, node)
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))
    }

    /// The WXSL expression feeding `node`'s input `socket`, or `None` if the
    /// input is optional and nothing feeds it.
    fn input_expr(&self, node: NodeId, socket_name: &str) -> Result<Option<String>, CodegenError> {
        let reference = SocketRef::new(node, socket_name);
        if let Some(edge) = self.graph.edge_into(&reference) {
            let expr = self.bindings.get(&edge.from).ok_or_else(|| {
                // Only reachable if the topological order was wrong.
                CodegenError::Invalid(GraphErrors(vec![GraphError::Cycle {
                    nodes: vec![edge.from.node, node],
                }]))
            })?;
            return Ok(Some(expr.clone()));
        }

        let def = self.definition(node)?;
        let socket = def
            .input(socket_name)
            .ok_or_else(|| CodegenError::BadTemplate {
                def: def.id.clone(),
                placeholder: socket_name.to_string(),
            })?;
        let instance = self.graph.node(node).ok_or_else(|| {
            CodegenError::Invalid(GraphErrors(vec![GraphError::UnknownNode(node)]))
        })?;

        // A generic socket's default is a scalar to spread over whatever
        // this instance resolved to (`Socket::splat_default`), so the type
        // has to be looked up before the default can be read.
        let value: Option<Value> = instance.params.get(socket_name).copied().or_else(|| {
            self.graph
                .effective_type(node, socket)
                .and_then(|ty| socket.default_for(ty))
        });
        match value {
            Some(value) => {
                let literal =
                    value
                        .wxsl_literal()
                        .ok_or_else(|| CodegenError::UnrepresentableValue {
                            socket: reference.clone(),
                        })?;
                Ok(Some(literal))
            }
            None if socket.optional => Ok(None),
            None => Err(CodegenError::Invalid(GraphErrors(vec![
                GraphError::MissingInput { socket: reference },
            ]))),
        }
    }

    fn emit_node(&mut self, node: NodeId) -> Result<(), CodegenError> {
        let def = self.definition(node)?.clone();
        for (module, item) in def.all_imports() {
            let items = self.imports.entry(module).or_default();
            if !items.contains(&item) {
                items.push(item);
            }
        }

        match &def.body {
            NodeBody::Expr(exprs) => {
                if exprs.len() != def.outputs.len() {
                    return Err(CodegenError::OutputArityMismatch {
                        def: def.id.clone(),
                        outputs: def.outputs.len(),
                        exprs: exprs.len(),
                    });
                }
                for (socket, template) in def.outputs.iter().zip(exprs) {
                    let target = SocketRef::new(node, socket.name.as_str());
                    // Skip outputs nobody reads: they are dead code, and the
                    // `let` would be an unused-variable warning downstream.
                    if self.graph.edge_from(&target).is_none() {
                        continue;
                    }
                    let expr = self.expand_template(node, &def, template)?;
                    let name = binding_name(node, socket.name.as_str());
                    // `socket.ty` is only a placeholder on a generic socket
                    // (see `Socket::generic`); this instance's resolved type
                    // is what the emitted WGSL must actually declare.
                    // `validate()` (run before codegen starts, in
                    // `generate()`) guarantees every generic parameter this
                    // node's definition declares is resolved by now.
                    let ty = self.graph.effective_type(node, socket).expect(
                        "a validated graph resolves every generic parameter its nodes declare",
                    );
                    // A texture or a sampler is a handle, and WGSL has no
                    // `let` for one: a node handing out a binding — the
                    // screen ABI's input image — binds the name straight
                    // through, exactly as a `NodeBody::Resource` does.
                    if ty.is_resource() {
                        self.bindings.insert(target, expr);
                        continue;
                    }
                    let _ = writeln!(self.body, "    let {name}: {} = {expr};", ty.wxsl_type());
                    self.bindings.insert(target, name);
                }
            }
            NodeBody::Call(func) => {
                let mut args = Vec::with_capacity(func.params.len());
                for param in &func.params {
                    let expr = self.input_expr(node, param.name.as_str())?.ok_or_else(|| {
                        CodegenError::Invalid(GraphErrors(vec![GraphError::MissingInput {
                            socket: SocketRef::new(node, param.name.as_str()),
                        }]))
                    })?;
                    args.push(expr);
                }
                // A generic function node calls a WXSL *template*
                // (`fn safe_normalize<T: vec2f | vec3f | vec4f>`), and the
                // graph always knows the type exactly, so the type
                // arguments are written explicitly — which is the case
                // ADR 0012 says never has to guess. Declaration order is
                // the argument order.
                let type_args = self.type_arguments(node, &def)?;
                let call = format!("{}{type_args}({})", func.name, args.join(", "));
                let name = binding_name(node, "call");
                match &func.ret {
                    FunctionReturn::Value(socket) => {
                        // Generic here too: the return socket's `ty` is only
                        // a placeholder when it carries a parameter.
                        let ty = self.graph.effective_type(node, socket).unwrap_or(socket.ty);
                        let _ = writeln!(self.body, "    let {name}: {} = {call};", ty.wxsl_type());
                        self.bindings
                            .insert(SocketRef::new(node, socket.name.as_str()), name);
                    }
                    FunctionReturn::Struct {
                        name: struct_name,
                        fields,
                    } => {
                        // Bind the call once, then read the fields off it, so
                        // a multi-output function is evaluated once however
                        // many of its outputs the graph uses.
                        let _ = writeln!(self.body, "    let {name}: {struct_name} = {call};");
                        for field in fields {
                            self.bindings.insert(
                                SocketRef::new(node, field.name.as_str()),
                                format!("{name}.{}", field.name),
                            );
                        }
                    }
                }
            }
            NodeBody::ContextRead(field) | NodeBody::VertexContextRead(field) => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                // In a bake module there is no context to read: the uv is
                // the generated function's parameter — the bake domain —
                // and every other field is the purity check's to have
                // refused before emission began.
                if self.bake_domain {
                    if matches!(def.body, NodeBody::ContextRead(_)) && field.as_str() == "uv" {
                        self.bindings
                            .insert(SocketRef::new(node, socket.name.as_str()), "uv".to_string());
                        return Ok(());
                    }
                    return Err(CodegenError::Invalid(GraphErrors(vec![
                        GraphError::InvalidBake {
                            texture: String::new(),
                            reason: "reads something a bake cannot evaluate".to_string(),
                        },
                    ])));
                }
                // The same node in either stage, reading whichever
                // context struct this partition was handed — which is
                // exactly why the vertex context is a superset of the
                // fragment one.
                self.bindings.insert(
                    SocketRef::new(node, socket.name.as_str()),
                    format!("{}.{field}", self.context_var()),
                );
            }
            NodeBody::Param => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                // A field read off the uniform buffer, bound as an
                // expression rather than a `let`: it is one uniform load
                // wherever it is used, and giving it a name would only add
                // a line. `read_expr` rather than `material.name` because
                // a `bool` parameter is stored as a `u32` and this is not
                // the place that knows it.
                let name = self.declared_name(node, node::SETTING_NAME)?;
                let expr = self
                    .interface
                    .params
                    .read_expr(abi::MATERIAL_PARAMS_VAR, &name)
                    .ok_or_else(|| {
                        // Only reachable if `interface_of` and this walk
                        // disagreed about what is reachable.
                        CodegenError::UndeclaredParam {
                            node,
                            name: name.clone(),
                        }
                    })?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), expr);
            }
            NodeBody::Resource => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                // The declared variable *is* the value: a texture handle
                // is passed to `textureSample` and to functions by name.
                let name = self.declared_name(node, node::SETTING_NAME)?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), name);
            }
            NodeBody::UserRead => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                let field = self.declared_name(node, node::SETTING_FIELD)?;
                let user = self
                    .interface
                    .user
                    .as_ref()
                    .ok_or(CodegenError::NoUserBlock { node })?;
                let expr = user
                    .layout
                    .read_expr(user.name.as_str(), &field)
                    .ok_or_else(|| CodegenError::UndeclaredParam {
                        node,
                        name: field.clone(),
                    })?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), expr);
            }
            NodeBody::AttributeRead => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                let name = self.declared_name(node, node::SETTING_NAME)?;
                let geometry = &self.interface.geometry;
                // Which backing a name has is the *declaration's* business
                // and not the node's, so this is where the three
                // frequencies stop being different: a per-vertex value and
                // a computed interpolant were both interpolated into
                // `attrs`, and a per-instance one is a field of the row
                // the flat index points at.
                let interpolated = geometry.vertex_attribute(name.as_str()).is_some()
                    || geometry.computed_varying(name.as_str()).is_some();
                let expr = if interpolated {
                    Some(format!("{}.{name}", abi::MATERIAL_ATTRIBUTES_VAR))
                } else {
                    geometry.instance().field(name.as_str()).and_then(|_| {
                        geometry.instance().read_expr(
                            &format!(
                                "{}[{}.{}]",
                                abi::MATERIAL_INSTANCE_VAR,
                                abi::MATERIAL_ATTRIBUTES_VAR,
                                abi::INSTANCE_INDEX_FIELD,
                            ),
                            name.as_str(),
                        )
                    })
                };
                let expr = expr.ok_or_else(|| CodegenError::UndeclaredAttribute {
                    node,
                    name: name.clone(),
                })?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), expr);
            }
            NodeBody::SurfaceOutput
            | NodeBody::VertexOutput
            | NodeBody::DiscardOutput
            | NodeBody::VaryingOutput
            | NodeBody::ScreenOutput => {
                // Emitted by `emit_surface`, which needs to run last.
                unreachable!("the surface output node is emitted separately");
            }
            NodeBody::Document => {
                // A pipeline document has no surface output, so `generate`
                // stops at `NoOutputNode` long before the walk reaches it.
                // It compiles to a `RenderGraph` (in `wxsl-render`), never
                // to WXSL.
                unreachable!("a pipeline document node has no WXSL to emit");
            }
        }
        Ok(())
    }

    /// Collect the surface field assignments from the output node.
    fn emit_terminal(&mut self, node: NodeId) -> Result<Vec<(String, String)>, CodegenError> {
        let def = self.definition(node)?.clone();
        let mut assignments = Vec::new();
        for socket in &def.inputs {
            if let Some(expr) = self.input_expr(node, socket.name.as_str())? {
                assignments.push((socket.name.to_string(), expr));
            }
        }
        Ok(assignments)
    }

    /// The `<vec3f, f32>` a call to a generic function node needs, or the
    /// empty string for a definition that declares no type parameters.
    fn type_arguments(&self, node: NodeId, def: &NodeDefinition) -> Result<String, CodegenError> {
        if def.generics.is_empty() {
            return Ok(String::new());
        }
        let mut args = Vec::with_capacity(def.generics.len());
        for param in &def.generics {
            let ty = self
                .graph
                .generic_type(node, param.name.as_str())
                .ok_or_else(|| {
                    CodegenError::Invalid(GraphErrors(vec![GraphError::UnresolvedGeneric {
                        node,
                        param: param.name.as_str().to_string(),
                    }]))
                })?;
            args.push(ty.wxsl_type());
        }
        Ok(format!("<{}>", args.join(", ")))
    }

    /// Substitute `{socket}` placeholders in an expression template.
    fn expand_template(
        &self,
        node: NodeId,
        def: &NodeDefinition,
        template: &str,
    ) -> Result<String, CodegenError> {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open].replace("}}", "}"));
            rest = &rest[open + 1..];
            if let Some(stripped) = rest.strip_prefix('{') {
                out.push('{');
                rest = stripped;
                continue;
            }
            let close = rest.find('}').ok_or_else(|| CodegenError::BadTemplate {
                def: def.id.clone(),
                placeholder: rest.to_string(),
            })?;
            let name = &rest[..close];
            rest = &rest[close + 1..];
            // `{$T}` is this node instance's resolved type for generic
            // parameter `T`, spelled as WGSL — what lets a template that
            // has to *name* its type (`vec3f(value)`, a `select` fallback
            // of the right width) stay one template across every type the
            // parameter allows, instead of one per type.
            if let Some(param) = name.strip_prefix('$') {
                let ty = self.graph.generic_type(node, param).ok_or_else(|| {
                    CodegenError::BadTemplate {
                        def: def.id.clone(),
                        placeholder: name.to_string(),
                    }
                })?;
                out.push_str(ty.wxsl_type());
                continue;
            }
            let expr = self
                .input_expr(node, name)?
                .ok_or_else(|| CodegenError::BadTemplate {
                    def: def.id.clone(),
                    placeholder: name.to_string(),
                })?;
            out.push_str(&as_atom(&expr));
        }
        // Trailing text, and literal `}}` escapes in it.
        out.push_str(&rest.replace("}}", "}"));
        Ok(out)
    }

    fn finish(self, graph: &Graph, macros: &MacroSet, parts: &Partitions) -> String {
        let Emitter {
            options,
            interface,
            imports,
            lighting_source,
            ..
        } = self;
        let mut out = String::with_capacity(2048);
        write_header(&mut out, graph, macros, &imports);
        write_declarations(&mut out, &interface);

        // The material function takes what the geometry supplied as a
        // second argument, and only when there is any: a graph that
        // declares nothing generates the signature it always did.
        let attributes = if interface.geometry.is_empty() {
            String::new()
        } else {
            format!(
                ", {}: {}",
                abi::MATERIAL_ATTRIBUTES_VAR,
                abi::MATERIAL_ATTRIBUTES_STRUCT
            )
        };

        // One function per partition, in stage order. Each is a complete
        // little program over its own subgraph; nothing is shared
        // between them but the module's declarations.
        if let Some(vertex) = &parts.vertex {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}) -> vec3f {{\n",
                abi::VERTEX_FN,
                abi::VERTEX_CONTEXT_VAR,
                abi::VERTEX_CONTEXT_STRUCT,
            );
            out.push_str(&vertex.body);
            let _ = writeln!(
                out,
                "    return {};",
                vertex.input(abi::SOCKET_POSITION_OFFSET, "vec3f(0.0, 0.0, 0.0)")
            );
            out.push_str("}\n");
        }

        // One function per computed interpolant, before the fragment
        // ones because they run first — and because reading the module
        // top to bottom should read as vertex stage then fragment stage.
        for (name, part) in &parts.varyings {
            let Some(varying) = interface.geometry.computed_varying(name.as_str()) else {
                continue;
            };
            // Unreachable — the socket is not optional — but a generated
            // function has to return something whatever the graph is.
            let zero = varying
                .ty
                .zero()
                .and_then(|value| value.wxsl_literal())
                .unwrap_or_else(|| "0.0".to_string());
            let _ = write!(
                out,
                "\nfn {}{name}({}: {}{attributes}) -> {} {{\n",
                abi::VARYING_FN_PREFIX,
                abi::VERTEX_CONTEXT_VAR,
                abi::VERTEX_CONTEXT_STRUCT,
                varying.ty.wxsl_type(),
            );
            out.push_str(&part.body);
            let _ = writeln!(
                out,
                "    return {};",
                part.input(abi::SOCKET_VARYING, &zero)
            );
            out.push_str("}\n");
        }

        if let Some(discard) = &parts.discard {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}) -> bool {{\n",
                abi::DISCARD_FN,
                abi::CONTEXT_VAR,
                abi::CONTEXT_STRUCT,
            );
            out.push_str(&discard.body);
            let _ = writeln!(
                out,
                "    return {};",
                discard.input(abi::SOCKET_DISCARD, "false")
            );
            out.push_str("}\n");
        }

        if let Some(surface) = &parts.surface {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}) -> {} {{\n",
                options.material_fn,
                abi::CONTEXT_VAR,
                abi::CONTEXT_STRUCT,
                abi::SURFACE_STRUCT
            );
            out.push_str(&surface.body);
            let _ = writeln!(
                out,
                "    var surface: {} = {}({});",
                abi::SURFACE_STRUCT,
                abi::DEFAULT_SURFACE_FN,
                abi::CONTEXT_VAR,
            );
            for (field, expr) in &surface.inputs {
                let _ = writeln!(out, "    surface.{field} = {expr};");
            }
            out.push_str("    return surface;\n}\n");
        }

        // What the lighting model generated for this stage: the dispatch
        // and the shading function, or the G-buffer struct and pack. One
        // block, appended before the entry points, because WGSL does not
        // care about declaration order and a reader reads the material's
        // own functions first.
        if !lighting_source.is_empty() {
            out.push('\n');
            out.push_str(&lighting_source);
        }

        if options.emit_entry_points {
            write_entry_points(&mut out, options, &interface, parts);
        }
        out
    }
}

/// The lines every generated module starts with, in either domain: where it
/// came from, what it imports, and the macro values it was generated at.
///
/// A WXSL module declares the knobs it uses (ADR 0011), so there is no
/// shared macro module to import from: the generated module is
/// self-contained, and the renderer binds the same values over the top.
/// There is no render-path flag any more either — a stage is chosen by
/// generating that stage's module, not by an `@if` inside a module holding
/// all of them (ADR 0022).
fn write_header(
    out: &mut String,
    graph: &Graph,
    macros: &MacroSet,
    imports: &BTreeMap<ModulePath, Vec<WxslIdent>>,
) {
    let _ = writeln!(
        out,
        "// Generated by wxsl-core from graph `{}`.",
        graph.name()
    );
    out.push_str("// Do not edit: regenerate from the graph instead.\n");
    let signature = macros.signature();
    if !signature.is_empty() {
        let _ = writeln!(out, "// Macros: {signature}");
    }
    out.push('\n');

    for (module, items) in imports {
        let mut items: Vec<&str> = items.iter().map(WxslIdent::as_str).collect();
        items.sort_unstable();
        match items.as_slice() {
            [only] => {
                let _ = writeln!(out, "import {module}::{only};");
            }
            many => {
                let _ = writeln!(out, "import {module}::{{{}}};", many.join(", "));
            }
        }
    }

    for (name, value) in macros.iter() {
        let Some(ident) = WxslIdent::new(name) else {
            continue;
        };
        match value {
            MacroValue::Flag(flag) => {
                let _ = writeln!(out, "@macro const {ident}: bool = {flag};");
            }
            MacroValue::Int(number) => {
                let _ = writeln!(out, "@macro const {ident}: i32 = {number};");
            }
            MacroValue::Float(number) => {
                let mut literal = String::new();
                if crate::wxsl::write_f32(&mut literal, number).is_some() {
                    let _ = writeln!(out, "@macro const {ident}: f32 = {literal};");
                }
            }
        }
    }
}

/// Emit what the material needs bound: its own group, and the block it
/// expects the application to supply.
///
/// The struct is generated from the computed layout rather than written
/// with `@align`/`@size` attributes, because the field order was chosen so
/// that WGSL's own rules put every field exactly where the layout says.
/// Two statements of the same offsets would be two places to disagree.
fn write_declarations(out: &mut String, interface: &MaterialInterface) {
    if interface.material_group_is_empty()
        && interface.user.is_none()
        && interface.geometry.is_empty()
    {
        return;
    }
    out.push('\n');
    if !interface.params.is_empty() {
        out.push_str(&interface.params.wgsl_struct(abi::MATERIAL_PARAMS_STRUCT));
        let _ = writeln!(
            out,
            "@group({}) @binding({}) var<uniform> {}: {};",
            abi::GROUP_MATERIAL,
            abi::BINDING_MATERIAL_PARAMS,
            abi::MATERIAL_PARAMS_VAR,
            abi::MATERIAL_PARAMS_STRUCT,
        );
    }
    for resource in &interface.resources {
        let _ = writeln!(
            out,
            "@group({}) @binding({}) var {}: {};",
            abi::GROUP_MATERIAL,
            resource.binding,
            resource.name,
            resource.ty.wxsl_type(),
        );
    }
    if let Some(user) = &interface.user {
        out.push_str(&user.layout.wgsl_struct(&user.struct_name));
        let _ = writeln!(
            out,
            "@group({}) @binding({}) var<uniform> {}: {};",
            abi::GROUP_USER,
            abi::BINDING_USER_BLOCK,
            user.name,
            user.struct_name,
        );
    }
    write_geometry_declarations(out, &interface.geometry);
}

/// Emit what the geometry supplies: the struct the material function
/// receives it in, and — when the graph declares per-instance attributes —
/// a *second, wider* view of the frame's instance buffer.
///
/// The attribute array is beside the transform array rather than inside
/// it. Widening the transform row was the obvious shape and it is wrong:
/// the vertex stage reads that array at the ABI's stride through
/// `transform_vertex`, so a row that grew would be read at the wrong
/// offsets by hand-written code that cannot know it grew. Two arrays,
/// one instance index, and neither has to know the other's width
/// (ADR 0024).
fn write_geometry_declarations(out: &mut String, geometry: &GeometryInterface) {
    if geometry.is_empty() {
        return;
    }
    let _ = writeln!(out, "struct {} {{", abi::MATERIAL_ATTRIBUTES_STRUCT);
    if geometry.instance_index_location().is_some() {
        let _ = writeln!(out, "    {}: u32,", abi::INSTANCE_INDEX_FIELD);
    }
    for attribute in geometry.vertex() {
        let _ = writeln!(out, "    {}: {},", attribute.name, attribute.ty.wxsl_type());
    }
    // Present in the vertex stage too, where they are the value being
    // computed rather than a value to read — left zeroed there, and
    // `Graph::check_outputs` is what stops anything reading them.
    for varying in geometry.computed() {
        let _ = writeln!(out, "    {}: {},", varying.name, varying.ty.wxsl_type());
    }
    out.push_str("}\n");

    if geometry.instance_index_location().is_some() {
        out.push_str(
            &geometry
                .instance()
                .wgsl_struct(abi::MATERIAL_INSTANCE_STRUCT),
        );
        let _ = writeln!(
            out,
            "@group({}) @binding({}) var<storage, read> {}: array<{}>;",
            abi::GROUP_FRAME,
            abi::BINDING_INSTANCE_ATTRIBUTES,
            abi::MATERIAL_INSTANCE_VAR,
            abi::MATERIAL_INSTANCE_STRUCT,
        );
    }
}

/// The extra `@location` lines shared by the extended vertex output and
/// the fragment's second parameter, written once so the two cannot drift.
fn extra_varyings(geometry: &GeometryInterface) -> String {
    let mut out = String::new();
    if let Some(location) = geometry.instance_index_location() {
        // Flat, and it has to be: WGSL will not interpolate an integer,
        // and an interpolated instance index is a row somewhere between
        // two objects.
        let _ = writeln!(
            out,
            "    @location({location}) @interpolate(flat) {}: u32,",
            abi::INSTANCE_INDEX_FIELD,
        );
    }
    for attribute in geometry.vertex() {
        let _ = writeln!(
            out,
            "    @location({}) {}: {},",
            attribute.varying,
            attribute.name,
            attribute.ty.wxsl_type(),
        );
    }
    for varying in geometry.computed() {
        let _ = writeln!(
            out,
            "    @location({}) {}: {},",
            varying.varying,
            varying.name,
            varying.ty.wxsl_type(),
        );
    }
    out
}

/// The inter-stage location the velocity stage's previous clip position
/// takes in a module with `geometry`'s declared attributes: one past
/// every location the material's own extras spent. A module without
/// declared attributes carries it in [`abi::VELOCITY_OUT_STRUCT`] at the
/// first location after the base fields instead, so this is only asked
/// for the geometry case.
fn previous_clip_location(geometry: &GeometryInterface) -> u32 {
    geometry
        .vertex()
        .iter()
        .map(|attribute| attribute.varying)
        .chain(geometry.computed().iter().map(|varying| varying.varying))
        .chain(geometry.instance_index_location())
        .max()
        .map(|last| last + 1)
        .unwrap_or(abi::VERTEX_OUT_FIELDS.len() as u32)
}

/// The IO structs a material with declared attributes needs, beside the
/// ABI's own.
fn write_geometry_io(out: &mut String, geometry: &GeometryInterface, stage: abi::MaterialStage) {
    if geometry.is_empty() {
        return;
    }
    out.push('\n');
    if !geometry.vertex().is_empty() {
        let _ = writeln!(out, "struct {} {{", abi::MATERIAL_VERTEX_IN_STRUCT);
        for attribute in geometry.vertex() {
            let _ = writeln!(
                out,
                "    @location({}) {}: {},",
                attribute.location,
                attribute.name,
                attribute.ty.wxsl_type(),
            );
        }
        out.push_str("}\n");
    }
    // The velocity stage carries two more interpolated values than the
    // interface's location budget accounted: this frame's clip and last
    // frame's, which no other stage reads. They take the locations
    // *after* the material's own extras — no interface location moves,
    // and a stage-independent material resolves to two IO shapes that
    // differ only where the velocity module says so.
    let first_clip_location = previous_clip_location(geometry);
    let velocity_clips = match stage.output() {
        abi::StageOutput::Velocity => format!(
            "    @location({}) {}: vec4f,\n    @location({}) {}: vec4f,\n",
            first_clip_location,
            abi::CURRENT_CLIP_FIELD,
            first_clip_location + 1,
            abi::PREVIOUS_CLIP_FIELD
        ),
        _ => String::new(),
    };
    // An entry point returns one value, so this is the one struct that
    // cannot be split into "the ABI's half" and "the material's half". Its
    // base half is written from `abi::VERTEX_OUT_FIELDS`, which is also
    // what `vertex.wxsl` declares — the table is the agreement.
    let _ = writeln!(out, "struct {} {{", abi::MATERIAL_VERTEX_OUT_STRUCT);
    let _ = writeln!(
        out,
        "    @builtin(position) {}: vec4f,",
        abi::CLIP_POSITION_FIELD
    );
    for (location, field) in abi::VERTEX_OUT_FIELDS.iter().enumerate() {
        let _ = writeln!(
            out,
            "    @location({location}) {}: {},",
            field.name,
            field.ty.wxsl_type()
        );
    }
    out.push_str(&extra_varyings(geometry));
    out.push_str(&velocity_clips);
    out.push_str("}\n");

    // The fragment side takes two parameters instead, so the ABI's own
    // `VertexOut` still arrives unchanged and `surface_context` needs no
    // widening at all.
    let _ = writeln!(out, "struct {} {{", abi::MATERIAL_VARYINGS_STRUCT);
    out.push_str(&extra_varyings(geometry));
    out.push_str(&velocity_clips);
    out.push_str("}\n");
}

/// The pass-group bindings a peel stage's fragment samples, in the order
/// the peel pass lists its reads. Other stages declare none: a geometry
/// pass that reads the pass group is a peel.
fn write_peel_declarations(out: &mut String, stage: abi::MaterialStage) {
    let bindings = match stage {
        abi::MaterialStage::PEEL_FRONT | abi::MaterialStage::PEEL_DEPTH => {
            "\n@group(3) @binding(0) var wxsl_scene_depth: texture_depth_2d;\n\
             @group(3) @binding(1) var wxsl_peel_bounds: texture_2d<f32>;\n"
        }
        abi::MaterialStage::PEEL_BACK => {
            "\n@group(3) @binding(0) var wxsl_scene_depth: texture_depth_2d;\n\
             @group(3) @binding(1) var wxsl_peel_bounds: texture_2d<f32>;\n\
             @group(3) @binding(2) var wxsl_peel_front: texture_depth_2d;\n"
        }
        abi::MaterialStage::PEEL_RESOLVE => {
            "\nstruct WxslPeelColors {\n    \
             @location(0) front: vec4f,\n    \
             @location(1) back: vec4f,\n}\n\
             \n@group(3) @binding(0) var wxsl_scene_depth: texture_depth_2d;\n\
             @group(3) @binding(1) var wxsl_peel_pair: texture_2d<f32>;\n"
        }
        _ => return,
    };
    out.push_str(bindings);
}

/// Discard fragments behind the opaque surface or outside the window the
/// previous iteration left. The bias on the window is `1e-5`: wide enough
/// that the layer just peeled is not taken again, narrow enough that the
/// crack between two surfaces stays under a pixel. Defines `wxsl_peel_z`
/// and `wxsl_peel_px`.
fn peel_window() -> &'static str {
    "    let wxsl_peel_z = vertex.clip_position.z;\n    \
     let wxsl_peel_px = vec2i(vertex.clip_position.xy);\n    \
     let wxsl_peel_scene_z = textureLoad(wxsl_scene_depth, wxsl_peel_px, 0);\n    \
     let wxsl_peel_window = textureLoad(wxsl_peel_bounds, wxsl_peel_px, 0);\n    \
     if wxsl_peel_z >= wxsl_peel_scene_z || wxsl_peel_z <= wxsl_peel_window.r + 1e-5 || wxsl_peel_z >= wxsl_peel_window.g - 1e-5 {\n        \
     discard;\n    }\n"
}

/// Emit the vertex entry, and the fragment entry this stage calls for.
///
/// The vertex stage is the same for every stage — the paths differ in what
/// the fragment stage does with the surface, never in how geometry is
/// transformed — so a stage with no fragment entry is a complete,
/// depth-writing shader on its own.
fn write_entry_points(
    out: &mut String,
    options: &CodegenOptions,
    interface: &MaterialInterface,
    parts: &Partitions,
) {
    let geometry = &interface.geometry;
    write_geometry_io(out, geometry, options.stage);
    // Bound once when anything in the vertex stage wants it — the
    // displacement, an interpolant, or both — and never computed twice.
    // Everything reads the *undisplaced* context, which is the input to
    // the displacement rather than its result.
    let needs_context = parts.vertex.is_some() || !parts.varyings.is_empty();
    let context = match needs_context {
        true => format!(
            "    let {var} = {call}(input);\n",
            var = abi::VERTEX_CONTEXT_VAR,
            call = abi::VERTEX_CONTEXT_FN,
        ),
        false => String::new(),
    };
    // How the vertex entry gets its transform: the plain one, or the one
    // that takes an offset the graph computed first.
    let transform = |args: &str| match parts.vertex.is_some() {
        true => format!(
            "{}(input, {}({}{args}))",
            abi::TRANSFORM_VERTEX_OFFSET_FN,
            abi::VERTEX_FN,
            abi::VERTEX_CONTEXT_VAR,
        ),
        false => format!("{}(input)", abi::TRANSFORM_VERTEX_FN),
    };
    // The velocity stage transforms every vertex twice — this frame's
    // camera and instance row, and last frame's. Both offsets come from
    // the *same* graph partition: this frame's against the context the
    // vertex stage always builds, the previous frame's against
    // `previous_vertex_context`, which is how a time-driven displacement
    // answers for the frame that has passed without the graph knowing
    // there is a previous frame at all.
    let velocity = options.stage.output() == abi::StageOutput::Velocity;
    let attrs_arg = match parts.vertex.is_some() && !geometry.is_empty() {
        true => format!(", {}", abi::MATERIAL_ATTRIBUTES_VAR),
        false => String::new(),
    };
    let (offset_now, offset_previous) = match parts.vertex.is_some() {
        true => (
            format!("{}({}{attrs_arg})", abi::VERTEX_FN, abi::VERTEX_CONTEXT_VAR),
            format!(
                "{}({}(input){attrs_arg})",
                abi::VERTEX_FN,
                abi::PREVIOUS_CONTEXT_FN,
            ),
        ),
        false => ("vec3f(0.0)".to_string(), "vec3f(0.0)".to_string()),
    };
    let velocity_transform = format!(
        "{}(input, {offset_now}, {offset_previous})",
        abi::TRANSFORM_VERTEX_VELOCITY_FN,
    );
    if geometry.is_empty() {
        let (vertex_out, call) = match velocity {
            true => (abi::VELOCITY_OUT_STRUCT, velocity_transform.clone()),
            false => (abi::VERTEX_OUT_STRUCT, transform("")),
        };
        let _ = write!(
            out,
            "
@vertex
fn {vertex}(input: {vertex_in}) -> {vertex_out} {{
{context}    return {call};
}}
",
            vertex = options.vertex_entry,
            vertex_in = abi::VERTEX_IN_STRUCT,
        );
    } else {
        let extra_param = if geometry.vertex().is_empty() {
            String::new()
        } else {
            format!(", extra: {}", abi::MATERIAL_VERTEX_IN_STRUCT)
        };
        // The attributes struct is built before the transform, because
        // a graph may displace a vertex by something the geometry
        // supplied — a per-vertex wind weight, a per-instance scale.
        let mut prologue = String::new();
        if needs_context {
            let _ = writeln!(
                prologue,
                "    var {var}: {ty};",
                var = abi::MATERIAL_ATTRIBUTES_VAR,
                ty = abi::MATERIAL_ATTRIBUTES_STRUCT,
            );
            if geometry.instance_index_location().is_some() {
                let _ = writeln!(
                    prologue,
                    "    {var}.{field} = input.instance;",
                    var = abi::MATERIAL_ATTRIBUTES_VAR,
                    field = abi::INSTANCE_INDEX_FIELD,
                );
            }
            for attribute in geometry.vertex() {
                let _ = writeln!(
                    prologue,
                    "    {var}.{name} = extra.{name};",
                    var = abi::MATERIAL_ATTRIBUTES_VAR,
                    name = attribute.name,
                );
            }
        }
        let _ = write!(
            out,
            "
@vertex
fn {vertex}(input: {vertex_in}{extra_param}) -> {vertex_out} {{
{prologue}{context}    let base = {call};
    var out: {vertex_out};
",
            vertex = options.vertex_entry,
            vertex_in = abi::VERTEX_IN_STRUCT,
            vertex_out = abi::MATERIAL_VERTEX_OUT_STRUCT,
            call = match velocity {
                true => velocity_transform.clone(),
                false => transform(&format!(", {}", abi::MATERIAL_ATTRIBUTES_VAR)),
            },
        );
        let _ = writeln!(
            out,
            "    out.{field} = base.{field};",
            field = abi::CLIP_POSITION_FIELD
        );
        for field in abi::VERTEX_OUT_FIELDS {
            let _ = writeln!(out, "    out.{name} = base.{name};", name = field.name);
        }
        if velocity {
            // Last in the struct, because write_geometry_io put them
            // after the material's own extras: the locations the
            // interface's budget did not spend.
            for field in [abi::CURRENT_CLIP_FIELD, abi::PREVIOUS_CLIP_FIELD] {
                let _ = writeln!(out, "    out.{field} = base.{field};");
            }
        }
        if geometry.instance_index_location().is_some() {
            let _ = writeln!(
                out,
                "    out.{} = input.instance;",
                abi::INSTANCE_INDEX_FIELD
            );
        }
        for attribute in geometry.vertex() {
            let _ = writeln!(out, "    out.{name} = extra.{name};", name = attribute.name);
        }
        // What the graph computed for itself, one call each.
        for (name, _) in &parts.varyings {
            let _ = writeln!(
                out,
                "    out.{name} = {prefix}{name}({var}{args});",
                prefix = abi::VARYING_FN_PREFIX,
                var = abi::VERTEX_CONTEXT_VAR,
                args = format_args!(", {}", abi::MATERIAL_ATTRIBUTES_VAR),
            );
        }
        out.push_str("    return out;\n}\n");
    }

    let stage = options.stage;
    if !stage.always_has_fragment() && parts.discard.is_none() {
        // Nothing to write and nothing to throw away: no fragment stage
        // at all, which is the whole economy of a depth prepass.
        return;
    }
    let fragment = stage.fragment_entry();
    // Unpacking the extras into a plain struct, rather than handing the
    // IO struct itself to the material function: the material function is
    // ordinary code, and an entry-point IO type is not the shape to make
    // it depend on.
    let (params, unpack, args) = if geometry.is_empty() {
        (String::new(), String::new(), String::new())
    } else {
        let mut unpack = format!(
            "    var {var}: {ty};\n",
            var = abi::MATERIAL_ATTRIBUTES_VAR,
            ty = abi::MATERIAL_ATTRIBUTES_STRUCT,
        );
        if geometry.instance_index_location().is_some() {
            let _ = writeln!(
                unpack,
                "    {var}.{field} = extra.{field};",
                var = abi::MATERIAL_ATTRIBUTES_VAR,
                field = abi::INSTANCE_INDEX_FIELD,
            );
        }
        for name in geometry
            .vertex()
            .iter()
            .map(|attribute| &attribute.name)
            .chain(geometry.computed().iter().map(|varying| &varying.name))
        {
            let _ = writeln!(
                unpack,
                "    {var}.{name} = extra.{name};",
                var = abi::MATERIAL_ATTRIBUTES_VAR,
            );
        }
        (
            format!(", extra: {}", abi::MATERIAL_VARYINGS_STRUCT),
            unpack,
            format!(", {}", abi::MATERIAL_ATTRIBUTES_VAR),
        )
    };
    // The discard test comes first in every stage that has one — before
    // the surface is even evaluated, which is the point of it being its
    // own function.
    let test = match parts.discard.is_some() {
        true => format!(
            "    if {discard}({ctx}{args}) {{\n        discard;\n    }}\n",
            discard = abi::DISCARD_FN,
            ctx = abi::CONTEXT_VAR,
        ),
        false => String::new(),
    };
    // The velocity fragment reads no surface: its context exists only
    // for the discard test, and only the non-geometry shape needs the
    // velocity twin of `surface_context` — with declared attributes the
    // fragment still receives the ABI's own `VertexOut`, so the original
    // function fits as it always did.
    let velocity = stage.output() == abi::StageOutput::Velocity;
    // The depth pair is position, like velocity's motion: a context exists
    // only so an alpha test can discard before the pair is written.
    let peel_depth = stage == abi::MaterialStage::PEEL_DEPTH;
    let context_fn = match velocity && geometry.is_empty() {
        true => abi::VELOCITY_CONTEXT_FN,
        false => abi::SURFACE_CONTEXT_FN,
    };
    let prologue = match velocity || peel_depth {
        true => match parts.discard.is_some() {
            true => format!(
                "    let {ctx} = {context}(vertex);\n{unpack}{test}",
                ctx = abi::CONTEXT_VAR,
                context = context_fn,
            ),
            false => String::new(),
        },
        false => format!(
            "    let {ctx} = {context}(vertex);\n{unpack}{test}",
            ctx = abi::CONTEXT_VAR,
            context = context_fn,
        ),
    };
    write_peel_declarations(out, stage);
    let body = match stage.output() {
        abi::StageOutput::Color if stage == abi::MaterialStage::PEEL_FRONT
            || stage == abi::MaterialStage::PEEL_BACK =>
        {
            let window = peel_window();
            let back = match stage == abi::MaterialStage::PEEL_BACK {
                true => {
                    "    let wxsl_peeled_front = textureLoad(wxsl_peel_front, wxsl_peel_px, 0);\n    \
                     if wxsl_peeled_front < 0.999 && wxsl_peel_z <= wxsl_peeled_front + 1e-5 {\n        \
                     discard;\n    }\n"
                }
                false => "",
            };
            format!(
                "-> @location(0) vec4f {{\n{prologue}{window}{back}    \
                 let wxsl_peel_lit = {shade}({material}({ctx}{args}), {ctx});\n    \
                 return vec4f(wxsl_peel_lit.rgb * wxsl_peel_lit.a, wxsl_peel_lit.a);\n}}\n",
                ctx = abi::CONTEXT_VAR,
                shade = abi::SHADE_SURFACE_FN,
                material = options.material_fn,
            )
        }
        abi::StageOutput::Color => format!(
            "-> @location(0) vec4f {{\n{prologue}    \
             return {shade}({material}({ctx}{args}), {ctx});\n}}\n",
            ctx = abi::CONTEXT_VAR,
            shade = abi::SHADE_SURFACE_FN,
            material = options.material_fn,
        ),
        abi::StageOutput::PeelDepth => {
            let window = peel_window();
            format!(
                "-> @location(0) vec2f {{\n{prologue}{window}    \
                 return vec2f(1.0 - wxsl_peel_z, wxsl_peel_z);\n}}\n"
            )
        }
        abi::StageOutput::PeelResolve => format!(
            "-> WxslPeelColors {{\n{prologue}    \
             let wxsl_peel_z = vertex.clip_position.z;\n    \
             let wxsl_peel_px = vec2i(vertex.clip_position.xy);\n    \
             let wxsl_peel_scene_z = textureLoad(wxsl_scene_depth, wxsl_peel_px, 0);\n    \
             if wxsl_peel_z >= wxsl_peel_scene_z {{ discard; }}\n    \
             let wxsl_peel_pair_v = textureLoad(wxsl_peel_pair, wxsl_peel_px, 0);\n    \
             let wxsl_peel_front_z = 1.0 - wxsl_peel_pair_v.r;\n    \
             let wxsl_peel_back_z = wxsl_peel_pair_v.g;\n    \
             let wxsl_peel_lit = {shade}({material}({ctx}{args}), {ctx});\n    \
             let wxsl_peel_premul = vec4f(wxsl_peel_lit.rgb * wxsl_peel_lit.a, wxsl_peel_lit.a);\n    \
             var wxsl_peel_front_color = vec4f(0.0);\n    \
             var wxsl_peel_back_color = vec4f(0.0);\n    \
             if wxsl_peel_pair_v.r > 0.0 && abs(wxsl_peel_z - wxsl_peel_front_z) <= 2e-4 {{\n        \
             wxsl_peel_front_color = wxsl_peel_premul;\n    }}\n    \
             if wxsl_peel_pair_v.g > 0.0 && abs(wxsl_peel_front_z - wxsl_peel_back_z) > 2e-5 && abs(wxsl_peel_z - wxsl_peel_back_z) <= 2e-4 {{\n        \
             wxsl_peel_back_color = wxsl_peel_premul;\n    }}\n    \
             if wxsl_peel_front_color.a == 0.0 && wxsl_peel_back_color.a == 0.0 {{ discard; }}\n    \
             return WxslPeelColors(wxsl_peel_front_color, wxsl_peel_back_color);\n}}\n",
            ctx = abi::CONTEXT_VAR,
            shade = abi::SHADE_SURFACE_FN,
            material = options.material_fn,
        ),
        abi::StageOutput::GBuffer if options.material.forward_shaded => {
            // The owner's "l'albedo est l'output" (plan5 D2): shade here,
            // in the geometry pass, and let the pack store the radiance
            // where the lighting pass looks for it. The id written is the
            // preshaded route's, so the lighting pass returns the stored
            // radiance instead of shading again.
            let route = options
                .material
                .lighting
                .set()
                .preshaded()
                .expect("resolution required the preshaded route in the set");
            format!(
                "-> {gbuffer} {{\n{prologue}    \
                 let surface = {material}({ctx}{args});\n    \
                 return {pack}(surface, {shade}(surface, {ctx}){id});\n}}\n",
                ctx = abi::CONTEXT_VAR,
                gbuffer = abi::GBUFFER_STRUCT,
                material = options.material_fn,
                pack = abi::PACK_GBUFFER_FN,
                shade = abi::SHADE_SURFACE_FN,
                id = if options.material.lighting.set().dispatches() {
                    format!(", {}u", route.id)
                } else {
                    String::new()
                },
            )
        }
        abi::StageOutput::GBuffer => format!(
            "-> {gbuffer} {{\n{prologue}    \
             return {pack}({material}({ctx}{args}){id});\n}}\n",
            ctx = abi::CONTEXT_VAR,
            gbuffer = abi::GBUFFER_STRUCT,
            material = options.material_fn,
            pack = abi::PACK_GBUFFER_FN,
            id = if options.material.lighting.set().dispatches() {
                format!(", {}u", options.material.lighting.model)
            } else {
                String::new()
            },
        ),
        abi::StageOutput::Velocity => {
            // Where this fragment was last frame: the velocity stage's
            // whole answer, interpolated from the two clips its vertex
            // entry computed. With declared attributes the previous clip
            // rides in `extra` — write_geometry_io put it there, at the
            // location both IO shapes agreed on.
            let clips = match geometry.is_empty() {
                true => (
                    format!("vertex.{}", abi::CURRENT_CLIP_FIELD),
                    format!("vertex.{}", abi::PREVIOUS_CLIP_FIELD),
                ),
                false => (
                    format!("extra.{}", abi::CURRENT_CLIP_FIELD),
                    format!("extra.{}", abi::PREVIOUS_CLIP_FIELD),
                ),
            };
            format!(
                "-> @location(0) vec2f {{\n{prologue}    \
                 return {motion}({current}, {previous});\n}}\n",
                motion = abi::SCREEN_MOTION_FN,
                current = clips.0,
                previous = clips.1,
            )
        }
        // Reached only when the material discards: a depth or shadow
        // stage whose whole fragment program is the alpha test.
        abi::StageOutput::Nothing => format!("{{\n{prologue}}}\n"),
    };
    let _ = write!(
        out,
        "\n@fragment\nfn {fragment}(vertex: {vertex_out}{params}) {body}",
        // The velocity stage's own IO shape when the material declares no
        // geometry: `VelocityOut`, which carries the previous clip the
        // body above reads. With declared attributes it is the plain
        // `VertexOut` plus `extra`, exactly as every other stage sees.
        vertex_out = match velocity && geometry.is_empty() {
            true => abi::VELOCITY_OUT_STRUCT,
            false => abi::VERTEX_OUT_STRUCT,
        },
    );
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

/// Header the generated macro module starts with.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use crate::macros::{MacroDef, MacroValue};
    use crate::node::{GenericParam, Socket, ValueType, WxslFunction};

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register(abi::surface_output_def());
        registry.register_all(abi::context_node_defs());
        registry.register_all([
            NodeDefinition::builder("math.multiply.f32", "Multiply")
                .input(Socket::new("a", ValueType::F32).with_splat_default(1.0))
                .input(Socket::new("b", ValueType::F32).with_splat_default(1.0))
                .output(Socket::new("out", ValueType::F32))
                .expr("{a} * {b}"),
            NodeDefinition::builder("math.negate.f32", "Negate")
                .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
                .output(Socket::new("out", ValueType::F32))
                .expr("-{a}"),
            NodeDefinition::builder("test.macro_user", "Macro user")
                .macro_var(MacroDef::new(
                    "WXSL_TEST_SCALE",
                    MacroValue::Float(2.0),
                    "test scale",
                ))
                .macro_var(MacroDef::new(
                    "wxsl_test_flag",
                    MacroValue::Flag(false),
                    "test flag",
                ))
                .input(Socket::new("a", ValueType::F32).with_splat_default(1.0))
                .output(Socket::new("out", ValueType::F32))
                .expr("{a} * WXSL_TEST_SCALE"),
            NodeDefinition::builder("test.generic_add", "Generic add")
                .generic_param(crate::node::GenericParam::new(
                    "T",
                    vec![ValueType::F32, ValueType::Vec3],
                ))
                .input(Socket::new("a", ValueType::F32).generic("T"))
                .input(Socket::new("b", ValueType::F32).generic("T"))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .expr("{a} + {b}"),
            NodeDefinition::builder("test.split", "Split").call(WxslFunction::new_struct(
                "package::test",
                "split_value",
                vec![Socket::new("value", ValueType::F32).with_splat_default(0.0)],
                "SplitResult",
                vec![
                    Socket::new("low", ValueType::F32),
                    Socket::new("high", ValueType::F32),
                ],
            )),
        ]);
        registry
    }

    fn generate_default(graph: &Graph, registry: &NodeRegistry) -> GeneratedShader {
        generate(graph, registry, &CodegenOptions::default()).expect("codegen succeeds")
    }

    /// A value both stages read: computed per vertex, handed to the
    /// fragment stage as a synthesized interpolant — the whole of plan2
    /// P9 in one graph.
    #[test]
    fn a_shared_node_is_computed_in_the_vertex_stage_and_interpolated_down() {
        let mut registry = NodeRegistry::new();
        registry.register(abi::surface_output_def());
        registry.register(abi::vertex_output_def());
        registry.register_all(abi::context_node_defs());
        registry.register_all([NodeDefinition::builder("test.vec3", "Vec3")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.5))
            .output(Socket::new("out", ValueType::Vec3))
            .expr("vec3f({a})")]);
        let mut graph = Graph::new("cut");
        let value = graph.add(Node::new("test.vec3"));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(&registry, (value, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        graph
            .wire(
                &registry,
                (value, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .expect("vec3 into the offset");

        let shader = generate_default(&graph, &registry);
        // The cut is a real interpolant: declared on the interface,
        // computed by a vertex-stage function of its own, written by the
        // vertex entry, and read by the material function through the
        // attributes struct.
        assert!(
            shader
                .source
                .contains("fn wxsl_varying_auto0(vtx: VertexContext"),
            "{})",
            shader.source
        );
        assert!(
            shader
                .source
                .contains("out.auto0 = wxsl_varying_auto0(vtx, attrs);"),
            "{}",
            shader.source
        );
        assert!(shader.source.contains("attrs.auto0"), "{}", shader.source);
        // And the fragment partition stops at the cut: the shared node's
        // expression appears once, in the vertex function, not again in
        // the material function.
        // The material function assigns the surface field straight from
        // the interpolant read — it does not re-evaluate the node.
        assert!(
            shader.source.contains("surface.base_color = attrs.auto0;"),
            "{}",
            shader.source
        );

        // A stage with no fragment program computes no cut — there is
        // nothing to hand the value to.
        let options = CodegenOptions {
            stage: abi::MaterialStage::DEPTH_ONLY,
            ..CodegenOptions::default()
        };
        let depth = generate(&graph, &registry, &options).expect("depth compiles");
        assert!(
            !depth.source.contains("wxsl_varying_auto0"),
            "the depth module should not compute the cut:\n{}",
            depth.source
        );
    }

    #[test]
    fn a_bare_output_node_still_compiles_to_a_material() {
        let registry = registry();
        let mut graph = Graph::new("bare");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let shader = generate_default(&graph, &registry);

        assert!(shader
            .source
            .contains("fn wxsl_material(ctx: SurfaceContext) -> Surface"));
        assert!(shader
            .source
            .contains("var surface: Surface = default_surface(ctx);"));
        // `normal` has no literal default, so it keeps what default_surface set.
        assert!(!shader.source.contains("surface.normal ="));
        assert!(shader
            .source
            .contains("surface.base_color = vec3f(0.8, 0.8, 0.8);"));
        // One stage, one fragment entry. The two `@if`s are the lighting
        // generator's debug-normals arm — the knobs the ABI honours are
        // conditional-translation features, and the pasted shading
        // function declares and switches them.
        assert!(shader
            .source
            .contains("fn fs_forward_lit(vertex: VertexOut)"));
        assert_eq!(shader.source.matches("@fragment").count(), 1);
        assert_eq!(shader.source.matches("@if(wxsl_debug_normals)").count(), 1);
        assert_eq!(shader.source.matches("@if(!wxsl_debug_normals)").count(), 1);
    }

    #[test]
    fn the_velocity_stage_transforms_every_vertex_twice_and_shades_nothing() {
        // The stage's whole shape: a vertex entry whose IO carries both
        // frames' clip positions, a fragment that is their difference in
        // uv units, and no surface anywhere — the material function and
        // the shading function are not in this module at all, which is
        // the velocity stage's share of the partitioning saving
        // (ADR 0025).
        let registry = registry();
        let mut graph = Graph::new("plain");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let options = CodegenOptions {
            stage: abi::MaterialStage::VELOCITY,
            ..CodegenOptions::default()
        };
        let source = generate(&graph, &registry, &options)
            .expect("compiles")
            .source;
        assert!(
            source
                .contains("fn vs_main(input: VertexIn) -> VelocityOut {\n    return transform_vertex_velocity(input, vec3f(0.0), vec3f(0.0));"),
            "the undismayed vertex entry passes both zero offsets: {source}"
        );
        assert!(
            source.contains(
                "fn fs_velocity(vertex: VelocityOut) -> @location(0) vec2f {\n    \
                 return screen_motion(vertex.current_clip, vertex.previous_clip);"
            ),
            "the fragment is the difference of the two interpolated clips: {source}"
        );
        assert!(!source.contains(abi::SHADE_SURFACE_FN), "{source}");
        assert!(!source.contains(abi::PACK_GBUFFER_FN), "{source}");
        assert!(
            source.contains("import package::wxsl::velocity::"),
            "the whole velocity ABI comes from its one module: {source}"
        );
    }

    #[test]
    fn a_time_driven_displacement_answers_for_both_frames() {
        // The graph half M5 settled: the vertex partition is evaluated
        // twice — once against this frame's context, once against
        // `previous_vertex_context` — by the same emitted function, so a
        // displacement driven by `input.time` is where it *was* last
        // frame without the graph knowing there is a previous frame.
        let mut registry = registry();
        registry.register(abi::vertex_output_def());
        registry.register_all([NodeDefinition::builder("test.vec3", "To vec3")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::Vec3))
            .expr("vec3f({a})")]);
        let registry = registry;
        let mut graph = Graph::new("displaced");
        let term = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
        let widened = graph.add(Node::new("test.vec3"));
        let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(&registry, (term, "out"), (out, "roughness"))
            .unwrap();
        graph
            .wire(&registry, (term, "out"), (widened, "a"))
            .unwrap();
        graph
            .wire(
                &registry,
                (widened, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .unwrap();
        let options = CodegenOptions {
            stage: abi::MaterialStage::VELOCITY,
            ..CodegenOptions::default()
        };
        let source = generate(&graph, &registry, &options)
            .expect("compiles")
            .source;
        assert!(
            source.contains(
                "transform_vertex_velocity(input, wxsl_vertex(vtx, attrs), \
                 wxsl_vertex(previous_vertex_context(input), attrs))"
            ),
            "both offsets come from the one partition, twice-contexted: {source}"
        );
    }

    #[test]
    fn the_previous_clip_rides_after_a_materials_own_extras() {
        // With declared attributes the velocity stage's extra varying
        // takes the first location the material's own did not, in both IO
        // structs — no interface location moves, and the fragment reads
        // it from `extra` like any other declared extra.
        let mut registry = registry();
        registry.register(
            NodeDefinition::builder("input.attribute", "Attribute")
                .setting(crate::node::SettingDef::new(
                    node::SETTING_NAME,
                    "name",
                    "Which attribute.",
                ))
                .generic_param(crate::node::GenericParam::new("T", ValueType::ALL.to_vec()))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .declaration(NodeBody::AttributeRead),
        );
        let registry = registry;
        let mut graph = Graph::new("with attributes");
        graph.declare_attribute(crate::graph::AttributeDecl::vertex(
            "weight",
            ValueType::F32,
        ));
        let weight = graph.add(Node::new("input.attribute").with_setting("name", "weight"));
        let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (weight, "out"), (out, "roughness"))
            .unwrap();
        let options = CodegenOptions {
            stage: abi::MaterialStage::VELOCITY,
            ..CodegenOptions::default()
        };
        let source = generate(&graph, &registry, &options)
            .expect("compiles")
            .source;
        let first = abi::VERTEX_OUT_FIELDS.len() + 1;
        for (location, field) in [
            (first, abi::CURRENT_CLIP_FIELD),
            (first + 1, abi::PREVIOUS_CLIP_FIELD),
        ] {
            let line = format!("@location({location}) {field}: vec4f,");
            assert_eq!(
                source.matches(&line).count(),
                2,
                "`{field}` rides in the vertex output struct and in the \
                 fragment's extras, at its own location:\n{source}"
            );
        }
        assert!(
            source.contains(&format!(
                "out.{} = base.{};",
                abi::CURRENT_CLIP_FIELD,
                abi::CURRENT_CLIP_FIELD
            )) && source.contains(&format!(
                "out.{} = base.{};",
                abi::PREVIOUS_CLIP_FIELD,
                abi::PREVIOUS_CLIP_FIELD
            )),
            "the vertex entry copies both like any other field: {source}"
        );
        assert!(
            source.contains(&format!(
                "return screen_motion(extra.{}, extra.{});",
                abi::CURRENT_CLIP_FIELD,
                abi::PREVIOUS_CLIP_FIELD
            )),
            "the fragment reads both from `extra`: {source}"
        );
    }

    #[test]
    fn each_stage_emits_its_own_entry_point_and_imports_only_what_it_calls() {
        let registry = registry();
        let mut graph = Graph::new("stages");
        graph.add_node(abi::SURFACE_OUTPUT_ID);

        let module = |stage: abi::MaterialStage| {
            let options = CodegenOptions {
                stage,
                ..CodegenOptions::default()
            };
            generate(&graph, &registry, &options)
                .expect("compiles")
                .source
        };

        let forward = module(abi::MaterialStage::FORWARD_LIT);
        assert!(forward.contains("fn fs_forward_lit(vertex: VertexOut) -> @location(0) vec4f"));
        assert!(forward.contains("shade_surface"));
        assert!(!forward.contains("pack_gbuffer"));

        let gbuffer = module(abi::MaterialStage::GBUFFER);
        assert!(gbuffer.contains("fn fs_gbuffer(vertex: VertexOut) -> GBuffer"));
        assert!(gbuffer.contains("pack_gbuffer"));
        assert!(!gbuffer.contains("shade_surface"));

        // Depth only: a vertex entry and *nothing else*. Not the
        // shading function, not the G-buffer, and — since this graph
        // neither displaces nor discards — not the material function
        // either, nor any fragment stage at all
        // ([ADR 0025](../../../docs/adr/0025-a-material-graph-spans-shader-stages.md)).
        let depth = module(abi::MaterialStage::DEPTH_ONLY);
        assert!(depth.contains("fn vs_main(input: VertexIn) -> VertexOut"));
        assert!(!depth.contains("@fragment"));
        assert!(!depth.contains("shade_surface"));
        assert!(!depth.contains("pack_gbuffer"));
        assert!(!depth.contains("fn wxsl_material"));
        // The shadow stage is the same shape, from a light's point of view.
        let shadow = module(abi::MaterialStage::SHADOW);
        assert!(!shadow.contains("fn wxsl_material"));

        // Four stages, four different modules — which is what lets the
        // variant cache tell them apart by source hash alone. Depth and
        // shadow differ only in the stage comment their macros carry,
        // which is enough and is why the key holds the stage too.
        for pair in [(&forward, &gbuffer), (&forward, &depth), (&gbuffer, &depth)] {
            assert_ne!(pair.0, pair.1);
        }
    }

    #[test]
    fn nodes_are_emitted_in_dependency_order_with_typed_bindings() {
        let registry = registry();
        let mut graph = Graph::new("chain");
        let uv = graph.add_node("input.uv");
        let time = graph.add_node("input.time");
        let scaled = graph.add(Node::new("math.multiply.f32").with_param("b", Value::F32(3.0)));
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph.wire(&registry, (time, "out"), (scaled, "a")).unwrap();
        graph
            .wire(&registry, (scaled, "out"), (out, "roughness"))
            .unwrap();
        // `uv` is wired to nothing: it must not appear in the output.
        let _ = uv;

        let shader = generate_default(&graph, &registry);
        assert!(
            shader.source.contains("let n3_out: f32 = ctx.time * 3.0;"),
            "{}",
            shader.source
        );
        assert!(shader.source.contains("surface.roughness = n3_out;"));
        assert!(!shader.source.contains("ctx.uv"), "dead nodes are dropped");
    }

    #[test]
    fn a_type_placeholder_emits_the_resolved_type_name() {
        // `{$T}` is how a template that has to *name* its type stays one
        // template: `convert.splat`'s constructor, or a `select` fallback of
        // the right width.
        let mut registry = registry();
        registry.register(
            NodeDefinition::builder("test.splat", "Splat")
                .generic_param(GenericParam::new("T", vec![ValueType::Vec3]))
                .input(Socket::new("value", ValueType::F32).with_splat_default(0.5))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .expr("{$T}({value})"),
        );
        let mut graph = Graph::new("splat");
        let splat = graph.add_node("test.splat");
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&registry, (splat, "out"), (out, "base_color"))
            .expect("T resolves to vec3f");

        let shader = generate_default(&graph, &registry);
        assert!(
            shader.source.contains("let n1_out: vec3f = vec3f(0.5);"),
            "{}",
            shader.source
        );
    }

    #[test]
    fn a_generic_function_node_writes_its_type_arguments() {
        // A generic `NodeBody::Call` node calls a WXSL *template*, and the
        // graph always knows the type exactly, so the type argument is
        // written rather than left to inference (ADR 0012).
        let mut registry = registry();
        registry.register(
            NodeDefinition::builder("test.generic_call", "Generic call")
                .generic_param(GenericParam::new(
                    "T",
                    vec![ValueType::Vec3, ValueType::Vec4],
                ))
                .call(WxslFunction::new(
                    "package::test::identity",
                    "identity",
                    vec![Socket::new("v", ValueType::F32)
                        .generic("T")
                        .with_splat_default(1.0)],
                    Socket::new("out", ValueType::F32).generic("T"),
                )),
        );
        let mut graph = Graph::new("call");
        let call = graph.add_node("test.generic_call");
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&registry, (call, "out"), (out, "base_color"))
            .expect("T resolves to vec3f");

        let shader = generate_default(&graph, &registry);
        assert!(
            shader
                .source
                .contains("let n1_call: vec3f = identity<vec3f>(vec3f(1.0, 1.0, 1.0));"),
            "{}",
            shader.source
        );
    }

    #[test]
    fn a_generic_node_emits_its_resolved_type_not_the_placeholder() {
        let registry = registry();

        // Resolved to vec3f, via a connection.
        let mut vec_graph = Graph::new("vec");
        let color = vec_graph.add_node("input.world_position");
        let add = vec_graph
            .add(Node::new("test.generic_add").with_param("b", Value::Vec3([1.0, 2.0, 3.0])));
        let out = vec_graph.add_node(abi::SURFACE_OUTPUT_ID);
        vec_graph
            .wire(&registry, (color, "out"), (add, "a"))
            .unwrap();
        vec_graph
            .wire(&registry, (add, "out"), (out, "base_color"))
            .unwrap();
        let shader = generate_default(&vec_graph, &registry);
        assert!(
            shader.source.contains(": vec3f = ") && shader.source.contains(" + vec3f("),
            "expected a vec3f binding and a vec3f literal, got:\n{}",
            shader.source
        );
        assert!(!shader.source.contains(": f32 ="), "{}", shader.source);

        // The *same node definition*, resolved to f32 instead, in a graph
        // of its own — one registry entry serving two concrete types.
        let mut scalar_graph = Graph::new("scalar");
        let add = scalar_graph.add(
            Node::new("test.generic_add")
                .with_param("a", Value::F32(1.0))
                .with_param("b", Value::F32(2.0)),
        );
        scalar_graph
            .set_generic(&registry, add, "T", ValueType::F32)
            .expect("f32 is allowed");
        let out = scalar_graph.add_node(abi::SURFACE_OUTPUT_ID);
        scalar_graph
            .wire(&registry, (add, "out"), (out, "roughness"))
            .unwrap();
        let shader = generate_default(&scalar_graph, &registry);
        assert!(
            shader.source.contains(": f32 = 1.0 + 2.0;"),
            "{}",
            shader.source
        );
    }

    #[test]
    fn atoms_are_recognized_and_everything_else_is_wrapped() {
        for atom in [
            "n1_out",
            "ctx.world_normal",
            "0.5",
            "vec3f(1.0, 0.0, 0.0)",
            "f(g(x))",
        ] {
            assert_eq!(as_atom(atom), atom, "`{atom}` should not be wrapped");
        }
        for compound in ["-1.0", "a + b", "f(x) * 2.0", "(a)", ""] {
            assert_eq!(as_atom(compound), format!("({compound})"));
        }
    }

    #[test]
    fn substituted_expressions_are_parenthesized_when_needed() {
        let registry = registry();
        let mut graph = Graph::new("negate");
        let negate = graph.add(Node::new("math.negate.f32").with_param("a", Value::F32(-1.0)));
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&registry, (negate, "out"), (out, "metallic"))
            .unwrap();

        let shader = generate_default(&graph, &registry);
        assert!(
            shader.source.contains("let n1_out: f32 = -(-1.0);"),
            "{}",
            shader.source
        );
    }

    #[test]
    fn struct_returning_functions_are_called_once_for_all_outputs() {
        let registry = registry();
        let mut graph = Graph::new("split");
        let split = graph.add(Node::new("test.split").with_param("value", Value::F32(0.5)));
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&registry, (split, "low"), (out, "metallic"))
            .unwrap();
        graph
            .wire(&registry, (split, "high"), (out, "roughness"))
            .unwrap();

        let shader = generate_default(&graph, &registry);
        assert_eq!(shader.source.matches("split_value(").count(), 1);
        assert!(shader.source.contains("surface.metallic = n1_call.low;"));
        assert!(shader.source.contains("surface.roughness = n1_call.high;"));
        assert!(shader
            .source
            .contains("import package::test::{SplitResult, split_value};"));
    }

    #[test]
    fn macros_reach_the_shader_as_consts_and_features() {
        let registry = registry();
        let mut graph = Graph::new("macros");
        let user = graph.add_node("test.macro_user");
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&registry, (user, "out"), (out, "roughness"))
            .unwrap();

        // Declared defaults apply until the graph pins something else.
        let shader = generate_default(&graph, &registry);
        assert_eq!(
            shader.macros.get("WXSL_TEST_SCALE"),
            Some(MacroValue::Float(2.0))
        );
        assert_eq!(
            shader.macros.get("wxsl_test_flag"),
            Some(MacroValue::Flag(false))
        );
        // Both kinds are *declared* in the generated module now, with their
        // effective values as defaults: a WXSL module declares the knobs it
        // uses, so there is no shared macro module to import from
        // (ADR 0011). One concept covers flags and numbers alike.
        assert!(
            shader
                .source
                .contains("@macro const WXSL_TEST_SCALE: f32 = 2.0;"),
            "{}",
            shader.source
        );
        assert!(
            shader
                .source
                .contains("@macro const wxsl_test_flag: bool = false;"),
            "{}",
            shader.source
        );
        assert!(!shader.source.contains("package::wxsl::macros"));

        graph.set_macro("WXSL_TEST_SCALE", MacroValue::Float(8.0));
        graph.set_macro("wxsl_test_flag", MacroValue::Flag(true));
        let pinned = generate_default(&graph, &registry);
        assert!(
            pinned
                .source
                .contains("@macro const WXSL_TEST_SCALE: f32 = 8.0;"),
            "{}",
            pinned.source
        );
        assert_eq!(
            pinned.macros.get("wxsl_test_flag"),
            Some(MacroValue::Flag(true))
        );

        // Changing a macro now changes the root module itself, because the
        // values are declared in it: the header comment and one declaration
        // per changed macro. Under the old design the root was byte-identical
        // and only the separate macro module differed, which is why
        // `variant_key` had to fold in the macro set rather than hash the
        // source.
        let differing: Vec<(&str, &str)> = shader
            .source
            .lines()
            .zip(pinned.source.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(differing.len(), 3, "{differing:?}");
        assert!(differing[0].0.starts_with("// Macros:"));
        assert!(differing[1].0.contains("WXSL_TEST_SCALE: f32 = 2.0"));
        assert!(differing[2].0.contains("wxsl_test_flag: bool = false"));

        // It still folds in the macro set, and still has to: the bindings
        // also reach *imported* modules, whose own `@macro const` defaults
        // are nowhere in this source.
        assert_ne!(shader.variant_key(), pinned.variant_key());
    }

    #[test]
    fn a_graph_with_no_output_node_is_an_error() {
        let registry = registry();
        let mut graph = Graph::new("headless");
        graph.add_node("input.uv");
        assert_eq!(
            generate(&graph, &registry, &CodegenOptions::default()),
            Err(CodegenError::NoOutputNode)
        );

        // Two of them is now reported by validation, which runs first
        // and reports every duplicated terminal under one rule.
        // `MultipleOutputNodes` stays as the belt-and-braces path for a
        // caller that reached codegen without validating.
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let Err(CodegenError::Invalid(errors)) =
            generate(&graph, &registry, &CodegenOptions::default())
        else {
            panic!("two surface outputs is an error");
        };
        assert!(
            errors.0.iter().any(
                |error| matches!(error, GraphError::DuplicateOutput { nodes, .. }
                    if nodes.len() == 2)
            ),
            "{errors:?}"
        );
    }

    #[test]
    fn generation_is_reproducible() {
        let registry = registry();
        let mut graph = Graph::new("repro");
        let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
        let time = graph.add_node("input.time");
        graph
            .wire(&registry, (time, "out"), (out, "roughness"))
            .unwrap();
        let first = generate_default(&graph, &registry);
        let second = generate_default(&graph, &registry);
        assert_eq!(first, second);
        assert_eq!(first.variant_key(), second.variant_key());
    }

    #[test]
    fn entry_points_can_be_left_out() {
        let registry = registry();
        let mut graph = Graph::new("material-only");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let options = CodegenOptions {
            emit_entry_points: false,
            ..CodegenOptions::default()
        };
        let shader = generate(&graph, &registry, &options).unwrap();
        assert!(!shader.source.contains("@vertex"));
        assert!(!shader.source.contains("@fragment"));
        assert!(shader.source.contains("fn wxsl_material"));
    }

    // -- bakes (ADR 0045) -----------------------------------------------

    use crate::graph::BakeDecl;

    /// A material whose roughness is an expensive term: two chained
    /// multiplies the bake will stand in for. The cone is deliberately
    /// constant — purity is about *what* the cone may read, and the
    /// no-context case is the simplest one that exercises the machinery.
    fn bake_graph(registry: &NodeRegistry) -> (Graph, NodeId) {
        let mut graph = Graph::new("baked");
        let expensive = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
        let more = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.5)));
        let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(registry, (expensive, "out"), (more, "b"))
            .unwrap();
        graph
            .wire(registry, (more, "out"), (out, "roughness"))
            .unwrap();
        graph.declare_bake(BakeDecl {
            node: more,
            socket: None,
            texture: "roughness_bake".to_string(),
            size: [64, 64],
            precision: crate::abi::GBufferPrecision::HighDynamicRange,
        });
        (graph, more)
    }

    #[test]
    fn a_baked_term_is_one_sample_and_its_cone_leaves_the_module() {
        let registry = registry();
        let (graph, _) = bake_graph(&registry);

        // Baked: one filtered sample stands in for the term, and the cone
        // is not in the module at all — the cost change the toggle sells.
        let baked = generate_default(&graph, &registry);
        assert!(
            baked.source.contains(
                "surface.roughness = textureSample(roughness_bake, \
                 roughness_bake_sampler, ctx.uv).x;",
            ),
            "{}",
            baked.source
        );
        // The cone's multiplies are gone from the fragment side: neither
        // the cone node's binding nor the baked node's own appears.
        assert!(
            !baked.source.contains("surface.roughness = (n"),
            "{}",
            baked.source
        );
        assert!(!baked.source.contains("let n1_out"), "{}", baked.source);
        assert!(!baked.source.contains("let n2_out"), "{}", baked.source);
        // The declaration turned into two bindings — the table and its
        // sampler — at material-group bindings.
        assert!(
            baked
                .source
                .contains("@group(1) @binding(1) var roughness_bake: texture_2d<f32>;"),
            "{}",
            baked.source
        );
        assert!(
            baked
                .source
                .contains("@group(1) @binding(2) var roughness_bake_sampler: sampler;"),
            "{}",
            baked.source
        );
        assert_eq!(baked.interface.resources.len(), 2);
        assert!(baked.interface.resources[0].bake.is_some());

        // Inline: the same graph with the toggle off emits the cone and no
        // sample — and the two module sources differ, which is what gives
        // the variant cache two shaders.
        let inline = generate(
            &graph,
            &registry,
            &CodegenOptions {
                material: crate::material::ResolvedMaterialConfig {
                    bakes: false,
                    ..crate::material::ResolvedMaterialConfig::default()
                },
                ..CodegenOptions::default()
            },
        )
        .unwrap();
        assert!(
            !inline.source.contains("textureSample("),
            "{}",
            inline.source
        );
        // The cone is back: both multiplies emitted into the material.
        assert!(inline.source.contains("let n1_out"), "{}", inline.source);
        assert!(inline.source.contains("let n2_out"), "{}", inline.source);
        assert_ne!(baked.source, inline.source);
    }

    #[test]
    fn a_bake_generates_the_dispatch_that_fills_its_table() {
        let registry = registry();
        let (graph, more) = bake_graph(&registry);
        let decl = graph.bake("roughness_bake").unwrap();
        let generated = generate_bake(&graph, decl, &registry, &BakeOptions::default()).unwrap();

        assert_eq!(generated.entry, abi::BAKE_ENTRY);
        assert_eq!(generated.workgroups, [8, 8, 1]);
        assert_eq!(generated.size, [64, 64]);
        assert!(
            generated.source.contains(
                "@group(3) @binding(0) var wxsl_bake_target: \
                           texture_storage_2d<rgba16float, write>;"
            ),
            "{}",
            generated.source
        );
        assert!(
            generated
                .source
                .contains("fn wxsl_bake_value(uv: vec2f) -> vec4f {"),
            "{}",
            generated.source
        );
        // The cone, computed inside the value function...
        assert!(
            generated.source.contains("0.5 * n1_out"),
            "{}",
            generated.source
        );
        // ...padded to the table's four channels...
        assert!(
            generated
                .source
                .contains("return vec4f(n2_out, 0.0, 0.0, 1.0);"),
            "{}",
            generated.source
        );
        // ...and dispatched, guarded by the table's own extent.
        assert!(
            generated.source.contains("if (id.x >= 64u || id.y >= 64u)"),
            "{}",
            generated.source
        );
        assert!(
            generated
                .source
                .contains("textureStore(wxsl_bake_target, vec2i(id.xy), wxsl_bake_value(uv));"),
            "{}",
            generated.source
        );

        // The declaration is addressable by the table's name, which is how
        // `Effect::from_bake` finds it.
        assert_eq!(decl.node, more);
    }

    #[test]
    fn a_cone_that_reads_more_than_the_domain_is_refused_by_name() {
        let mut registry = registry();
        registry.register_all([NodeDefinition::builder("test.param", "Parameter")
            .setting(crate::node::SettingDef::new(
                node::SETTING_NAME,
                "Name",
                "The uniform field.",
            ))
            .input(Socket::new("value", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::F32))
            .declaration(NodeBody::Param)]);
        let mut graph = Graph::new("impure");
        let param = graph.add(Node::new("test.param").with_param("value", Value::F32(3.0)));
        graph.set_setting(param, node::SETTING_NAME, "wobble");
        let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (param, "out"), (out, "roughness"))
            .unwrap();
        graph.declare_bake(BakeDecl {
            node: param,
            socket: None,
            texture: "roughness_bake".to_string(),
            size: [64, 64],
            precision: crate::abi::GBufferPrecision::HighDynamicRange,
        });
        let error = generate_bake(
            &graph,
            graph.bake("roughness_bake").unwrap(),
            &registry,
            &BakeOptions::default(),
        )
        .expect_err("a bake over a material parameter is not a function of the domain");
        let message = error.to_string();
        assert!(
            message.contains("roughness_bake") && message.contains("material parameter"),
            "{message}"
        );
    }

    #[test]
    fn a_cone_shared_outside_the_bake_is_refused() {
        let registry = registry();
        let mut graph = Graph::new("shared");
        let expensive = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
        let baked = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.5)));
        let other = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.75)));
        let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (expensive, "out"), (baked, "b"))
            .unwrap();
        graph
            .wire(&registry, (expensive, "out"), (other, "b"))
            .unwrap();
        graph
            .wire(&registry, (baked, "out"), (out, "roughness"))
            .unwrap();
        graph
            .wire(&registry, (other, "out"), (out, "metallic"))
            .unwrap();
        graph.declare_bake(BakeDecl {
            node: baked,
            socket: None,
            texture: "roughness_bake".to_string(),
            size: [64, 64],
            precision: crate::abi::GBufferPrecision::HighDynamicRange,
        });
        let error = generate(&graph, &registry, &CodegenOptions::default())
            .expect_err("the cone feeds `metallic` too, and the bake would go dark");
        let message = error.to_string();
        assert!(
            message.contains("roughness_bake") && message.contains("also feeds"),
            "{message}"
        );
    }

    #[test]
    fn a_baked_node_the_vertex_stage_reaches_is_refused() {
        let mut registry = registry();
        registry.register(abi::vertex_output_def());
        registry.register_all([NodeDefinition::builder("test.vec3", "To vec3")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::Vec3))
            .expr("vec3f({a})")]);
        let registry = registry;
        let mut graph = Graph::new("vertex");
        let term = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
        let widened = graph.add(Node::new("test.vec3"));
        let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(&registry, (term, "out"), (out, "roughness"))
            .unwrap();
        graph
            .wire(&registry, (term, "out"), (widened, "a"))
            .unwrap();
        graph
            .wire(
                &registry,
                (widened, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .unwrap();
        graph.declare_bake(BakeDecl {
            node: term,
            socket: None,
            texture: "roughness_bake".to_string(),
            size: [64, 64],
            precision: crate::abi::GBufferPrecision::HighDynamicRange,
        });
        let error = generate(&graph, &registry, &CodegenOptions::default())
            .expect_err("textureSample does not run in the vertex stage");
        assert!(error.to_string().contains("vertex"), "{error}");
    }
}
