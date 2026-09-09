//! Every stdlib function and operator as a `wxsl-core` node definition.
//!
//! Two kinds of node live here, and the split is deliberate:
//!
//! * **Operators** ([`math_nodes`], [`vector_nodes`], [`convert_nodes`],
//!   [`logic_nodes`], [`constant_nodes`]) are inline WXSL expressions over
//!   WGSL's own built-ins — `{a} + {b}`, `mix({a}, {b}, {t})`. Wrapping an
//!   addition in a function call would cost a WXSL module, an import and a
//!   call per node for no gain.
//! * **Functions** (everything else: [`color_nodes`], [`lighting_nodes`],
//!   [`generative_nodes`], …) are calls to the `.wxsl` functions this crate
//!   ships, each described by a
//!   [`wxsl_core::node::WxslFunction`] giving its module,
//!   name, parameters and return shape. The WXSL source stays the single
//!   definition of the behaviour; the descriptor here is what lets the graph
//!   type-check a call to it and lets codegen emit one
//!   ([ADR 0003](../../../docs/adr/0003-wesl-as-the-shading-language.md)).
//!
//! **One node per operation, not one per operation and type.** A definition
//! declares the types it works over as a [`GenericParam`] and its sockets
//! carry it, so `math.add` is one registry entry covering `f32` through
//! `mat4x4f`, resolved per graph node from whatever is connected — not
//! `math.add.f32`, `math.add.vec2f`, … as separate node kinds
//! ([ADR 0015](../../../docs/adr/0015-generic-sockets-for-arithmetic-nodes.md),
//! [ADR 0018](../../../docs/adr/0018-one-generic-node-per-operation.md)).
//! Where WGSL lets an operator's two operands differ — `f32 * vec3f`,
//! `mat3x3f * vec3f`, `vec3f + f32` — the node declares *two* parameters and
//! derives the result with a [`TypeRule`], which is the only way to say
//! "these two may legitimately differ". `crates/wxsl/tests/wgsl_types.rs`
//! checks those rules against `wgpu`'s validator, and is where the tables
//! below came from.
//!
//! A function whose WXSL reads a macro variable declares that macro on its
//! node definition (see [`generative_nodes`] and `shaders/generative/fbm3.wxsl`).
//! That is what puts the macro in a graph's editable macro set, and what
//! guarantees the generated macro module declares it whenever the function is
//! reachable.

use wxsl_core::abi;
use wxsl_core::macros::{MacroDef, MacroValue};
use wxsl_core::node::{
    GenericParam, NodeDefinition, NodeRegistry, Socket, TypeRule, Value, ValueType, WxslFunction,
};

/// Every node definition in the library, including the ABI's input and
/// output nodes.
pub fn all_nodes() -> Vec<NodeDefinition> {
    let mut defs = Vec::new();
    defs.extend(abi_nodes());
    defs.extend(constant_nodes());
    defs.extend(math_nodes());
    defs.extend(vector_nodes());
    defs.extend(convert_nodes());
    defs.extend(logic_nodes());
    defs.extend(math_function_nodes());
    defs.extend(safe_normalize_nodes());
    defs.extend(color_nodes());
    defs.extend(space_nodes());
    defs.extend(generative_nodes());
    defs.extend(lighting_nodes());
    defs.extend(sdf_nodes());
    defs.extend(animation_nodes());
    defs.extend(distort_nodes());
    defs
}

/// A registry holding [`all_nodes`].
///
/// This is what a graph is validated and compiled against; a consumer with
/// its own nodes registers them on top.
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register_all(all_nodes());
    registry
}

/// The graph's entry and exit nodes, generated from the shader ABI: one
/// reader per [`abi::CONTEXT_FIELDS`] entry, and the surface output node.
pub fn abi_nodes() -> Vec<NodeDefinition> {
    let mut defs = abi::context_node_defs();
    defs.push(abi::surface_output_def());
    defs
}

// ---------------------------------------------------------------------------
// Socket helpers
// ---------------------------------------------------------------------------

fn out(ty: ValueType) -> Socket {
    Socket::new("out", ty)
}

fn scalar(name: &str, default: f32) -> Socket {
    Socket::new(name, ValueType::F32).with_default(Value::F32(default))
}

fn splat(name: &str, ty: ValueType, default: f32) -> Socket {
    Socket::new(name, ty).with_splat_default(default)
}

fn color_socket(name: &str, default: [f32; 3]) -> Socket {
    Socket::new(name, ValueType::Vec3).with_default(Value::Vec3(default))
}

fn direction(name: &str, default: [f32; 3]) -> Socket {
    Socket::new(name, ValueType::Vec3).with_default(Value::Vec3(default))
}

/// A generic parameter named `name`, resolvable to any of `allowed`.
///
/// A free function rather than a `const` because [`GenericParam`] owns a
/// `Vec` (a `ValueType` slice turned into one), so it cannot be a `const`
/// itself.
fn param(name: &str, allowed: &[ValueType]) -> GenericParam {
    GenericParam::new(name, allowed.to_vec())
}

/// The single parameter `T` that most generic nodes declare: one type,
/// shared by every socket carrying it, resolved per graph node from
/// whatever is actually connected (or picked explicitly with
/// [`wxsl_core::graph::Graph::set_generic`]).
///
/// The nodes that do *not* use this shape are the ones whose operands WGSL
/// lets differ: those declare two parameters and derive the result with a
/// [`TypeRule`] — see [`BINARY`] and `vector.transform`.
fn shared(allowed: &[ValueType]) -> GenericParam {
    param("T", allowed)
}

/// A socket carrying [`shared`]'s `T`, with no default.
///
/// The placeholder `F32` is never read: codegen always asks the graph for
/// this node instance's *resolved* type instead (see
/// [`wxsl_core::node::Socket::generic`]).
fn generic_socket(name: &str) -> Socket {
    Socket::new(name, ValueType::F32).generic("T")
}

/// A socket carrying `T`, defaulting to `default` spread over whatever `T`
/// resolves to — `clamp`'s `high` is 1 at every width.
///
/// See [`wxsl_core::node::Socket::splat_default`]: a fixed [`Value`] would
/// be the wrong type for every resolution but one, but a scalar to spread is
/// well defined at all of them.
fn generic_splat(name: &str, default: f32) -> Socket {
    generic_socket(name).with_splat_default(default)
}

/// An expression that falls back to `fallback` where `span` is degenerate.
///
/// The component-wise replacement for `if abs(span) < 1e-8 { return … }`,
/// which only type-checks for `f32`: a vector comparison yields a
/// `vec<bool>`, and `if` demands a scalar. `select(false, true, cond)` takes
/// a vector condition and chooses per component.
///
/// `{$T}` is the resolved WGSL spelling of the node's generic parameter `T`
/// (see [`wxsl_core::codegen`]), which is what lets one template serve every
/// type `T` allows instead of one node per type.
fn guarded(fallback: &str, value: &str, span: &str) -> String {
    format!("select({fallback}, {value}, abs({span}) >= {{$T}}(1e-8))")
}

/// Build a node that calls a single-value WXSL function.
fn function_node(
    id: &str,
    label: &str,
    doc: &str,
    module: &str,
    name: &str,
    params: Vec<Socket>,
    ret: Socket,
) -> NodeDefinition {
    NodeDefinition::from_function(id, label, doc, WxslFunction::new(module, name, params, ret))
}

/// Build a node that calls a WXSL *template* — one function the compiler
/// instantiates per type
/// ([ADR 0012](../../../docs/adr/0012-monomorphize-templates-on-the-flat-module.md)) —
/// generic over one parameter `T` restricted to `allowed`.
///
/// Codegen writes the type argument explicitly (`safe_normalize<vec3f>(v)`),
/// which is the case ADR 0012 says never has to be inferred: the graph knows
/// every socket's type exactly.
fn template_node(
    id: &str,
    label: &str,
    doc: &str,
    allowed: &[ValueType],
    func: WxslFunction,
) -> NodeDefinition {
    NodeDefinition::builder(id, label)
        .doc(doc)
        .generic_param(shared(allowed))
        .call(func)
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

/// A unary operator family: one generic node, covering every type its
/// parameter allows.
struct Unary {
    /// Id stem: `math.{stem}` is the node this family becomes.
    stem: &'static str,
    /// Editor label.
    label: &'static str,
    /// Description.
    doc: &'static str,
    /// Expression template over the family's socket names.
    expr: &'static str,
}

/// A binary operator family: one generic node, covering every combination of
/// types WGSL accepts for it.
struct Binary {
    /// Id stem: `math.{stem}`.
    stem: &'static str,
    /// Editor label.
    label: &'static str,
    /// Description.
    doc: &'static str,
    /// Expression template over `a` and `b`.
    expr: &'static str,
    /// How the two operands' types relate.
    ///
    /// `None` — one shared parameter `T` on both operands and the result,
    /// because the WGSL *builtin* behind this family (`pow`, `min`, `max`)
    /// has a single `(T, T) -> T` overload and accepts nothing else.
    ///
    /// `Some(rule)` — two independent parameters `A` and `B` with the
    /// result derived by `rule`, because the WGSL *operator* behind it also
    /// spreads a scalar over a vector's components (`f32 + vec3f` is
    /// `vec3f`), and `*` additionally does linear algebra (`mat3x3f *
    /// vec3f` is `vec3f`). Two parameters is the only way to say "these two
    /// may legitimately differ"; see [`wxsl_core::node::Socket::combine`].
    rule: Option<TypeRule>,
    /// Whether the operands may also be matrices. WGSL adds and subtracts
    /// two matrices of the same shape and multiplies a matrix by a scalar,
    /// a vector or another matrix — but `/` and `%` accept no matrix at
    /// all, and neither does any of the builtins.
    matrices: bool,
}

const BINARY: &[Binary] = &[
    Binary {
        stem: "add",
        label: "Add",
        doc: "Sum of two operands. Either may be a scalar spread over the \
              other's components, and two matrices of the same shape add \
              element-wise.",
        expr: "{a} + {b}",
        rule: Some(TypeRule::Componentwise),
        matrices: true,
    },
    Binary {
        stem: "subtract",
        label: "Subtract",
        doc: "Difference of two operands, with the same mixing rules as \
              `math.add`.",
        expr: "{a} - {b}",
        rule: Some(TypeRule::Componentwise),
        matrices: true,
    },
    Binary {
        stem: "multiply",
        label: "Multiply",
        doc: "Product of two operands. Either may be a scalar spread over \
              the other's components, and a matrix may multiply a vector of \
              its own size — which is how a basis from \
              `space.tangent_basis` is applied to a direction — or another \
              matrix of the same shape.",
        expr: "{a} * {b}",
        rule: Some(TypeRule::Product),
        matrices: true,
    },
    Binary {
        stem: "divide",
        label: "Divide",
        doc: "Quotient of two operands, either of which may be a scalar \
              spread over the other's components. Division by zero yields \
              an infinity, which will spread; guard the divisor if it can \
              reach zero.",
        expr: "{a} / {b}",
        rule: Some(TypeRule::Componentwise),
        matrices: false,
    },
    Binary {
        stem: "modulo",
        label: "Modulo",
        doc: "Floating-point remainder, keeping the sign of the dividend. \
              For a periodic wrap use `math.wrap` instead.",
        expr: "{a} % {b}",
        rule: Some(TypeRule::Componentwise),
        matrices: false,
    },
    Binary {
        stem: "power",
        label: "Power",
        doc: "`a` raised to `b`, component-wise. Undefined for a negative \
              base with a fractional exponent.",
        expr: "pow({a}, {b})",
        rule: None,
        matrices: false,
    },
    Binary {
        stem: "minimum",
        label: "Minimum",
        doc: "Component-wise smaller of the two.",
        expr: "min({a}, {b})",
        rule: None,
        matrices: false,
    },
    Binary {
        stem: "maximum",
        label: "Maximum",
        doc: "Component-wise larger of the two.",
        expr: "max({a}, {b})",
        rule: None,
        matrices: false,
    },
];

const UNARY: &[Unary] = &[
    Unary {
        stem: "negate",
        label: "Negate",
        doc: "Flip the sign, component-wise.",
        expr: "-{a}",
    },
    Unary {
        stem: "absolute",
        label: "Absolute",
        doc: "Drop the sign, component-wise.",
        expr: "abs({a})",
    },
    Unary {
        stem: "sign",
        label: "Sign",
        doc: "-1, 0 or 1 per component.",
        expr: "sign({a})",
    },
    Unary {
        stem: "floor",
        label: "Floor",
        doc: "Round down, component-wise.",
        expr: "floor({a})",
    },
    Unary {
        stem: "ceil",
        label: "Ceil",
        doc: "Round up, component-wise.",
        expr: "ceil({a})",
    },
    Unary {
        stem: "round",
        label: "Round",
        doc: "Round to nearest, halves to even.",
        expr: "round({a})",
    },
    Unary {
        stem: "truncate",
        label: "Truncate",
        doc: "Drop the fractional part, towards zero.",
        expr: "trunc({a})",
    },
    Unary {
        stem: "fraction",
        label: "Fraction",
        doc: "The fractional part, always in [0, 1).",
        expr: "fract({a})",
    },
    Unary {
        stem: "saturate",
        label: "Saturate",
        doc: "Clamp to [0, 1], component-wise.",
        expr: "saturate({a})",
    },
    Unary {
        stem: "square_root",
        label: "Square root",
        doc: "Component-wise square root; negative inputs give NaN.",
        expr: "sqrt({a})",
    },
    Unary {
        stem: "inverse_square_root",
        label: "Inverse square root",
        doc: "1/sqrt, component-wise, as a single instruction.",
        expr: "inverseSqrt({a})",
    },
    Unary {
        stem: "exponential",
        label: "Exponential",
        doc: "e raised to the input, component-wise.",
        expr: "exp({a})",
    },
    Unary {
        stem: "exponential_2",
        label: "Exponential (base 2)",
        doc: "2 raised to the input, component-wise.",
        expr: "exp2({a})",
    },
    Unary {
        stem: "logarithm",
        label: "Logarithm",
        doc: "Natural log, component-wise.",
        expr: "log({a})",
    },
    Unary {
        stem: "logarithm_2",
        label: "Logarithm (base 2)",
        doc: "Base-2 log, component-wise.",
        expr: "log2({a})",
    },
    Unary {
        stem: "sine",
        label: "Sine",
        doc: "Sine of an angle in radians.",
        expr: "sin({a})",
    },
    Unary {
        stem: "cosine",
        label: "Cosine",
        doc: "Cosine of an angle in radians.",
        expr: "cos({a})",
    },
    Unary {
        stem: "tangent",
        label: "Tangent",
        doc: "Tangent of an angle in radians.",
        expr: "tan({a})",
    },
    Unary {
        stem: "arcsine",
        label: "Arcsine",
        doc: "Inverse sine, in radians. Input outside [-1, 1] gives NaN.",
        expr: "asin({a})",
    },
    Unary {
        stem: "arccosine",
        label: "Arccosine",
        doc: "Inverse cosine, in radians. Input outside [-1, 1] gives NaN.",
        expr: "acos({a})",
    },
    Unary {
        stem: "arctangent",
        label: "Arctangent",
        doc: "Inverse tangent, in radians.",
        expr: "atan({a})",
    },
];

/// Arithmetic and the WGSL builtins that go with it, one generic node per
/// family instead of one per family *and* type.
///
/// `{a} + {b}` is the same WXSL whatever the operands are, so what used to
/// differ between `math.add.f32`, `math.add.vec2f`, `math.add.vec3f` and
/// `math.add.vec4f` was only the type annotation on otherwise identical
/// sockets ([ADR 0015](../../../docs/adr/0015-generic-sockets-for-arithmetic-nodes.md)).
/// The families whose operands WGSL lets differ declare two parameters and
/// derive the result, rather than forcing one shared `T` on both — see
/// [`Binary::rule`] and
/// [ADR 0018](../../../docs/adr/0018-one-generic-node-per-operation.md).
pub fn math_nodes() -> Vec<NodeDefinition> {
    let mut defs = Vec::new();
    for family in BINARY {
        let allowed = if family.matrices {
            ValueType::operands()
        } else {
            ValueType::FLOATS.to_vec()
        };
        let builder = NodeDefinition::builder(format!("math.{}", family.stem), family.label)
            .category("math")
            .doc(family.doc);
        defs.push(match family.rule {
            None => builder
                .generic_param(param("T", &allowed))
                .input(generic_socket("a"))
                .input(generic_socket("b"))
                .output(generic_socket("out"))
                .expr(family.expr),
            Some(rule) => builder
                .generic_param(param("A", &allowed))
                .generic_param(param("B", &allowed))
                .input(Socket::new("a", ValueType::F32).generic("A"))
                .input(Socket::new("b", ValueType::F32).generic("B"))
                .output(Socket::new("out", ValueType::F32).combine(rule, "A", "B"))
                .expr(family.expr),
        });
    }
    for family in UNARY {
        defs.push(
            NodeDefinition::builder(format!("math.{}", family.stem), family.label)
                .category("math")
                .doc(family.doc)
                .generic_param(shared(ValueType::FLOATS))
                .input(generic_socket("a"))
                .output(generic_socket("out"))
                .expr(family.expr),
        );
    }

    // Operators whose sockets are not interchangeable operands, so they are
    // spelled out rather than generated from a table. They are still one
    // generic node each: every socket below either carries `T` or is a
    // scalar that WGSL accepts at *every* type `T` allows, which is a fixed
    // type rather than a per-type split.
    defs.push(
        NodeDefinition::builder("math.clamp", "Clamp")
            .category("math")
            .doc("Constrain to a range, component-wise.")
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("x", 0.0))
            .input(generic_splat("low", 0.0))
            .input(generic_splat("high", 1.0))
            .output(generic_socket("out"))
            .expr("clamp({x}, {low}, {high})"),
    );
    defs.push(
        NodeDefinition::builder("math.mix", "Mix")
            .category("math")
            .doc(
                "Linear blend: `a` at t=0, `b` at t=1, extrapolating \
                 outside. `t` is a scalar, which WGSL's `mix` accepts \
                 against operands of any width.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("a", 0.0))
            .input(generic_splat("b", 1.0))
            .input(scalar("t", 0.5))
            .output(generic_socket("out"))
            .expr("mix({a}, {b}, {t})"),
    );
    defs.push(
        NodeDefinition::builder("math.step", "Step")
            .category("math")
            .doc("0 below the edge, 1 at or above it, component-wise.")
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("edge", 0.5))
            .input(generic_splat("x", 0.0))
            .output(generic_socket("out"))
            .expr("step({edge}, {x})"),
    );
    defs.push(
        NodeDefinition::builder("math.smoothstep", "Smoothstep")
            .category("math")
            .doc(
                "Hermite ramp between two edges. For a continuous second \
                 derivative use `math.smootherstep`.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("edge0", 0.0))
            .input(generic_splat("edge1", 1.0))
            .input(generic_splat("x", 0.5))
            .output(generic_socket("out"))
            .expr("smoothstep({edge0}, {edge1}, {x})"),
    );

    // Range operators. These have a body in the mathematical sense — a
    // span, and a guard for the degenerate case where it is zero — but they
    // stay expressions rather than WXSL functions so that one template
    // covers every type `T` allows. A scalar `if` cannot: for the vector
    // types `abs(span) < 1e-8` is a `vec<bool>`, which `if` rejects.
    // `guarded` reshapes it into a `select`, which also gives the better
    // semantics — one degenerate component collapses only itself, not the
    // whole vector. The span appears more than once in the emitted
    // expression; the shader compiler folds it.
    defs.push(
        NodeDefinition::builder("math.inverse_lerp", "Inverse lerp")
            .category("math")
            .doc(
                "Where a value sits between two others, as a 0..1 factor — \
                 the inverse of `math.mix`. Components where `a == b` give 0.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("a", 0.0))
            .input(generic_splat("b", 1.0))
            .input(generic_splat("value", 0.5))
            .output(generic_socket("out"))
            .expr(guarded(
                "{$T}(0.0)",
                "({value} - {a}) / ({b} - {a})",
                "({b} - {a})",
            )),
    );
    defs.push(
        NodeDefinition::builder("math.remap", "Remap")
            .category("math")
            .doc(
                "Map a value from [in_min, in_max] onto [out_min, out_max]. \
                 Not clamped: feed the result through `math.clamp` if the \
                 input can leave its stated range. A zero-width input range \
                 gives `out_min`.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("value", 0.0))
            .input(generic_splat("in_min", 0.0))
            .input(generic_splat("in_max", 1.0))
            .input(generic_splat("out_min", 0.0))
            .input(generic_splat("out_max", 1.0))
            .output(generic_socket("out"))
            .expr(guarded(
                "{out_min}",
                "{out_min} + ({value} - {in_min}) * ({out_max} - {out_min}) \
                 / ({in_max} - {in_min})",
                "({in_max} - {in_min})",
            )),
    );
    defs.push(
        NodeDefinition::builder("math.wrap", "Wrap")
            .category("math")
            .doc(
                "Wrap a value into [low, high), the way a repeating texture \
                 coordinate does. Unlike `math.modulo` this is correct for \
                 negative inputs: wrapping -0.25 into 0..1 gives 0.75.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("value", 0.0))
            .input(generic_splat("low", 0.0))
            .input(generic_splat("high", 1.0))
            .output(generic_socket("out"))
            .expr(guarded(
                "{low}",
                "{low} + ({high} - {low}) \
                 * fract(({value} - {low}) / ({high} - {low}))",
                "({high} - {low})",
            )),
    );

    defs.push(
        NodeDefinition::builder("math.arctangent2", "Arctangent 2")
            .category("math")
            .doc(
                "Angle of the vector (x, y) in radians, over the full \
                 circle. Component-wise for the vector types, which makes \
                 it a field of angles rather than one angle.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("y", 0.0))
            .input(generic_splat("x", 1.0))
            .output(generic_socket("out"))
            .expr("atan2({y}, {x})"),
    );
    defs
}

/// Vector algebra: the operations that change or collapse dimensionality.
///
/// One generic node each, over the *vector* types — `dot`, `normalize`,
/// `reflect` and `refract` need more than one component to mean anything,
/// so their parameter allows `vec2f | vec3f | vec4f` rather than
/// [`ValueType::FLOATS`]. The reductions (`dot`, `length`, `distance`)
/// output a fixed `f32` however wide the input is, which is a concrete type
/// and not a per-type split.
pub fn vector_nodes() -> Vec<NodeDefinition> {
    vec![
        NodeDefinition::builder("vector.dot", "Dot product")
            .category("vector")
            .doc(
                "Sum of component-wise products; the cosine of the angle \
                 between two unit vectors.",
            )
            .generic_param(shared(ValueType::VECTORS))
            .input(generic_splat("a", 0.0))
            .input(generic_splat("b", 0.0))
            .output(out(ValueType::F32))
            .expr("dot({a}, {b})"),
        NodeDefinition::builder("vector.length", "Length")
            .category("vector")
            .doc("Euclidean length.")
            .generic_param(shared(ValueType::VECTORS))
            .input(generic_splat("v", 0.0))
            .output(out(ValueType::F32))
            .expr("length({v})"),
        NodeDefinition::builder("vector.distance", "Distance")
            .category("vector")
            .doc("Euclidean distance between two points.")
            .generic_param(shared(ValueType::VECTORS))
            .input(generic_splat("a", 0.0))
            .input(generic_splat("b", 0.0))
            .output(out(ValueType::F32))
            .expr("distance({a}, {b})"),
        NodeDefinition::builder("vector.normalize", "Normalize")
            .category("vector")
            .doc(
                "Scale to unit length. A zero-length input gives NaN; use \
                 `math.safe_normalize` where that is possible.",
            )
            .generic_param(shared(ValueType::VECTORS))
            .input(generic_splat("v", 0.0))
            .output(generic_socket("out"))
            .expr("normalize({v})"),
        NodeDefinition::builder("vector.reflect", "Reflect")
            .category("vector")
            .doc(
                "Mirror an incident direction about a normal. Both should be \
                 unit length, and `incident` points *at* the surface.",
            )
            .generic_param(shared(ValueType::VECTORS))
            .input(generic_splat("incident", 0.0))
            .input(generic_splat("normal", 0.0))
            .output(generic_socket("out"))
            .expr("reflect({incident}, {normal})"),
        NodeDefinition::builder("vector.refract", "Refract")
            .category("vector")
            .doc(
                "Bend an incident direction through a surface. `eta` is the \
                 ratio of refractive indices; total internal reflection \
                 returns the zero vector.",
            )
            .generic_param(shared(ValueType::VECTORS))
            .input(generic_splat("incident", 0.0))
            .input(generic_splat("normal", 0.0))
            .input(scalar("eta", 1.0 / 1.5))
            .output(generic_socket("out"))
            .expr("refract({incident}, {normal}, {eta})"),
        NodeDefinition::builder("vector.transform", "Transform by matrix")
            .category("vector")
            .doc(
                "Multiply a vector by a matrix of its own size — the way to \
                 use a basis from `space.tangent_basis`, or any other \
                 frame, on a direction. `M` and `V` resolve independently, \
                 so a mat3x3f with a vec4f is reported rather than \
                 silently accepted.",
            )
            .generic_param(param("M", ValueType::MATRICES))
            // Not every vector: `Product` never combines a matrix with a
            // `vec2f`, so offering one would be offering a type that can
            // never resolve — and `V`'s first allowed type is what a freshly
            // placed node starts at, which has to combine with `M`'s.
            .generic_param(param("V", &[ValueType::Vec3, ValueType::Vec4]))
            .input(
                Socket::new("m", ValueType::Mat3)
                    .generic("M")
                    .with_splat_default(1.0),
            )
            .input(
                Socket::new("v", ValueType::Vec3)
                    .generic("V")
                    .with_splat_default(1.0),
            )
            .output(Socket::new("out", ValueType::Vec3).combine(TypeRule::Product, "M", "V"))
            .expr("{m} * {v}"),
        // `cross` is the one vector operation WGSL defines for exactly one
        // width, so this is a concrete node rather than a generic one with
        // a single allowed type.
        NodeDefinition::builder("vector.cross", "Cross product")
            .category("vector")
            .doc("The vector perpendicular to both inputs, right-handed.")
            .input(direction("a", [1.0, 0.0, 0.0]))
            .input(direction("b", [0.0, 1.0, 0.0]))
            .output(out(ValueType::Vec3))
            .expr("cross({a}, {b})"),
    ]
}

/// Conversions between scalars and vectors: splat, combine, split.
///
/// Sockets are matched by exact type, so these are how a graph changes
/// dimensionality — no implicit promotion happens behind the author's back.
///
/// `convert.combine` and `convert.split` are the two families in this
/// library that stay one node *per* type: how many sockets they have is
/// part of what the type is (a `vec4f` split has four outputs, a `vec2f`
/// split has two), and a [`GenericParam`] resolves a socket's type, not a
/// node's socket list. `convert.splat` has the same one input and one
/// output at every width, so it is generic like everything else — with
/// `{$T}` naming the resolved type in the constructor it emits.
pub fn convert_nodes() -> Vec<NodeDefinition> {
    let component_names = ["x", "y", "z", "w"];
    let mut defs = Vec::new();

    defs.push(
        NodeDefinition::builder("convert.splat", "Splat")
            .category("convert")
            .doc("Copy one scalar into every component.")
            .generic_param(shared(ValueType::VECTORS))
            .input(scalar("value", 0.0))
            .output(generic_socket("out"))
            .expr("{$T}({value})"),
    );

    for ty in [ValueType::Vec2, ValueType::Vec3, ValueType::Vec4] {
        let count = ty.component_count().expect("float vector") as usize;

        let mut combine =
            NodeDefinition::builder(format!("convert.combine.{}", ty.suffix()), "Combine")
                .category("convert")
                .doc("Build a vector from its components.");
        let mut expr = format!("{}(", ty.wxsl_type());
        for (index, name) in component_names[..count].iter().enumerate() {
            // Alpha defaults to 1, so a colour built without it is opaque.
            let default = if ty == ValueType::Vec4 && index == 3 {
                1.0
            } else {
                0.0
            };
            combine = combine.input(scalar(name, default));
            if index > 0 {
                expr.push_str(", ");
            }
            expr.push('{');
            expr.push_str(name);
            expr.push('}');
        }
        expr.push(')');
        defs.push(combine.output(out(ty)).expr(expr));

        let mut split = NodeDefinition::builder(format!("convert.split.{}", ty.suffix()), "Split")
            .category("convert")
            .doc("Take a vector apart into its components.")
            .input(splat("v", ty, 0.0));
        let mut exprs = Vec::with_capacity(count);
        for name in &component_names[..count] {
            split = split.output(Socket::new(name, ValueType::F32));
            exprs.push(format!("{{v}}.{name}"));
        }
        defs.push(split.exprs(exprs));
    }
    defs
}

/// Comparison and boolean logic, plus the `select` that makes a `bool`
/// useful in a graph without branching.
pub fn logic_nodes() -> Vec<NodeDefinition> {
    let comparisons: &[(&str, &str, &str)] = &[
        ("less", "Less than", "{a} < {b}"),
        ("less_equal", "Less or equal", "{a} <= {b}"),
        ("greater", "Greater than", "{a} > {b}"),
        ("greater_equal", "Greater or equal", "{a} >= {b}"),
        ("equal", "Equal", "{a} == {b}"),
        ("not_equal", "Not equal", "{a} != {b}"),
    ];
    let mut defs = Vec::new();
    for (stem, label, expr) in comparisons {
        defs.push(
            // Scalar-only, and honestly so rather than as a family with
            // one member: WGSL's comparison operators on vectors give a
            // `vecN<bool>`, which `ValueType` does not carry, so there is
            // no type for a generic output to resolve to.
            NodeDefinition::builder(format!("compare.{stem}"), *label)
                .category("compare")
                .doc(
                    "Compare two scalars. Exact float comparison is rarely \
                      what you want for equality; compare a difference \
                      against a tolerance instead.",
                )
                .input(scalar("a", 0.0))
                .input(scalar("b", 0.0))
                .output(out(ValueType::Bool))
                .expr(*expr),
        );
    }

    let boolean =
        |name: &str, label: &'static str, doc: &'static str, expr: &'static str, binary: bool| {
            let mut builder = NodeDefinition::builder(format!("logic.{name}"), label)
                .category("logic")
                .doc(doc)
                .input(Socket::new("a", ValueType::Bool).with_default(Value::Bool(false)));
            if binary {
                builder = builder
                    .input(Socket::new("b", ValueType::Bool).with_default(Value::Bool(false)));
            }
            builder.output(out(ValueType::Bool)).expr(expr)
        };
    defs.push(boolean(
        "and",
        "And",
        "True when both inputs are true.",
        "{a} && {b}",
        true,
    ));
    defs.push(boolean(
        "or",
        "Or",
        "True when either input is true.",
        "{a} || {b}",
        true,
    ));
    defs.push(boolean("not", "Not", "Invert a boolean.", "!{a}", false));

    defs.push(
        NodeDefinition::builder("logic.select", "Select")
            .category("logic")
            .doc(
                "Pick one of two values by a condition. Both inputs are \
                 evaluated — this is a data selection, not a branch.",
            )
            .generic_param(shared(ValueType::FLOATS))
            .input(generic_splat("if_false", 0.0))
            .input(generic_splat("if_true", 1.0))
            .input(Socket::new("condition", ValueType::Bool).with_default(Value::Bool(false)))
            .output(generic_socket("out"))
            .expr("select({if_false}, {if_true}, {condition})"),
    );
    defs
}

/// Literal values: one generic node, plus `bool`.
///
/// A constant node earns its place over typing the value into the consuming
/// socket when several sockets should share one value: change it once and
/// every consumer follows.
pub fn constant_nodes() -> Vec<NodeDefinition> {
    vec![
        NodeDefinition::builder("const.value", "Constant")
            .category("const")
            .doc(
                "A literal value, shared by everything wired to it. A \
                 matrix constant defaults to the identity, which is the \
                 diagonal a splat default builds.",
            )
            .generic_param(shared(&ValueType::operands()))
            .input(generic_splat("value", 1.0))
            .output(generic_socket("out"))
            .expr("{value}"),
        // `bool` has no float components, so it is not one of `T`'s allowed
        // types and cannot share the node above; a splat default has
        // nothing to build there either.
        NodeDefinition::builder("const.bool", "Constant (boolean)")
            .category("const")
            .doc("A literal boolean.")
            .input(Socket::new("value", ValueType::Bool).with_default(Value::Bool(false)))
            .output(out(ValueType::Bool))
            .expr("{value}"),
    ]
}

// ---------------------------------------------------------------------------
// Function nodes
// ---------------------------------------------------------------------------

/// The `math/` functions: the ones with a body, which WGSL does not provide.
pub fn math_function_nodes() -> Vec<NodeDefinition> {
    vec![template_node(
        "math.smootherstep",
        "Smootherstep",
        "Quintic ramp between two edges, with a continuous second \
         derivative — no crease where the ramp meets the flat parts.",
        ValueType::FLOATS,
        WxslFunction::new(
            "package::math::smootherstep",
            "smootherstep",
            vec![
                generic_splat("edge0", 0.0),
                generic_splat("edge1", 1.0),
                generic_splat("x", 0.5),
            ],
            generic_socket("out"),
        ),
    )]
}

/// `math.safe_normalize`: one node, over the vector types.
///
/// Unlike the range operators, this one keeps a WXSL body. It is a
/// *reduction* — `dot(v, v)` collapses to a scalar whatever the input width
/// — so its guard is a scalar `if` that generalizes unchanged, and there is
/// a real intermediate worth naming. WGSL has no function overloading, but
/// WXSL has templates: one `fn safe_normalize<T: vec2f | vec3f | vec4f>`,
/// which the compiler instantiates per type actually used
/// ([ADR 0012](../../../docs/adr/0012-monomorphize-templates-on-the-flat-module.md)).
fn safe_normalize_nodes() -> Vec<NodeDefinition> {
    vec![template_node(
        "math.safe_normalize",
        "Safe normalize",
        "Normalize, returning zero instead of NaN for a zero-length \
         input. A NaN here spreads through everything downstream and \
         shows up as black or missing pixels far from its cause.",
        ValueType::VECTORS,
        WxslFunction::new(
            "package::math::safe_normalize",
            "safe_normalize",
            vec![generic_splat("v", 0.0)],
            generic_socket("out"),
        ),
    )]
}

/// The `color/` functions: transfer functions, tonemaps, colour spaces.
pub fn color_nodes() -> Vec<NodeDefinition> {
    vec![
        function_node(
            "color.srgb_to_linear",
            "sRGB to linear",
            "Decode an sRGB colour (what a colour picker gives you) to the \
             linear light every lighting computation expects.",
            "package::color::srgb_to_linear",
            "srgb_to_linear",
            vec![color_socket("color", [1.0, 1.0, 1.0])],
            out(ValueType::Vec3),
        ),
        function_node(
            "color.linear_to_srgb",
            "Linear to sRGB",
            "Encode linear light as sRGB, for writing to a non-`Srgb` format.",
            "package::color::linear_to_srgb",
            "linear_to_srgb",
            vec![color_socket("color", [1.0, 1.0, 1.0])],
            out(ValueType::Vec3),
        ),
        function_node(
            "color.luminance",
            "Luminance",
            "Relative luminance of a linear colour (Rec. 709 weights).",
            "package::color::luminance",
            "luminance",
            vec![color_socket("color", [1.0, 1.0, 1.0])],
            out(ValueType::F32),
        ),
        function_node(
            "color.tonemap_reinhard",
            "Tonemap (Reinhard)",
            "Compress high dynamic range on luminance, preserving hue.",
            "package::color::tonemap_reinhard",
            "tonemap_reinhard",
            vec![color_socket("color", [1.0, 1.0, 1.0])],
            out(ValueType::Vec3),
        ),
        function_node(
            "color.tonemap_filmic",
            "Tonemap (filmic)",
            "Filmic curve with a gentle toe, a long shoulder and highlight \
             desaturation. Also what the ABI applies when `wxsl_tonemap` \
             is on.",
            "package::color::tonemap_filmic",
            "tonemap_filmic",
            vec![color_socket("color", [1.0, 1.0, 1.0])],
            out(ValueType::Vec3),
        ),
        function_node(
            "color.hsv_to_rgb",
            "HSV to RGB",
            "Hue (in turns), saturation and value to RGB.",
            "package::color::hsv_to_rgb",
            "hsv_to_rgb",
            vec![Socket::new("hsv", ValueType::Vec3).with_default(Value::Vec3([0.0, 1.0, 1.0]))],
            out(ValueType::Vec3),
        ),
        function_node(
            "color.rgb_to_hsv",
            "RGB to HSV",
            "RGB to hue (in turns), saturation and value.",
            "package::color::rgb_to_hsv",
            "rgb_to_hsv",
            vec![color_socket("rgb", [1.0, 0.0, 0.0])],
            out(ValueType::Vec3),
        ),
    ]
}

/// The `space/` functions: tangent frames and coordinate manipulation.
pub fn space_nodes() -> Vec<NodeDefinition> {
    vec![
        function_node(
            "space.apply_normal_map",
            "Apply normal map",
            "Take a tangent-space normal into world space, with a strength \
             control. Wire the result into the output node's `normal`.",
            "package::space::apply_normal_map",
            "apply_normal_map",
            vec![
                direction("tangent_normal", [0.0, 0.0, 1.0]),
                direction("normal", [0.0, 1.0, 0.0]),
                direction("tangent", [1.0, 0.0, 0.0]),
                direction("bitangent", [0.0, 0.0, 1.0]),
                scalar("strength", 1.0),
            ],
            out(ValueType::Vec3),
        ),
        function_node(
            "space.tangent_basis",
            "Tangent basis",
            "An arbitrary but stable orthonormal frame around a normal, as \
             the columns (tangent, bitangent, normal).",
            "package::space::tangent_basis",
            "tangent_basis",
            vec![direction("normal", [0.0, 1.0, 0.0])],
            out(ValueType::Mat3),
        ),
        function_node(
            "space.rotate_uv",
            "Rotate UV",
            "Rotate a coordinate around a pivot, in turns.",
            "package::space::rotate_uv",
            "rotate_uv",
            vec![
                Socket::new("uv", ValueType::Vec2).with_splat_default(0.0),
                Socket::new("pivot", ValueType::Vec2).with_splat_default(0.5),
                scalar("turns", 0.0),
            ],
            out(ValueType::Vec2),
        ),
    ]
}

/// The `generative/` functions: hashes and noise.
///
/// `generative.fbm3` is where macro variables show up in the node library:
/// the octave count is a loop bound in the WXSL, so it has to be a
/// compile-time constant rather than a socket. Declaring it here is what puts
/// it in the graph's editable macro set.
pub fn generative_nodes() -> Vec<NodeDefinition> {
    vec![
        function_node(
            "generative.hash13",
            "Hash (3 to 1)",
            "A deterministic pseudo-random value in 0..1 for a point.",
            "package::generative::hash13",
            "hash13",
            vec![Socket::new("p", ValueType::Vec3).with_splat_default(0.0)],
            out(ValueType::F32),
        ),
        function_node(
            "generative.value_noise3",
            "Value noise (3D)",
            "Smooth 0..1 noise over a lattice, quintically interpolated.",
            "package::generative::value_noise3",
            "value_noise3",
            vec![Socket::new("p", ValueType::Vec3).with_splat_default(0.0)],
            out(ValueType::F32),
        ),
        NodeDefinition::from_function(
            "generative.fbm3",
            "Fractal noise (3D)",
            "Octaves of value noise summed at rising frequency and falling \
             amplitude, normalized to 0..1. The octave count is the macro \
             variable `WXSL_FBM_OCTAVES`, and `wxsl_fbm_ridged` \
             switches the octaves to ridges — both change which code is \
             compiled, not what a socket carries.",
            WxslFunction::new(
                "package::generative::fbm3",
                "fbm3",
                vec![
                    Socket::new("p", ValueType::Vec3).with_splat_default(0.0),
                    scalar("lacunarity", 2.0).with_doc("Frequency multiplier per octave."),
                    scalar("gain", 0.5).with_doc("Amplitude multiplier per octave."),
                ],
                out(ValueType::F32),
            ),
        )
        .with_macros(vec![
            MacroDef::new(
                "WXSL_FBM_OCTAVES",
                MacroValue::Int(5),
                "How many octaves of noise to sum. A loop bound, so it is a \
                 compile-time constant.",
            ),
            MacroDef::new(
                "wxsl_fbm_ridged",
                MacroValue::Flag(false),
                "Fold each octave into a ridge, for a creased look.",
            ),
        ]),
    ]
}

/// The `lighting/` functions: the PBR building blocks and the two composite
/// shading functions built from them.
pub fn lighting_nodes() -> Vec<NodeDefinition> {
    let pbr_params = || {
        vec![
            direction("normal", [0.0, 1.0, 0.0]).with_doc("Unit shading normal, world space."),
            direction("view_direction", [0.0, 0.0, 1.0]).with_doc("Unit vector towards the eye."),
            direction("light_direction", [0.0, 1.0, 0.0])
                .with_doc("Unit vector towards the light."),
            color_socket("radiance", [1.0, 1.0, 1.0])
                .with_doc("The light's radiance, already attenuated for distance."),
            color_socket("base_color", [0.8, 0.8, 0.8]),
            scalar("metallic", 0.0),
            scalar("roughness", 0.5),
        ]
    };

    vec![
        function_node(
            "lighting.distribution_ggx",
            "GGX distribution",
            "Microfacet normal distribution: how much of the surface faces \
             the half vector.",
            "package::lighting::distribution_ggx",
            "distribution_ggx",
            vec![scalar("n_dot_h", 1.0), scalar("roughness", 0.5)],
            out(ValueType::F32),
        ),
        function_node(
            "lighting.visibility_smith",
            "Smith visibility",
            "Height-correlated masking and shadowing, already divided by the \
             microfacet BRDF's denominator.",
            "package::lighting::visibility_smith",
            "visibility_smith",
            vec![
                scalar("n_dot_v", 1.0),
                scalar("n_dot_l", 1.0),
                scalar("roughness", 0.5),
            ],
            out(ValueType::F32),
        ),
        function_node(
            "lighting.fresnel_schlick",
            "Fresnel (Schlick)",
            "Reflectance as a function of viewing angle.",
            "package::lighting::fresnel_schlick",
            "fresnel_schlick",
            vec![
                color_socket("f0", [0.04, 0.04, 0.04]).with_doc("Reflectance at normal incidence."),
                scalar("cos_theta", 1.0),
            ],
            out(ValueType::Vec3),
        ),
        function_node(
            "lighting.diffuse_lambert",
            "Lambert diffuse",
            "Energy-normalized diffuse albedo, faded out for metals.",
            "package::lighting::diffuse_lambert",
            "diffuse_lambert",
            vec![
                color_socket("base_color", [0.8, 0.8, 0.8]),
                scalar("metallic", 0.0),
            ],
            out(ValueType::Vec3),
        ),
        function_node(
            "lighting.pbr_direct",
            "PBR direct light",
            "One light's full contribution to a physically-based surface: \
             GGX specular plus Lambert diffuse, Fresnel-balanced.",
            "package::lighting::pbr_direct",
            "pbr_direct",
            pbr_params(),
            out(ValueType::Vec3),
        ),
        NodeDefinition::from_function(
            "lighting.pbr_direct_split",
            "PBR direct light (split)",
            "As `lighting.pbr_direct`, but keeping the diffuse and specular \
             halves separate. The function is evaluated once however many of \
             its outputs are used.",
            WxslFunction::new_struct(
                "package::lighting::pbr_direct_split",
                "pbr_direct_split",
                pbr_params(),
                "PbrDirect",
                vec![
                    Socket::new("diffuse", ValueType::Vec3),
                    Socket::new("specular", ValueType::Vec3),
                ],
            ),
        ),
        function_node(
            "lighting.ambient_environment",
            "Ambient environment",
            "Analytic hemisphere ambient: sky above, bounce below, with a \
             roughness-aware specular response.",
            "package::lighting::ambient_environment",
            "ambient_environment",
            vec![
                direction("normal", [0.0, 1.0, 0.0]),
                direction("view_direction", [0.0, 0.0, 1.0]),
                color_socket("base_color", [0.8, 0.8, 0.8]),
                scalar("metallic", 0.0),
                scalar("roughness", 0.5),
                color_socket("sky_color", [0.35, 0.45, 0.6]),
                color_socket("ground_color", [0.12, 0.1, 0.08]),
            ],
            out(ValueType::Vec3),
        ),
    ]
}

/// The `sdf/` functions: signed distance primitives and a smooth union.
pub fn sdf_nodes() -> Vec<NodeDefinition> {
    vec![
        function_node(
            "sdf.sphere",
            "SDF sphere",
            "Signed distance to a sphere at the origin.",
            "package::sdf::sphere",
            "sdf_sphere",
            vec![
                Socket::new("p", ValueType::Vec3).with_splat_default(0.0),
                scalar("radius", 0.5),
            ],
            out(ValueType::F32),
        ),
        function_node(
            "sdf.box",
            "SDF box",
            "Signed distance to an axis-aligned box at the origin.",
            "package::sdf::box",
            "sdf_box",
            vec![
                Socket::new("p", ValueType::Vec3).with_splat_default(0.0),
                Socket::new("half_extents", ValueType::Vec3).with_splat_default(0.5),
            ],
            out(ValueType::F32),
        ),
        function_node(
            "sdf.smooth_union",
            "SDF smooth union",
            "Union of two fields with a rounded seam of width `k`.",
            "package::sdf::smooth_union",
            "sdf_smooth_union",
            vec![scalar("a", 0.0), scalar("b", 0.0), scalar("k", 0.1)],
            out(ValueType::F32),
        ),
    ]
}

/// The `animation/` functions: time-driven shaping.
pub fn animation_nodes() -> Vec<NodeDefinition> {
    vec![
        function_node(
            "animation.pulse",
            "Pulse",
            "A 0..1 sine oscillation. Wire the `input.time` node into it.",
            "package::animation::pulse",
            "pulse",
            vec![
                scalar("time", 0.0),
                scalar("frequency", 1.0).with_doc("Cycles per second."),
                scalar("phase", 0.0).with_doc("Offset in turns."),
            ],
            out(ValueType::F32),
        ),
        function_node(
            "animation.ease_in_out_cubic",
            "Ease in-out (cubic)",
            "Cubic ease over 0..1, clamped outside it.",
            "package::animation::ease_in_out_cubic",
            "ease_in_out_cubic",
            vec![scalar("t", 0.0)],
            out(ValueType::F32),
        ),
    ]
}

/// The `distort/` functions: coordinate distortions.
pub fn distort_nodes() -> Vec<NodeDefinition> {
    vec![function_node(
        "distort.swirl_uv",
        "Swirl UV",
        "Rotate a coordinate around a centre by an amount that falls off with \
         distance.",
        "package::distort::swirl_uv",
        "swirl_uv",
        vec![
            Socket::new("uv", ValueType::Vec2).with_splat_default(0.0),
            Socket::new("center", ValueType::Vec2).with_splat_default(0.5),
            scalar("radius", 0.5),
            scalar("turns", 0.25),
        ],
        out(ValueType::Vec2),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders;
    use wxsl_core::graph::{Graph, Node};
    use wxsl_core::node::NodeBody;

    #[test]
    fn registry_has_no_duplicate_ids() {
        // `register_all` panics on a duplicate, so building it is the test.
        let registry = registry();
        assert_eq!(registry.len(), all_nodes().len());
    }

    #[test]
    fn every_operator_family_is_exactly_one_node_covering_every_type_it_allows() {
        // One registry entry per family — not one per family *and* type,
        // which is the duplication generic sockets exist to remove — and
        // every socket's type governed by a parameter rather than fixed.
        let registry = registry();
        for (id, params, allowed) in BINARY
            .iter()
            .map(|family| {
                let params: &[&str] = match family.rule {
                    None => &["T"],
                    Some(_) => &["A", "B"],
                };
                let allowed = if family.matrices {
                    ValueType::operands()
                } else {
                    ValueType::FLOATS.to_vec()
                };
                (format!("math.{}", family.stem), params, allowed)
            })
            .chain(UNARY.iter().map(|family| {
                (
                    format!("math.{}", family.stem),
                    &["T"][..],
                    ValueType::FLOATS.to_vec(),
                )
            }))
        {
            let def = registry
                .get(&id)
                .unwrap_or_else(|| panic!("missing `{id}`"));
            for suffix in ValueType::FLOATS {
                assert!(
                    !registry.contains(&format!("{id}.{}", suffix.suffix())),
                    "`{id}.{suffix}` should not exist alongside the generic `{id}`"
                );
            }
            assert_eq!(
                def.generics
                    .iter()
                    .map(|param| param.name.as_str())
                    .collect::<Vec<_>>(),
                params,
                "`{id}` declares the wrong type parameters"
            );
            for param in &def.generics {
                assert_eq!(
                    param.allowed, allowed,
                    "`{id}`'s `{}` allows the wrong types",
                    param.name
                );
            }
            for socket in def.inputs.iter().chain(&def.outputs) {
                assert!(
                    socket.referenced_params().next().is_some(),
                    "`{id}`'s socket `{}` has a fixed type, not a parameter",
                    socket.name
                );
                assert!(
                    socket.default.is_none(),
                    "`{id}`'s socket `{}` is generic and must have no fixed default",
                    socket.name
                );
            }
        }
    }

    #[test]
    fn the_operators_whose_operands_may_differ_declare_two_parameters() {
        // `+`, `-`, `*`, `/` and `%` all spread a scalar over a vector, so
        // their two operands resolve independently and the result is
        // derived from both. `pow`/`min`/`max` are builtins with a single
        // `(T, T) -> T` overload, so they share one parameter.
        let registry = registry();
        let rule_of = |id: &str| {
            registry
                .get(id)
                .unwrap_or_else(|| panic!("missing `{id}`"))
                .output("out")
                .expect("has an `out` output")
                .combine
                .as_ref()
                .map(|combined| {
                    (
                        combined.rule,
                        combined.a.to_string(),
                        combined.b.to_string(),
                    )
                })
        };
        for id in ["math.add", "math.subtract", "math.divide", "math.modulo"] {
            assert_eq!(
                rule_of(id),
                Some((TypeRule::Componentwise, "A".into(), "B".into())),
                "`{id}`'s output should combine `A` and `B` componentwise"
            );
        }
        assert_eq!(
            rule_of("math.multiply"),
            Some((TypeRule::Product, "A".into(), "B".into())),
            "`*` also does linear algebra, which `Componentwise` does not cover"
        );
        for id in ["math.power", "math.minimum", "math.maximum"] {
            assert_eq!(rule_of(id), None, "`{id}` shares one parameter");
        }

        // Matrices are operands of `+`, `-` and `*` and of nothing else.
        for id in ["math.add", "math.subtract", "math.multiply"] {
            let def = registry.get(id).unwrap();
            assert!(def
                .generics
                .iter()
                .all(|param| param.allowed.contains(&ValueType::Mat3)));
        }
        for id in ["math.divide", "math.modulo", "math.power", "math.negate"] {
            let def = registry.get(id).unwrap();
            assert!(
                def.generics
                    .iter()
                    .all(|param| !param.allowed.contains(&ValueType::Mat3)),
                "`{id}` takes no matrix in WGSL"
            );
        }
    }

    #[test]
    fn no_definition_id_carries_a_type_suffix_a_parameter_could_replace() {
        // Two exceptions. `convert.combine`/`convert.split` have a socket
        // *count* that is part of the type, so one node cannot serve every
        // width. `const.bool` names a type that is not in any parameter's
        // allowed set — `bool` is not a float, and a splat default has
        // nothing to spread there. Everything else that was once a per-type
        // family is one node now.
        let allowed_per_type = ["convert.combine.", "convert.split.", "const.bool"];
        for def in all_nodes() {
            let suffixed = ValueType::ALL
                .iter()
                .any(|ty| def.id.ends_with(&format!(".{}", ty.suffix())));
            assert!(
                !suffixed || allowed_per_type.iter().any(|stem| def.id.starts_with(stem)),
                "`{}` still names a type in its id",
                def.id
            );
        }
    }

    #[test]
    fn a_splat_default_on_a_generic_socket_follows_the_resolved_type() {
        // `clamp`'s `high` is 1 at every width: one scalar on the
        // definition, spread over whatever the instance resolved to.
        let registry = registry();
        let high = registry
            .get("math.clamp")
            .expect("registered")
            .input("high")
            .expect("has a `high` input");
        assert_eq!(high.default, None, "no fixed default on a generic socket");
        assert_eq!(high.splat_default, Some(1.0));
        assert_eq!(high.default_for(ValueType::F32), Some(Value::F32(1.0)));
        assert_eq!(
            high.default_for(ValueType::Vec3),
            Some(Value::Vec3([1.0; 3]))
        );
        assert!(!high.is_required(), "a splat default feeds the socket");
    }

    #[test]
    fn every_definition_is_valid_at_its_default_types() {
        // `NodeDefinition::default_generics` is what a node placed on a
        // canvas starts at, so those types have to actually work together:
        // `vector.transform` starting at `mat3x3f` with a `vec2f` would be
        // a node that reports `IncompatibleGenerics` before anyone touched
        // it. This is the constraint on the *order* of every
        // `GenericParam::allowed` in this file.
        let registry = registry();
        let mut graph = Graph::new("defaults");
        for def in registry.iter() {
            if def.is_surface_output() {
                continue;
            }
            let node = graph.add_resolved(&registry, Node::new(def.id.clone()));
            for socket in def.inputs.iter().chain(&def.outputs) {
                assert!(
                    graph.effective_type(node, socket).is_some(),
                    "`{}`'s socket `{}` has no type at the default {:?}",
                    def.id,
                    socket.name,
                    def.default_generics()
                );
            }
        }
        // Every one of them is complete on its own: no unresolved
        // parameter, no unfed input, nothing to report.
        graph
            .validate(&registry)
            .expect("a freshly placed node of every kind is valid");
    }

    #[test]
    fn every_called_function_lives_in_a_module_this_crate_ships() {
        // Catches a descriptor pointing at a module path that does not
        // exist — which would otherwise only surface as a shader-compiler
        // "module not found" much later.
        for def in all_nodes() {
            if let NodeBody::Call(func) = &def.body {
                assert!(
                    shaders::module(func.module.as_str()).is_some(),
                    "`{}` calls into unknown module `{}`",
                    def.id,
                    func.module
                );
                let source = shaders::module(func.module.as_str()).unwrap();
                // `fn name(` for an ordinary function, `fn name<` for a
                // template (`fn safe_normalize<T: vec2f | …>`).
                assert!(
                    source.contains(&format!("fn {}(", func.name))
                        || source.contains(&format!("fn {}<", func.name)),
                    "`{}` declares `{}` but `{}` has no such function",
                    def.id,
                    func.signature(),
                    func.module
                );
            }
        }
    }

    #[test]
    fn socket_defaults_match_socket_types() {
        for def in all_nodes() {
            for socket in def.inputs.iter().chain(&def.outputs) {
                if let Some(default) = socket.default {
                    assert_eq!(
                        default.ty(),
                        socket.ty,
                        "`{}`'s socket `{}` has a {} default",
                        def.id,
                        socket.name,
                        default.ty()
                    );
                }
            }
        }
    }

    #[test]
    fn expression_placeholders_name_real_sockets() {
        for def in all_nodes() {
            let NodeBody::Expr(exprs) = &def.body else {
                continue;
            };
            for expr in exprs {
                let mut rest = expr.as_str();
                while let Some(open) = rest.find('{') {
                    rest = &rest[open + 1..];
                    let close = rest.find('}').expect("closed placeholder");
                    let name = &rest[..close];
                    // `{$T}` names a generic parameter, not a socket; the
                    // node builder already checks those are declared.
                    if let Some(param) = name.strip_prefix('$') {
                        assert!(
                            def.generic(param).is_some(),
                            "`{}` references `{{${param}}}`, which it does not declare",
                            def.id
                        );
                    } else {
                        assert!(
                            def.input(name).is_some(),
                            "`{}` references `{{{name}}}`, which is not an input",
                            def.id
                        );
                    }
                    rest = &rest[close + 1..];
                }
            }
        }
    }
}
