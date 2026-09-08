//! Turns a validated graph into WXSL source, ready for the WXSL compiler to
//! resolve and lower to WGSL.
//!
//! The emitted module has four parts:
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
//! 3. **The material function** — one `let` per node output, in dependency
//!    order, ending in the [`crate::abi::SURFACE_STRUCT`] the graph produces.
//! 4. **Entry points** — a vertex entry shared by both render paths, and two
//!    `@if`-gated fragment entries: one that shades to a colour (forward) and
//!    one that writes a G-buffer (deferred). One module, compiled once per
//!    path, is exactly what
//!    [ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)
//!    asks for — the graph author writes no path-specific nodes.
//!
//! Only the nodes the output node actually depends on are emitted, so a
//! half-finished branch parked on the editor canvas costs nothing.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::abi;
use crate::error::{CodegenError, GraphError, GraphErrors};
use crate::graph::{Graph, NodeId, SocketRef};
use crate::macros::{MacroSet, MacroValue};
use crate::node::{FunctionReturn, NodeBody, NodeDefinition, NodeRegistry, Value};
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
    /// Name of the generated fragment entry points (both paths share it; the
    /// two definitions are `@if`-gated, so only one survives compilation).
    pub fragment_entry: String,
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
}

impl Default for CodegenOptions {
    fn default() -> Self {
        CodegenOptions {
            material_fn: abi::MATERIAL_FN.to_string(),
            vertex_entry: abi::VERTEX_ENTRY.to_string(),
            fragment_entry: abi::FRAGMENT_ENTRY.to_string(),
            emit_entry_points: true,
            base_macros: abi::abi_macros()
                .into_iter()
                .map(|decl| (decl.name.as_str().to_string(), decl.default))
                .collect(),
            override_macros: MacroSet::new(),
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

    let outputs = graph.surface_outputs(registry);
    let output_node = match outputs.len() {
        0 => return Err(CodegenError::NoOutputNode),
        1 => outputs[0],
        _ => return Err(CodegenError::MultipleOutputNodes(outputs)),
    };

    // Precedence, weakest first: the caller's defaults, each node's declared
    // default, what the graph pins, the caller's overrides.
    let mut macros = options.base_macros.clone();
    macros.overlay(&graph.effective_macros(registry)?);
    macros.overlay(&options.override_macros);
    let needed = graph.dependencies_of(output_node);
    let order = graph
        .topological_order(Some(&needed))
        .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;

    let mut emitter = Emitter {
        graph,
        registry,
        options,
        bindings: BTreeMap::new(),
        imports: BTreeMap::new(),
        body: String::new(),
    };
    emitter.request_abi_imports();

    for node in &order {
        if *node == output_node {
            continue;
        }
        emitter.emit_node(*node)?;
    }
    let surface = emitter.emit_surface(output_node)?;

    let source = emitter.finish(graph, &macros, &surface);
    let source_hash = stable_hash(source.as_bytes());
    Ok(GeneratedShader {
        source,
        macros,
        material_fn: options.material_fn.clone(),
        source_hash,
    })
}

/// Assignments the surface output node contributes, as `(field, expression)`.
type SurfaceAssignments = Vec<(String, String)>;

struct Emitter<'a> {
    graph: &'a Graph,
    registry: &'a NodeRegistry,
    options: &'a CodegenOptions,
    /// Expression that reads each already-emitted node output.
    bindings: BTreeMap<SocketRef, String>,
    /// Items to import, grouped by module and deduplicated.
    imports: BTreeMap<ModulePath, Vec<WxslIdent>>,
    /// The material function's statements.
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

    /// Import the fixed vocabulary every generated module uses.
    ///
    /// The deferred-path items are imported unconditionally; when the
    /// deferred fragment entry is dropped by conditional translation they
    /// become unused, and the compiler strips them.
    fn request_abi_imports(&mut self) {
        self.request_import(abi::SURFACE_MODULE, abi::SURFACE_STRUCT);
        self.request_import(abi::SURFACE_MODULE, abi::CONTEXT_STRUCT);
        self.request_import(abi::SURFACE_MODULE, abi::DEFAULT_SURFACE_FN);
        if self.options.emit_entry_points {
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_IN_STRUCT);
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_OUT_STRUCT);
            self.request_import(abi::VERTEX_MODULE, abi::TRANSFORM_VERTEX_FN);
            self.request_import(abi::VERTEX_MODULE, abi::SURFACE_CONTEXT_FN);
            self.request_import(abi::SHADING_MODULE, abi::SHADE_SURFACE_FN);
            self.request_import(abi::DEFERRED_MODULE, abi::GBUFFER_STRUCT);
            self.request_import(abi::DEFERRED_MODULE, abi::PACK_GBUFFER_FN);
        }
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

        let value: Option<Value> = instance.params.get(socket_name).copied().or(socket.default);
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
                    let _ = writeln!(
                        self.body,
                        "    let {name}: {} = {expr};",
                        socket.ty.wxsl_type()
                    );
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
                let call = format!("{}({})", func.name, args.join(", "));
                let name = binding_name(node, "call");
                match &func.ret {
                    FunctionReturn::Value(socket) => {
                        let _ = writeln!(
                            self.body,
                            "    let {name}: {} = {call};",
                            socket.ty.wxsl_type()
                        );
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
            NodeBody::ContextRead(field) => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                self.bindings.insert(
                    SocketRef::new(node, socket.name.as_str()),
                    format!("ctx.{field}"),
                );
            }
            NodeBody::SurfaceOutput => {
                // Emitted by `emit_surface`, which needs to run last.
                unreachable!("the surface output node is emitted separately");
            }
        }
        Ok(())
    }

    /// Collect the surface field assignments from the output node.
    fn emit_surface(&mut self, node: NodeId) -> Result<SurfaceAssignments, CodegenError> {
        let def = self.definition(node)?.clone();
        let mut assignments = Vec::new();
        for socket in &def.inputs {
            if let Some(expr) = self.input_expr(node, socket.name.as_str())? {
                assignments.push((socket.name.to_string(), expr));
            }
        }
        Ok(assignments)
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

    fn finish(self, graph: &Graph, macros: &MacroSet, surface: &SurfaceAssignments) -> String {
        let Emitter {
            options,
            imports,
            body,
            ..
        } = self;
        let mut out = String::with_capacity(body.len() + 2048);

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
        // The render-path flag is declared here rather than coming from the
        // macro set, because *this* module is what writes the `@if`s that
        // read it: the two fragment entry points below. It is not an
        // editable macro (ADR 0005 — the pipeline picks the path), so it
        // never appears in a graph's macro set, and the renderer binds it
        // per variant.
        if options.emit_entry_points {
            let _ = writeln!(out, "@macro const {}: bool = false;", abi::FEATURE_DEFERRED);
        }
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

        let _ = write!(
            out,
            "\nfn {}(ctx: {}) -> {} {{\n",
            options.material_fn,
            abi::CONTEXT_STRUCT,
            abi::SURFACE_STRUCT
        );
        out.push_str(&body);
        let _ = writeln!(
            out,
            "    var surface: {} = {}(ctx);",
            abi::SURFACE_STRUCT,
            abi::DEFAULT_SURFACE_FN
        );
        for (field, expr) in surface {
            let _ = writeln!(out, "    surface.{field} = {expr};");
        }
        out.push_str("    return surface;\n}\n");

        if options.emit_entry_points {
            write_entry_points(&mut out, options);
        }
        out
    }
}

/// Emit the vertex entry and the two `@if`-gated fragment entries.
fn write_entry_points(out: &mut String, options: &CodegenOptions) {
    let _ = write!(
        out,
        "
@vertex
fn {vertex}(input: {vertex_in}) -> {vertex_out} {{
    return {transform}(input);
}}

@if(!{feature})
@fragment
fn {fragment}(vertex: {vertex_out}) -> @location(0) vec4f {{
    let ctx = {context}(vertex);
    return {shade}({material}(ctx), ctx);
}}

@if({feature})
@fragment
fn {fragment}(vertex: {vertex_out}) -> {gbuffer} {{
    let ctx = {context}(vertex);
    return {pack}({material}(ctx));
}}
",
        vertex = options.vertex_entry,
        fragment = options.fragment_entry,
        material = options.material_fn,
        vertex_in = abi::VERTEX_IN_STRUCT,
        vertex_out = abi::VERTEX_OUT_STRUCT,
        transform = abi::TRANSFORM_VERTEX_FN,
        context = abi::SURFACE_CONTEXT_FN,
        shade = abi::SHADE_SURFACE_FN,
        gbuffer = abi::GBUFFER_STRUCT,
        pack = abi::PACK_GBUFFER_FN,
        feature = abi::FEATURE_DEFERRED,
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
    use crate::node::{Socket, ValueType, WxslFunction};

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
        // Both fragment entries are emitted, gated on the render path.
        assert!(shader.source.contains("@if(!wxsl_deferred)"));
        assert!(shader.source.contains("@if(wxsl_deferred)"));
        assert_eq!(shader.source.matches("fn fs_main").count(), 2);
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

        graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        assert!(matches!(
            generate(&graph, &registry, &CodegenOptions::default()),
            Err(CodegenError::MultipleOutputNodes(nodes)) if nodes.len() == 2
        ));
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
