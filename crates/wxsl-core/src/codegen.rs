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

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::abi;
use crate::error::{CodegenError, GraphError, GraphErrors};
use crate::graph::{Graph, NodeId, ShaderStage, SocketRef};
use crate::macros::{MacroSet, MacroValue};
use crate::node::{self, FunctionReturn, NodeBody, NodeDefinition, NodeRegistry, Value};
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
    /// Macro values to sit *above* the graph's own, overriding what it pins.
    ///
    /// For values the application decides rather than the graph author: a
    /// runtime toggle, a quality setting, a debug view. Keeping them here
    /// rather than writing them into the graph means the graph still holds
    /// what it was authored with.
    pub override_macros: MacroSet,
    /// Which lighting model shades this material, and which set the
    /// surrounding pipeline enables
    /// ([`crate::lighting`]).
    ///
    /// The set decides whether the G-buffer stage writes a dispatch id and
    /// which targets the G-buffer struct carries; the model decides which
    /// function the generated shading calls. The default is the library's
    /// default model in a set of one, which needs neither.
    pub lighting: crate::lighting::MaterialLighting,
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
            override_macros: MacroSet::new(),
            lighting: crate::lighting::MaterialLighting::default(),
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
    macros.overlay(&graph.effective_macros(registry)?);
    macros.overlay(&options.override_macros);

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
    let mut emitter = Emitter {
        graph,
        registry,
        options,
        interface,
        stage: ShaderStage::Fragment,
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
    let has_fragment = stage.needs_surface() || outputs.discard.is_some();
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
        let stops: std::collections::BTreeSet<NodeId> =
            cuts.keys().map(|socket| socket.node).collect();
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
                abi::StageOutput::Color => {
                    // The dispatch and the whole shading function come from
                    // the material's lighting model, generated here rather
                    // than imported from a fixed module: the call inside
                    // the light loop names the model.
                    let generated = crate::lighting::shade_surface_with(
                        &crate::lighting::Dispatch::Direct(*self.options.lighting.model()),
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
                    // The struct's fields are the set's layout, so the pack
                    // is generated beside it instead of imported from a
                    // fixed module.
                    let generated = crate::lighting::pack_gbuffer(
                        self.options.lighting.model(),
                        self.options.lighting.set(),
                    );
                    for (module, item) in &generated.imports {
                        self.request_import(module, item);
                    }
                    self.lighting_source = generated.source;
                }
                abi::StageOutput::Nothing => {}
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
            | NodeBody::VaryingOutput => {
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

        for (module, items) in &imports {
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

        // The macros this shader was compiled with, declared with their
        // effective values as defaults. A WXSL module declares the knobs it
        // uses (ADR 0011), so there is no shared macro module to import
        // from: the generated module is self-contained, and the renderer
        // binds the same values over the top.
        // There is no render-path flag any more: a stage is chosen by
        // generating that stage's module, not by an `@if` inside a module
        // holding all of them (ADR 0022).
        if !macros.is_empty() {
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

/// The IO structs a material with declared attributes needs, beside the
/// ABI's own.
fn write_geometry_io(out: &mut String, geometry: &GeometryInterface) {
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
    out.push_str("}\n");

    // The fragment side takes two parameters instead, so the ABI's own
    // `VertexOut` still arrives unchanged and `surface_context` needs no
    // widening at all.
    let _ = writeln!(out, "struct {} {{", abi::MATERIAL_VARYINGS_STRUCT);
    out.push_str(&extra_varyings(geometry));
    out.push_str("}\n");
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
    write_geometry_io(out, geometry);
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
    if geometry.is_empty() {
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
            vertex_out = abi::VERTEX_OUT_STRUCT,
            call = transform(""),
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
            call = transform(&format!(", {}", abi::MATERIAL_ATTRIBUTES_VAR)),
        );
        let _ = writeln!(
            out,
            "    out.{field} = base.{field};",
            field = abi::CLIP_POSITION_FIELD
        );
        for field in abi::VERTEX_OUT_FIELDS {
            let _ = writeln!(out, "    out.{name} = base.{name};", name = field.name);
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
    if !stage.needs_surface() && parts.discard.is_none() {
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
    let prologue = format!(
        "    let {ctx} = {context}(vertex);\n{unpack}{test}",
        ctx = abi::CONTEXT_VAR,
        context = abi::SURFACE_CONTEXT_FN,
    );
    let body = match stage.output() {
        abi::StageOutput::Color => format!(
            "-> @location(0) vec4f {{\n{prologue}    \
             return {shade}({material}({ctx}{args}), {ctx});\n}}\n",
            ctx = abi::CONTEXT_VAR,
            shade = abi::SHADE_SURFACE_FN,
            material = options.material_fn,
        ),
        abi::StageOutput::GBuffer => format!(
            "-> {gbuffer} {{\n{prologue}    \
             return {pack}({material}({ctx}{args}){id});\n}}\n",
            ctx = abi::CONTEXT_VAR,
            gbuffer = abi::GBUFFER_STRUCT,
            material = options.material_fn,
            pack = abi::PACK_GBUFFER_FN,
            id = if options.lighting.set().dispatches() {
                format!(", {}u", options.lighting.model)
            } else {
                String::new()
            },
        ),
        // Reached only when the material discards: a depth or shadow
        // stage whose whole fragment program is the alpha test.
        abi::StageOutput::Nothing => format!("{{\n{prologue}}}\n"),
    };
    let _ = write!(
        out,
        "\n@fragment\nfn {fragment}(vertex: {vertex_out}{params}) {body}",
        vertex_out = abi::VERTEX_OUT_STRUCT,
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
}
