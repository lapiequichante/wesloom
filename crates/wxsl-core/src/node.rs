//! Node and socket definitions: the typed interface a node kind describes to
//! the graph, and the WXSL it emits.
//!
//! A [`NodeDefinition`] is the *kind* of a node (`math.add.vec3f`,
//! `lighting.pbr_direct`, …): its typed input and output sockets, the macro
//! variables it reads, and a [`NodeBody`] saying how it turns its inputs into
//! WXSL. A node in a [`crate::graph::Graph`] is an *instance* of one of these,
//! looked up by id in a [`NodeRegistry`].
//!
//! Two bodies matter most:
//!
//! * [`NodeBody::Expr`] — an inline WXSL expression per output, used for the
//!   granular arithmetic that would be silly to wrap in a function
//!   (`{a} + {b}`).
//! * [`NodeBody::Call`] — a call to a [`WxslFunction`], i.e. a real WXSL
//!   function shipped as source (`wxsl-stdlib`) or hand-written by the
//!   user, described by data: its module path, parameter list and return
//!   shape. Anything nontrivial — PBR shading, noise, tonemapping — is a
//!   function, not an inline expression, so the same code is reachable from
//!   hand-written WXSL and from a graph
//!   ([ADR 0003](../../../docs/adr/0003-wesl-as-the-shading-language.md)).

use core::fmt;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::macros::MacroDef;
use crate::wxsl::{stable_hash, write_f32, ModulePath, WxslIdent};

/// The type of a value flowing along an edge.
///
/// Deliberately small and concrete: these are the WGSL types a material graph
/// actually moves between nodes. Sockets are matched by exact type — there is
/// no implicit conversion, so a graph that compiles is a graph whose WXSL
/// type-checks (`space.splat_vec3` and friends make conversions explicit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ValueType {
    /// WGSL `bool`.
    Bool,
    /// WGSL `i32`.
    I32,
    /// WGSL `u32`.
    U32,
    /// WGSL `f32`.
    F32,
    /// WGSL `vec2f`.
    Vec2,
    /// WGSL `vec3f`.
    Vec3,
    /// WGSL `vec4f`.
    Vec4,
    /// WGSL `mat3x3f`.
    Mat3,
    /// WGSL `mat4x4f`.
    Mat4,
}

impl ValueType {
    /// Every type, in declaration order.
    pub const ALL: &'static [ValueType] = &[
        ValueType::Bool,
        ValueType::I32,
        ValueType::U32,
        ValueType::F32,
        ValueType::Vec2,
        ValueType::Vec3,
        ValueType::Vec4,
        ValueType::Mat3,
        ValueType::Mat4,
    ];

    /// The float scalar and vector types, in increasing width.
    ///
    /// This is the set the arithmetic node families are generated over: the
    /// same `+` node makes sense for `f32` and `vec4f`, but not for `bool`.
    pub const FLOATS: &'static [ValueType] = &[
        ValueType::F32,
        ValueType::Vec2,
        ValueType::Vec3,
        ValueType::Vec4,
    ];

    /// The WGSL/WXSL spelling of this type.
    pub fn wxsl_type(&self) -> &'static str {
        match self {
            ValueType::Bool => "bool",
            ValueType::I32 => "i32",
            ValueType::U32 => "u32",
            ValueType::F32 => "f32",
            ValueType::Vec2 => "vec2f",
            ValueType::Vec3 => "vec3f",
            ValueType::Vec4 => "vec4f",
            ValueType::Mat3 => "mat3x3f",
            ValueType::Mat4 => "mat4x4f",
        }
    }

    /// The short suffix used in generated node ids (`math.add.vec3f`).
    pub fn suffix(&self) -> &'static str {
        self.wxsl_type()
    }

    /// Number of `f32` components for a float scalar/vector type.
    pub fn component_count(&self) -> Option<u32> {
        match self {
            ValueType::F32 => Some(1),
            ValueType::Vec2 => Some(2),
            ValueType::Vec3 => Some(3),
            ValueType::Vec4 => Some(4),
            _ => None,
        }
    }

    /// Whether this is a float scalar or float vector.
    pub fn is_float(&self) -> bool {
        self.component_count().is_some()
    }

    /// The all-zero value of this type.
    pub fn zero(&self) -> Value {
        match self {
            ValueType::Bool => Value::Bool(false),
            ValueType::I32 => Value::I32(0),
            ValueType::U32 => Value::U32(0),
            ValueType::F32 => Value::F32(0.0),
            ValueType::Vec2 => Value::Vec2([0.0; 2]),
            ValueType::Vec3 => Value::Vec3([0.0; 3]),
            ValueType::Vec4 => Value::Vec4([0.0; 4]),
            ValueType::Mat3 => Value::Mat3([0.0; 9]),
            ValueType::Mat4 => Value::Mat4([0.0; 16]),
        }
    }

    /// A float scalar/vector filled with `value`.
    ///
    /// Returns `None` for non-float types.
    pub fn splat(&self, value: f32) -> Option<Value> {
        match self {
            ValueType::F32 => Some(Value::F32(value)),
            ValueType::Vec2 => Some(Value::Vec2([value; 2])),
            ValueType::Vec3 => Some(Value::Vec3([value; 3])),
            ValueType::Vec4 => Some(Value::Vec4([value; 4])),
            _ => None,
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` so `{:<8}` in a listing actually aligns.
        f.pad(self.wxsl_type())
    }
}

/// A literal value: what an unconnected input carries, and what a constant
/// node emits.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Value {
    /// A `bool`.
    Bool(bool),
    /// An `i32`.
    I32(i32),
    /// A `u32`.
    U32(u32),
    /// An `f32`.
    F32(f32),
    /// A `vec2f`.
    Vec2([f32; 2]),
    /// A `vec3f`.
    Vec3([f32; 3]),
    /// A `vec4f`.
    Vec4([f32; 4]),
    /// A `mat3x3f`, in column-major order.
    Mat3([f32; 9]),
    /// A `mat4x4f`, in column-major order.
    Mat4([f32; 16]),
}

impl Value {
    /// This value's type.
    pub fn ty(&self) -> ValueType {
        match self {
            Value::Bool(_) => ValueType::Bool,
            Value::I32(_) => ValueType::I32,
            Value::U32(_) => ValueType::U32,
            Value::F32(_) => ValueType::F32,
            Value::Vec2(_) => ValueType::Vec2,
            Value::Vec3(_) => ValueType::Vec3,
            Value::Vec4(_) => ValueType::Vec4,
            Value::Mat3(_) => ValueType::Mat3,
            Value::Mat4(_) => ValueType::Mat4,
        }
    }

    /// This value as a WXSL expression, or `None` if any float component is
    /// not finite (WGSL has no `inf`/`nan` literal).
    pub fn wxsl_literal(&self) -> Option<String> {
        fn components(ty: &str, values: &[f32]) -> Option<String> {
            let mut out = String::with_capacity(ty.len() + values.len() * 6 + 2);
            out.push_str(ty);
            out.push('(');
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_f32(&mut out, *v)?;
            }
            out.push(')');
            Some(out)
        }

        match self {
            Value::Bool(v) => Some(v.to_string()),
            Value::I32(v) => Some(format!("{v}i")),
            Value::U32(v) => Some(format!("{v}u")),
            Value::F32(v) => {
                let mut out = String::new();
                write_f32(&mut out, *v)?;
                Some(out)
            }
            Value::Vec2(v) => components("vec2f", v),
            Value::Vec3(v) => components("vec3f", v),
            Value::Vec4(v) => components("vec4f", v),
            Value::Mat3(v) => components("mat3x3f", v),
            Value::Mat4(v) => components("mat4x4f", v),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.wxsl_literal() {
            Some(text) => f.write_str(&text),
            None => write!(f, "<unrepresentable {}>", self.ty()),
        }
    }
}

/// One typed port on a node: an input or an output.
#[derive(Clone, Debug, PartialEq)]
pub struct Socket {
    /// Socket name, unique among the node's inputs (or outputs). Doubles as
    /// the placeholder name in [`NodeBody::Expr`] templates and as the
    /// argument name in the serialized node format.
    pub name: WxslIdent,
    /// The type of value this socket carries.
    pub ty: ValueType,
    /// Value used when an input is left unconnected and the node instance
    /// pins no parameter. `None` on a non-optional input makes it mandatory.
    pub default: Option<Value>,
    /// Whether the input may be left unfed entirely, with no value at all.
    ///
    /// Only meaningful on a [`NodeBody::SurfaceOutput`] node, whose fields
    /// fall back to what [`crate::abi::DEFAULT_SURFACE_FN`] wrote: every
    /// other body needs an expression for each input to emit anything. The
    /// builder enforces that.
    pub optional: bool,
    /// One-line description for the editor.
    pub doc: String,
}

impl Socket {
    /// A socket with no default, i.e. a mandatory input (or any output).
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid WXSL identifier, or if `default` (when
    /// set later) does not match `ty`. Node definitions are authored in Rust,
    /// so these are programming errors rather than input to validate.
    pub fn new(name: &str, ty: ValueType) -> Self {
        Socket {
            name: WxslIdent::new(name).expect("socket name must be a valid WXSL identifier"),
            ty,
            default: None,
            optional: false,
            doc: String::new(),
        }
    }

    /// Mark this input as skippable when nothing feeds it.
    ///
    /// See [`Socket::optional`] for the one body kind this is valid on.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Whether this input must be fed by an edge, a parameter or a default.
    pub fn is_required(&self) -> bool {
        !self.optional && self.default.is_none()
    }

    /// Give this socket a default value.
    ///
    /// # Panics
    ///
    /// Panics if `value`'s type is not the socket's type.
    pub fn with_default(mut self, value: Value) -> Self {
        assert_eq!(
            value.ty(),
            self.ty,
            "default for socket `{}` is {} but the socket is {}",
            self.name,
            value.ty(),
            self.ty
        );
        self.default = Some(value);
        self
    }

    /// Default this socket to a float scalar/vector filled with `value`.
    ///
    /// # Panics
    ///
    /// Panics if the socket is not a float scalar or vector.
    pub fn with_splat_default(self, value: f32) -> Self {
        let splat = self
            .ty
            .splat(value)
            .unwrap_or_else(|| panic!("socket `{}` is not a float type", self.name));
        self.with_default(splat)
    }

    /// Attach a one-line description.
    pub fn with_doc(mut self, doc: impl Into<String>) -> Self {
        self.doc = doc.into();
        self
    }
}

/// What a [`WxslFunction`] returns, and therefore what outputs the node has.
#[derive(Clone, Debug, PartialEq)]
pub enum FunctionReturn {
    /// A single value, exposed as one output socket with the given name.
    Value(Socket),
    /// A struct, exposed as one output socket per field. The generated code
    /// binds the call once and reads the fields off it, so a function that
    /// computes several related quantities (a full BRDF split into diffuse
    /// and specular, say) is evaluated once no matter how many of its outputs
    /// are used.
    Struct {
        /// The struct type's name, imported alongside the function.
        name: WxslIdent,
        /// The fields to expose as outputs. Names must match the WXSL struct.
        fields: Vec<Socket>,
    },
}

/// A WXSL function usable as a node, described by data.
///
/// This is the bridge between "a shader function someone wrote in WXSL" and
/// "a node the graph and editor can reason about": the WXSL source stays the
/// source of truth for *behaviour*, and this struct describes its *interface*
/// so the graph can type-check calls to it and codegen can emit them.
#[derive(Clone, Debug, PartialEq)]
pub struct WxslFunction {
    /// Module the function lives in, e.g. `package::lighting::pbr`.
    pub module: ModulePath,
    /// The function's name.
    pub name: WxslIdent,
    /// Parameters, in call order. Each becomes an input socket.
    pub params: Vec<Socket>,
    /// What the function returns.
    pub ret: FunctionReturn,
}

impl WxslFunction {
    /// Describe a function returning a single value.
    ///
    /// # Panics
    ///
    /// Panics if `module` is not a valid module path or `name` is not a valid
    /// identifier.
    pub fn new(module: &str, name: &str, params: Vec<Socket>, ret: Socket) -> Self {
        WxslFunction {
            module: ModulePath::new(module).expect("invalid WXSL module path"),
            name: WxslIdent::new(name).expect("invalid WXSL function name"),
            params,
            ret: FunctionReturn::Value(ret),
        }
    }

    /// Describe a function returning a struct, exposing `fields` as outputs.
    ///
    /// # Panics
    ///
    /// Panics if `module`, `name` or `struct_name` are not valid WXSL.
    pub fn new_struct(
        module: &str,
        name: &str,
        params: Vec<Socket>,
        struct_name: &str,
        fields: Vec<Socket>,
    ) -> Self {
        WxslFunction {
            module: ModulePath::new(module).expect("invalid WXSL module path"),
            name: WxslIdent::new(name).expect("invalid WXSL function name"),
            params,
            ret: FunctionReturn::Struct {
                name: WxslIdent::new(struct_name).expect("invalid WXSL struct name"),
                fields,
            },
        }
    }

    /// The `(module, item)` pairs this call needs imported.
    pub fn imports(&self) -> Vec<(ModulePath, WxslIdent)> {
        let mut imports = vec![(self.module.clone(), self.name.clone())];
        if let FunctionReturn::Struct { name, .. } = &self.ret {
            imports.push((self.module.clone(), name.clone()));
        }
        imports
    }

    /// The output sockets this function exposes.
    pub fn outputs(&self) -> Vec<Socket> {
        match &self.ret {
            FunctionReturn::Value(socket) => vec![socket.clone()],
            FunctionReturn::Struct { fields, .. } => fields.clone(),
        }
    }

    /// The function's WXSL signature, for docs and error messages.
    pub fn signature(&self) -> String {
        let params = self
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = match &self.ret {
            FunctionReturn::Value(socket) => socket.ty.wxsl_type().to_string(),
            FunctionReturn::Struct { name, .. } => name.to_string(),
        };
        format!("fn {}({params}) -> {ret}", self.name)
    }
}

/// How a node turns its inputs into WXSL.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeBody {
    /// One WXSL expression per output socket, in output order.
    ///
    /// `{socket}` placeholders are replaced by the expression bound to that
    /// input; `{{` and `}}` are literal braces. Meant for granular
    /// arithmetic — anything with a body belongs in a [`WxslFunction`].
    Expr(Vec<String>),
    /// A call to a WXSL function.
    Call(WxslFunction),
    /// Reads one field of the per-fragment surface context the renderer hands
    /// to the material function (world position, UV, view direction, …). The
    /// field set is part of the shader ABI, see [`crate::abi`].
    ContextRead(WxslIdent),
    /// The terminal node: its inputs are the fields of the surface struct the
    /// material function returns. A graph has exactly one of these.
    SurfaceOutput,
}

/// The kind of a node: its typed interface plus the WXSL it emits.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeDefinition {
    /// Registry id, dotted and stable: `math.add.vec3f`, `lighting.pbr`.
    /// This is what a serialized graph stores, so treat it as a public name.
    pub id: String,
    /// Human-readable name for the editor.
    pub label: String,
    /// Category for grouping in the editor's node palette (`math`, `color`,
    /// `lighting`, …). Conventionally the first segment of [`Self::id`].
    pub category: String,
    /// One-paragraph description.
    pub doc: String,
    /// Typed input sockets.
    pub inputs: Vec<Socket>,
    /// Typed output sockets.
    pub outputs: Vec<Socket>,
    /// Macro variables this node reads. Declaring them here is what puts them
    /// in the graph's editable macro set, see [`crate::macros`].
    pub macros: Vec<MacroDef>,
    /// Extra `(module, item)` imports the body needs, beyond those implied by
    /// a [`NodeBody::Call`].
    pub imports: Vec<(ModulePath, WxslIdent)>,
    /// How the node emits WXSL.
    pub body: NodeBody,
}

impl NodeDefinition {
    /// Start building a definition.
    pub fn builder(id: impl Into<String>, label: impl Into<String>) -> NodeDefinitionBuilder {
        let id = id.into();
        let category = id.split('.').next().unwrap_or_default().to_string();
        NodeDefinitionBuilder {
            def: NodeDefinition {
                id,
                label: label.into(),
                category,
                doc: String::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                macros: Vec::new(),
                imports: Vec::new(),
                body: NodeBody::Expr(Vec::new()),
            },
        }
    }

    /// Build a definition that calls `func`, taking its inputs and outputs
    /// from the function's described interface.
    pub fn from_function(
        id: impl Into<String>,
        label: impl Into<String>,
        doc: impl Into<String>,
        func: WxslFunction,
    ) -> Self {
        let mut def = NodeDefinition::builder(id, label).doc(doc).def;
        def.inputs = func.params.clone();
        def.outputs = func.outputs();
        def.body = NodeBody::Call(func);
        def
    }

    /// Declare the macro variables this node reads.
    ///
    /// Replaces any already declared. Useful after
    /// [`NodeDefinition::from_function`], which takes its interface from the
    /// function and knows nothing about the macros the body reads.
    pub fn with_macros(mut self, macros: Vec<MacroDef>) -> Self {
        self.macros = macros;
        self
    }

    /// Look up an input socket by name.
    pub fn input(&self, name: &str) -> Option<&Socket> {
        self.inputs.iter().find(|s| s.name.as_str() == name)
    }

    /// Look up an output socket by name.
    pub fn output(&self, name: &str) -> Option<&Socket> {
        self.outputs.iter().find(|s| s.name.as_str() == name)
    }

    /// Whether this is the graph's terminal surface-output node.
    pub fn is_surface_output(&self) -> bool {
        matches!(self.body, NodeBody::SurfaceOutput)
    }

    /// Every `(module, item)` pair this node needs imported.
    pub fn all_imports(&self) -> Vec<(ModulePath, WxslIdent)> {
        let mut imports = self.imports.clone();
        if let NodeBody::Call(func) = &self.body {
            imports.extend(func.imports());
        }
        imports
    }
}

/// Builder for [`NodeDefinition`], so definitions read like declarations.
///
/// Every setter panics on invalid WXSL names or mistyped defaults: node
/// definitions are code, and a bad one should fail loudly at startup rather
/// than produce a shader that will not compile.
#[derive(Clone, Debug)]
pub struct NodeDefinitionBuilder {
    def: NodeDefinition,
}

impl NodeDefinitionBuilder {
    /// Set the description.
    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.def.doc = doc.into();
        self
    }

    /// Override the palette category (defaults to the id's first segment).
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.def.category = category.into();
        self
    }

    /// Add an input socket.
    pub fn input(mut self, socket: Socket) -> Self {
        assert!(
            self.def.input(socket.name.as_str()).is_none(),
            "duplicate input `{}` on `{}`",
            socket.name,
            self.def.id
        );
        self.def.inputs.push(socket);
        self
    }

    /// Add an output socket.
    pub fn output(mut self, socket: Socket) -> Self {
        assert!(
            self.def.output(socket.name.as_str()).is_none(),
            "duplicate output `{}` on `{}`",
            socket.name,
            self.def.id
        );
        self.def.outputs.push(socket);
        self
    }

    /// Declare a macro variable this node reads.
    pub fn macro_var(mut self, decl: MacroDef) -> Self {
        self.def.macros.push(decl);
        self
    }

    /// Import `item` from `module` for the body's use.
    ///
    /// # Panics
    ///
    /// Panics if `module` or `item` are not valid WXSL names.
    pub fn import(mut self, module: &str, item: &str) -> Self {
        self.def.imports.push((
            ModulePath::new(module).expect("invalid WXSL module path"),
            WxslIdent::new(item).expect("invalid WXSL item name"),
        ));
        self
    }

    /// Finish with a [`NodeBody::Expr`] body, one expression per output.
    pub fn exprs(mut self, exprs: impl IntoIterator<Item = impl Into<String>>) -> NodeDefinition {
        self.def.body = NodeBody::Expr(exprs.into_iter().map(Into::into).collect());
        self.build()
    }

    /// Finish with a single-output [`NodeBody::Expr`] body.
    pub fn expr(self, expr: impl Into<String>) -> NodeDefinition {
        self.exprs([expr])
    }

    /// Finish with a [`NodeBody::Call`] body. Inputs and outputs are taken
    /// from the function, replacing anything added with
    /// [`Self::input`]/[`Self::output`].
    pub fn call(mut self, func: WxslFunction) -> NodeDefinition {
        self.def.inputs = func.params.clone();
        self.def.outputs = func.outputs();
        self.def.body = NodeBody::Call(func);
        self.build()
    }

    /// Finish with a [`NodeBody::ContextRead`] body reading `field`.
    ///
    /// # Panics
    ///
    /// Panics if `field` is not a valid WXSL identifier.
    pub fn context_read(mut self, field: &str) -> NodeDefinition {
        self.def.body =
            NodeBody::ContextRead(WxslIdent::new(field).expect("invalid context field name"));
        self.build()
    }

    /// Finish with a [`NodeBody::SurfaceOutput`] body.
    pub fn surface_output(mut self) -> NodeDefinition {
        self.def.body = NodeBody::SurfaceOutput;
        self.build()
    }

    /// Finish without changing the body.
    ///
    /// # Panics
    ///
    /// Panics if an [`NodeBody::Expr`] body has a different number of
    /// expressions than the node has outputs.
    pub fn build(self) -> NodeDefinition {
        if !matches!(self.def.body, NodeBody::SurfaceOutput) {
            for socket in &self.def.inputs {
                assert!(
                    !socket.optional,
                    "input `{}` of `{}` is optional, which only a surface output node supports",
                    socket.name, self.def.id
                );
            }
        }
        if let NodeBody::Expr(exprs) = &self.def.body {
            assert_eq!(
                exprs.len(),
                self.def.outputs.len(),
                "node `{}` has {} outputs but {} expressions",
                self.def.id,
                self.def.outputs.len(),
                exprs.len()
            );
        }
        self.def
    }
}

/// The set of node definitions a graph is validated and compiled against.
///
/// Definitions are shared (`Arc`) because validation, codegen and an editor
/// all hold onto them while a graph is being edited.
#[derive(Clone, Debug, Default)]
pub struct NodeRegistry {
    defs: BTreeMap<String, Arc<NodeDefinition>>,
}

impl NodeRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `def`, returning the definition it replaced, if any.
    pub fn register(&mut self, def: NodeDefinition) -> Option<Arc<NodeDefinition>> {
        self.defs.insert(def.id.clone(), Arc::new(def))
    }

    /// Register every definition in `defs`.
    ///
    /// # Panics
    ///
    /// Panics on a duplicate id: two definitions claiming the same name would
    /// make a serialized graph's meaning depend on registration order.
    pub fn register_all(&mut self, defs: impl IntoIterator<Item = NodeDefinition>) {
        for def in defs {
            let id = def.id.clone();
            assert!(
                self.register(def).is_none(),
                "node definition `{id}` is registered twice"
            );
        }
    }

    /// Look up a definition by id.
    pub fn get(&self, id: &str) -> Option<&Arc<NodeDefinition>> {
        self.defs.get(id)
    }

    /// Whether `id` is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.defs.contains_key(id)
    }

    /// Number of registered definitions.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Iterate over definitions in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<NodeDefinition>> {
        self.defs.values()
    }

    /// Every registered category, in order, without duplicates.
    pub fn categories(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = Vec::new();
        for def in self.iter() {
            if !seen.contains(&def.category.as_str()) {
                seen.push(&def.category);
            }
        }
        seen
    }

    /// A stable hash of every definition id in the registry.
    ///
    /// Part of the shader variant cache key: swapping a definition's
    /// implementation under the same id must not silently reuse WGSL compiled
    /// from the old one.
    pub fn signature(&self) -> u64 {
        let mut text = String::new();
        for def in self.iter() {
            text.push_str(&def.id);
            text.push('\0');
        }
        stable_hash(text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_literals_are_wgsl_shaped() {
        assert_eq!(Value::F32(1.0).wxsl_literal().unwrap(), "1.0");
        assert_eq!(Value::I32(-2).wxsl_literal().unwrap(), "-2i");
        assert_eq!(Value::U32(7).wxsl_literal().unwrap(), "7u");
        assert_eq!(Value::Bool(true).wxsl_literal().unwrap(), "true");
        assert_eq!(
            Value::Vec3([0.5, 1.0, 0.0]).wxsl_literal().unwrap(),
            "vec3f(0.5, 1.0, 0.0)"
        );
        assert!(Value::Vec2([1.0, f32::NAN]).wxsl_literal().is_none());
    }

    #[test]
    fn builder_derives_category_from_id() {
        let def = NodeDefinition::builder("math.add.f32", "Add")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("b", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("{a} + {b}");
        assert_eq!(def.category, "math");
        assert_eq!(def.inputs.len(), 2);
        assert!(!def.is_surface_output());
    }

    #[test]
    fn function_nodes_take_their_interface_from_the_function() {
        let func = WxslFunction::new_struct(
            "package::lighting::pbr",
            "pbr_split",
            vec![Socket::new("normal", ValueType::Vec3)],
            "PbrSplit",
            vec![
                Socket::new("diffuse", ValueType::Vec3),
                Socket::new("specular", ValueType::Vec3),
            ],
        );
        let def = NodeDefinition::from_function("lighting.pbr_split", "PBR split", "", func);
        assert_eq!(def.inputs.len(), 1);
        assert_eq!(def.outputs.len(), 2);
        // Both the function and its return struct must be imported.
        assert_eq!(def.all_imports().len(), 2);
    }

    #[test]
    #[should_panic(expected = "1 outputs but 2 expressions")]
    fn expression_arity_is_checked_at_build_time() {
        NodeDefinition::builder("math.bogus", "Bogus")
            .output(Socket::new("out", ValueType::F32))
            .exprs(["1.0", "2.0"]);
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_ids_are_rejected() {
        let def = || {
            NodeDefinition::builder("math.dup", "Dup")
                .output(Socket::new("out", ValueType::F32))
                .expr("0.0")
        };
        let mut registry = NodeRegistry::new();
        registry.register_all([def(), def()]);
    }
}
