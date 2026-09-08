//! Every stdlib function and operator as a `wesloom-core` node definition.
//!
//! Two kinds of node live here, and the split is deliberate:
//!
//! * **Operators** ([`math_nodes`], [`vector_nodes`], [`convert_nodes`],
//!   [`logic_nodes`], [`constant_nodes`]) are inline WESL expressions over
//!   WGSL's own built-ins — `{a} + {b}`, `mix({a}, {b}, {t})`. Wrapping an
//!   addition in a function call would cost a WESL module, an import and a
//!   call per node for no gain, so these are generated per value type from
//!   one table.
//! * **Functions** (everything else: [`color_nodes`], [`lighting_nodes`],
//!   [`generative_nodes`], …) are calls to the `.wesl` functions this crate
//!   ships, each described by a
//!   [`wesloom_core::node::WeslFunction`] giving its module,
//!   name, parameters and return shape. The WESL source stays the single
//!   definition of the behaviour; the descriptor here is what lets the graph
//!   type-check a call to it and lets codegen emit one
//!   ([ADR 0003](../../../docs/adr/0003-wesl-as-the-shading-language.md)).
//!
//! A function whose WESL reads a macro variable declares that macro on its
//! node definition (see [`generative_nodes`] and `shaders/generative/fbm3.wesl`).
//! That is what puts the macro in a graph's editable macro set, and what
//! guarantees the generated macro module declares it whenever the function is
//! reachable.

use wesloom_core::abi;
use wesloom_core::macros::{MacroDef, MacroValue};
use wesloom_core::node::{NodeDefinition, NodeRegistry, Socket, Value, ValueType, WeslFunction};

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

/// Build a node that calls a single-value WESL function.
fn function_node(
    id: &str,
    label: &str,
    doc: &str,
    module: &str,
    name: &str,
    params: Vec<Socket>,
    ret: Socket,
) -> NodeDefinition {
    NodeDefinition::from_function(id, label, doc, WeslFunction::new(module, name, params, ret))
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

/// A per-type operator family: one node per float value type.
struct Family {
    /// Id stem, e.g. `add` in `math.add.vec3f`.
    stem: &'static str,
    /// Editor label.
    label: &'static str,
    /// Description.
    doc: &'static str,
    /// Expression template over the family's socket names.
    expr: &'static str,
    /// Default for the operand sockets: 0 for additive operators, 1 for
    /// multiplicative ones, so an unconnected input is a no-op.
    identity: f32,
}

const BINARY: &[Family] = &[
    Family {
        stem: "add",
        label: "Add",
        doc: "Component-wise sum.",
        expr: "{a} + {b}",
        identity: 0.0,
    },
    Family {
        stem: "subtract",
        label: "Subtract",
        doc: "Component-wise difference.",
        expr: "{a} - {b}",
        identity: 0.0,
    },
    Family {
        stem: "multiply",
        label: "Multiply",
        doc: "Component-wise product.",
        expr: "{a} * {b}",
        identity: 1.0,
    },
    Family {
        stem: "divide",
        label: "Divide",
        doc: "Component-wise quotient. Division by zero yields an infinity, \
              which will spread; guard the divisor if it can reach zero.",
        expr: "{a} / {b}",
        identity: 1.0,
    },
    Family {
        stem: "modulo",
        label: "Modulo",
        doc: "Component-wise floating-point remainder, keeping the sign of \
              the dividend. For a periodic wrap use `math.wrap` instead.",
        expr: "{a} % {b}",
        identity: 1.0,
    },
    Family {
        stem: "power",
        label: "Power",
        doc: "`a` raised to `b`, component-wise. Undefined for a negative \
              base with a fractional exponent.",
        expr: "pow({a}, {b})",
        identity: 1.0,
    },
    Family {
        stem: "minimum",
        label: "Minimum",
        doc: "Component-wise smaller of the two.",
        expr: "min({a}, {b})",
        identity: 0.0,
    },
    Family {
        stem: "maximum",
        label: "Maximum",
        doc: "Component-wise larger of the two.",
        expr: "max({a}, {b})",
        identity: 0.0,
    },
];

const UNARY: &[Family] = &[
    Family {
        stem: "negate",
        label: "Negate",
        doc: "Flip the sign, component-wise.",
        expr: "-{a}",
        identity: 0.0,
    },
    Family {
        stem: "absolute",
        label: "Absolute",
        doc: "Drop the sign, component-wise.",
        expr: "abs({a})",
        identity: 0.0,
    },
    Family {
        stem: "sign",
        label: "Sign",
        doc: "-1, 0 or 1 per component.",
        expr: "sign({a})",
        identity: 0.0,
    },
    Family {
        stem: "floor",
        label: "Floor",
        doc: "Round down, component-wise.",
        expr: "floor({a})",
        identity: 0.0,
    },
    Family {
        stem: "ceil",
        label: "Ceil",
        doc: "Round up, component-wise.",
        expr: "ceil({a})",
        identity: 0.0,
    },
    Family {
        stem: "round",
        label: "Round",
        doc: "Round to nearest, halves to even.",
        expr: "round({a})",
        identity: 0.0,
    },
    Family {
        stem: "truncate",
        label: "Truncate",
        doc: "Drop the fractional part, towards zero.",
        expr: "trunc({a})",
        identity: 0.0,
    },
    Family {
        stem: "fraction",
        label: "Fraction",
        doc: "The fractional part, always in [0, 1).",
        expr: "fract({a})",
        identity: 0.0,
    },
    Family {
        stem: "saturate",
        label: "Saturate",
        doc: "Clamp to [0, 1], component-wise.",
        expr: "saturate({a})",
        identity: 0.0,
    },
    Family {
        stem: "square_root",
        label: "Square root",
        doc: "Component-wise square root; negative inputs give NaN.",
        expr: "sqrt({a})",
        identity: 1.0,
    },
    Family {
        stem: "inverse_square_root",
        label: "Inverse square root",
        doc: "1/sqrt, component-wise, as a single instruction.",
        expr: "inverseSqrt({a})",
        identity: 1.0,
    },
    Family {
        stem: "exponential",
        label: "Exponential",
        doc: "e raised to the input, component-wise.",
        expr: "exp({a})",
        identity: 0.0,
    },
    Family {
        stem: "exponential_2",
        label: "Exponential (base 2)",
        doc: "2 raised to the input, component-wise.",
        expr: "exp2({a})",
        identity: 0.0,
    },
    Family {
        stem: "logarithm",
        label: "Logarithm",
        doc: "Natural log, component-wise.",
        expr: "log({a})",
        identity: 1.0,
    },
    Family {
        stem: "logarithm_2",
        label: "Logarithm (base 2)",
        doc: "Base-2 log, component-wise.",
        expr: "log2({a})",
        identity: 1.0,
    },
    Family {
        stem: "sine",
        label: "Sine",
        doc: "Sine of an angle in radians.",
        expr: "sin({a})",
        identity: 0.0,
    },
    Family {
        stem: "cosine",
        label: "Cosine",
        doc: "Cosine of an angle in radians.",
        expr: "cos({a})",
        identity: 0.0,
    },
    Family {
        stem: "tangent",
        label: "Tangent",
        doc: "Tangent of an angle in radians.",
        expr: "tan({a})",
        identity: 0.0,
    },
    Family {
        stem: "arcsine",
        label: "Arcsine",
        doc: "Inverse sine, in radians. Input outside [-1, 1] gives NaN.",
        expr: "asin({a})",
        identity: 0.0,
    },
    Family {
        stem: "arccosine",
        label: "Arccosine",
        doc: "Inverse cosine, in radians. Input outside [-1, 1] gives NaN.",
        expr: "acos({a})",
        identity: 0.0,
    },
    Family {
        stem: "arctangent",
        label: "Arctangent",
        doc: "Inverse tangent, in radians.",
        expr: "atan({a})",
        identity: 0.0,
    },
];

/// Arithmetic, one node per float value type.
///
/// Generated rather than listed so `f32` and `vec4f` cannot drift apart, and
/// so adding a value type adds its whole arithmetic set at once.
pub fn math_nodes() -> Vec<NodeDefinition> {
    let mut defs = Vec::new();
    for ty in ValueType::FLOATS {
        for family in BINARY {
            defs.push(
                NodeDefinition::builder(
                    format!("math.{}.{}", family.stem, ty.suffix()),
                    family.label,
                )
                .category("math")
                .doc(family.doc)
                .input(splat("a", *ty, family.identity))
                .input(splat("b", *ty, family.identity))
                .output(out(*ty))
                .expr(family.expr),
            );
        }
        for family in UNARY {
            defs.push(
                NodeDefinition::builder(
                    format!("math.{}.{}", family.stem, ty.suffix()),
                    family.label,
                )
                .category("math")
                .doc(family.doc)
                .input(splat("a", *ty, family.identity))
                .output(out(*ty))
                .expr(family.expr),
            );
        }

        // Operators whose sockets are not interchangeable operands, so they
        // are spelled out rather than generated from the table above.
        defs.push(
            NodeDefinition::builder(format!("math.clamp.{}", ty.suffix()), "Clamp")
                .category("math")
                .doc("Constrain to a range, component-wise.")
                .input(splat("x", *ty, 0.0))
                .input(splat("low", *ty, 0.0))
                .input(splat("high", *ty, 1.0))
                .output(out(*ty))
                .expr("clamp({x}, {low}, {high})"),
        );
        defs.push(
            NodeDefinition::builder(format!("math.mix.{}", ty.suffix()), "Mix")
                .category("math")
                .doc("Linear blend: `a` at t=0, `b` at t=1, extrapolating outside.")
                .input(splat("a", *ty, 0.0))
                .input(splat("b", *ty, 1.0))
                .input(scalar("t", 0.5))
                .output(out(*ty))
                .expr("mix({a}, {b}, {t})"),
        );
        defs.push(
            NodeDefinition::builder(format!("math.step.{}", ty.suffix()), "Step")
                .category("math")
                .doc("0 below the edge, 1 at or above it, component-wise.")
                .input(splat("edge", *ty, 0.5))
                .input(splat("x", *ty, 0.0))
                .output(out(*ty))
                .expr("step({edge}, {x})"),
        );
        defs.push(
            NodeDefinition::builder(format!("math.smoothstep.{}", ty.suffix()), "Smoothstep")
                .category("math")
                .doc(
                    "Hermite ramp between two edges. For a continuous second \
                      derivative use `math.smootherstep`.",
                )
                .input(splat("edge0", *ty, 0.0))
                .input(splat("edge1", *ty, 1.0))
                .input(splat("x", *ty, 0.5))
                .output(out(*ty))
                .expr("smoothstep({edge0}, {edge1}, {x})"),
        );
    }

    defs.push(
        NodeDefinition::builder("math.arctangent2.f32", "Arctangent 2")
            .category("math")
            .doc("Angle of the vector (x, y) in radians, over the full circle.")
            .input(scalar("y", 0.0))
            .input(scalar("x", 1.0))
            .output(out(ValueType::F32))
            .expr("atan2({y}, {x})"),
    );
    defs
}

/// Vector algebra: the operations that change or collapse dimensionality.
pub fn vector_nodes() -> Vec<NodeDefinition> {
    let mut defs = Vec::new();
    for ty in [ValueType::Vec2, ValueType::Vec3, ValueType::Vec4] {
        defs.push(
            NodeDefinition::builder(format!("vector.dot.{}", ty.suffix()), "Dot product")
                .category("vector")
                .doc(
                    "Sum of component-wise products; the cosine of the angle \
                      between two unit vectors.",
                )
                .input(splat("a", ty, 0.0))
                .input(splat("b", ty, 0.0))
                .output(out(ValueType::F32))
                .expr("dot({a}, {b})"),
        );
        defs.push(
            NodeDefinition::builder(format!("vector.length.{}", ty.suffix()), "Length")
                .category("vector")
                .doc("Euclidean length.")
                .input(splat("v", ty, 0.0))
                .output(out(ValueType::F32))
                .expr("length({v})"),
        );
        defs.push(
            NodeDefinition::builder(format!("vector.distance.{}", ty.suffix()), "Distance")
                .category("vector")
                .doc("Euclidean distance between two points.")
                .input(splat("a", ty, 0.0))
                .input(splat("b", ty, 0.0))
                .output(out(ValueType::F32))
                .expr("distance({a}, {b})"),
        );
        defs.push(
            NodeDefinition::builder(format!("vector.normalize.{}", ty.suffix()), "Normalize")
                .category("vector")
                .doc(
                    "Scale to unit length. A zero-length input gives NaN; use \
                      `math.safe_normalize` where that is possible.",
                )
                .input(splat("v", ty, 0.0))
                .output(out(ty))
                .expr("normalize({v})"),
        );
    }

    defs.push(
        NodeDefinition::builder("vector.cross.vec3f", "Cross product")
            .category("vector")
            .doc("The vector perpendicular to both inputs, right-handed.")
            .input(direction("a", [1.0, 0.0, 0.0]))
            .input(direction("b", [0.0, 1.0, 0.0]))
            .output(out(ValueType::Vec3))
            .expr("cross({a}, {b})"),
    );
    defs.push(
        NodeDefinition::builder("vector.transform.mat3", "Transform by matrix")
            .category("vector")
            .doc(
                "Multiply a vector by a 3x3 matrix — the way to use a basis \
                  from `space.tangent_basis`, or any other frame, on a \
                  direction.",
            )
            .input(Socket::new("m", ValueType::Mat3).with_default(Value::Mat3([
                1.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, //
                0.0, 0.0, 1.0,
            ])))
            .input(direction("v", [0.0, 0.0, 1.0]))
            .output(out(ValueType::Vec3))
            .expr("{m} * {v}"),
    );
    defs.push(
        NodeDefinition::builder("vector.reflect.vec3f", "Reflect")
            .category("vector")
            .doc(
                "Mirror an incident direction about a normal. Both should be \
                  unit length, and `incident` points *at* the surface.",
            )
            .input(direction("incident", [0.0, -1.0, 0.0]))
            .input(direction("normal", [0.0, 1.0, 0.0]))
            .output(out(ValueType::Vec3))
            .expr("reflect({incident}, {normal})"),
    );
    defs.push(
        NodeDefinition::builder("vector.refract.vec3f", "Refract")
            .category("vector")
            .doc(
                "Bend an incident direction through a surface. `eta` is the \
                  ratio of refractive indices; total internal reflection \
                  returns the zero vector.",
            )
            .input(direction("incident", [0.0, -1.0, 0.0]))
            .input(direction("normal", [0.0, 1.0, 0.0]))
            .input(scalar("eta", 1.0 / 1.5))
            .output(out(ValueType::Vec3))
            .expr("refract({incident}, {normal}, {eta})"),
    );
    defs
}

/// Conversions between scalars and vectors: splat, combine, split.
///
/// Sockets are matched by exact type, so these are how a graph changes
/// dimensionality — no implicit promotion happens behind the author's back.
pub fn convert_nodes() -> Vec<NodeDefinition> {
    let component_names = ["x", "y", "z", "w"];
    let mut defs = Vec::new();

    for ty in [ValueType::Vec2, ValueType::Vec3, ValueType::Vec4] {
        let count = ty.component_count().expect("float vector") as usize;

        defs.push(
            NodeDefinition::builder(format!("convert.splat.{}", ty.suffix()), "Splat")
                .category("convert")
                .doc("Copy one scalar into every component.")
                .input(scalar("value", 0.0))
                .output(out(ty))
                .expr(format!("{}({{value}})", ty.wesl_type())),
        );

        let mut combine =
            NodeDefinition::builder(format!("convert.combine.{}", ty.suffix()), "Combine")
                .category("convert")
                .doc("Build a vector from its components.");
        let mut expr = format!("{}(", ty.wesl_type());
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
            NodeDefinition::builder(format!("compare.{stem}.f32"), *label)
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

    for ty in ValueType::FLOATS {
        defs.push(
            NodeDefinition::builder(format!("logic.select.{}", ty.suffix()), "Select")
                .category("logic")
                .doc(
                    "Pick one of two values by a condition. Both inputs are \
                      evaluated — this is a data selection, not a branch.",
                )
                .input(splat("if_false", *ty, 0.0))
                .input(splat("if_true", *ty, 1.0))
                .input(Socket::new("condition", ValueType::Bool).with_default(Value::Bool(false)))
                .output(out(*ty))
                .expr("select({if_false}, {if_true}, {condition})"),
        );
    }
    defs
}

/// Literal values, one node per type.
///
/// A constant node earns its place over typing the value into the consuming
/// socket when several sockets should share one value: change it once and
/// every consumer follows.
pub fn constant_nodes() -> Vec<NodeDefinition> {
    let mut defs = Vec::new();
    for ty in ValueType::FLOATS {
        defs.push(
            NodeDefinition::builder(format!("const.{}", ty.suffix()), "Constant")
                .category("const")
                .doc("A literal value, shared by everything wired to it.")
                .input(splat("value", *ty, 0.0))
                .output(out(*ty))
                .expr("{value}"),
        );
    }
    defs.push(
        NodeDefinition::builder("const.bool", "Constant (boolean)")
            .category("const")
            .doc("A literal boolean.")
            .input(Socket::new("value", ValueType::Bool).with_default(Value::Bool(false)))
            .output(out(ValueType::Bool))
            .expr("{value}"),
    );
    defs
}

// ---------------------------------------------------------------------------
// Function nodes
// ---------------------------------------------------------------------------

/// The `math/` functions: the ones with a body, which WGSL does not provide.
pub fn math_function_nodes() -> Vec<NodeDefinition> {
    vec![
        function_node(
            "math.remap.f32",
            "Remap",
            "Map a value from one range onto another. Not clamped.",
            "package::math::remap",
            "remap",
            vec![
                scalar("value", 0.0),
                scalar("in_min", 0.0),
                scalar("in_max", 1.0),
                scalar("out_min", 0.0),
                scalar("out_max", 1.0),
            ],
            out(ValueType::F32),
        ),
        function_node(
            "math.inverse_lerp.f32",
            "Inverse lerp",
            "Where a value sits between two others, as a 0..1 factor.",
            "package::math::inverse_lerp",
            "inverse_lerp",
            vec![scalar("a", 0.0), scalar("b", 1.0), scalar("value", 0.5)],
            out(ValueType::F32),
        ),
        function_node(
            "math.smootherstep.f32",
            "Smootherstep",
            "Quintic ramp between two edges, with a continuous second \
             derivative — no crease where the ramp meets the flat parts.",
            "package::math::smootherstep",
            "smootherstep",
            vec![scalar("edge0", 0.0), scalar("edge1", 1.0), scalar("x", 0.5)],
            out(ValueType::F32),
        ),
        function_node(
            "math.safe_normalize.vec3f",
            "Safe normalize",
            "Normalize, returning zero instead of NaN for a zero-length input.",
            "package::math::safe_normalize",
            "safe_normalize",
            vec![direction("v", [0.0, 1.0, 0.0])],
            out(ValueType::Vec3),
        ),
        function_node(
            "math.wrap.f32",
            "Wrap",
            "Wrap a value into a range, correctly for negative inputs.",
            "package::math::wrap",
            "wrap",
            vec![
                scalar("value", 0.0),
                scalar("low", 0.0),
                scalar("high", 1.0),
            ],
            out(ValueType::F32),
        ),
    ]
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
             desaturation. Also what the ABI applies when `wesloom_tonemap` \
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
/// the octave count is a loop bound in the WESL, so it has to be a
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
             variable `WESLOOM_FBM_OCTAVES`, and `wesloom_fbm_ridged` \
             switches the octaves to ridges — both change which code is \
             compiled, not what a socket carries.",
            WeslFunction::new(
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
                "WESLOOM_FBM_OCTAVES",
                MacroValue::Int(5),
                "How many octaves of noise to sum. A loop bound, so it is a \
                 compile-time constant.",
            ),
            MacroDef::new(
                "wesloom_fbm_ridged",
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
            WeslFunction::new_struct(
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
    use wesloom_core::node::NodeBody;

    #[test]
    fn registry_has_no_duplicate_ids() {
        // `register_all` panics on a duplicate, so building it is the test.
        let registry = registry();
        assert_eq!(registry.len(), all_nodes().len());
    }

    #[test]
    fn every_arithmetic_family_covers_every_float_type() {
        let registry = registry();
        for ty in ValueType::FLOATS {
            for family in BINARY.iter().chain(UNARY) {
                let id = format!("math.{}.{}", family.stem, ty.suffix());
                assert!(registry.contains(&id), "missing `{id}`");
            }
        }
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
                assert!(
                    source.contains(&format!("fn {}(", func.name)),
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
                    assert!(
                        def.input(name).is_some(),
                        "`{}` references `{{{name}}}`, which is not an input",
                        def.id
                    );
                    rest = &rest[close + 1..];
                }
            }
        }
    }
}
