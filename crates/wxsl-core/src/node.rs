//! Node and socket definitions: the typed interface a node kind describes to
//! the graph, and the WXSL it emits.
//!
//! A [`NodeDefinition`] is the *kind* of a node (`math.add`,
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
///
/// # The resource types are here, but not in [`Self::ALL`]
///
/// A texture and a sampler are values in WGSL — handles, passed to
/// `textureSample` and to functions — so an edge can carry one, and
/// `sample.texture_2d` is an ordinary function node whose first two
/// parameters happen to be a texture and a sampler. But they are not values
/// anyone *types in*: there is no [`Value`] for a texture, no zero, no
/// splat, and no arithmetic. So they live in this enum, because a socket's
/// type is one enum everywhere in the repo — the editor, the graph typing,
/// the serialized format — and a parallel socket kind would fork all three;
/// and they are kept out of [`Self::ALL`], so nothing that iterates "every
/// type a value can have" ever offers one
/// ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
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
    /// WGSL `texture_2d<f32>`. A resource, see the type's own docs.
    Texture2d,
    /// WGSL `texture_cube<f32>`. A resource, see the type's own docs.
    TextureCube,
    /// WGSL `sampler`. A resource, see the type's own docs.
    Sampler,
}

impl ValueType {
    /// Every type a [`Value`] can have, in declaration order.
    ///
    /// The resource types are deliberately absent — see the type's own
    /// docs. [`Self::RESOURCES`] is the other half.
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

    /// The types that name a bound resource rather than a value: textures
    /// and samplers.
    ///
    /// Disjoint from [`Self::ALL`], and the two together are every variant.
    pub const RESOURCES: &'static [ValueType] = &[
        ValueType::Texture2d,
        ValueType::TextureCube,
        ValueType::Sampler,
    ];

    /// The float scalar and vector types, in increasing width.
    ///
    /// This is the set the arithmetic node families are generic over: the
    /// same `+` node makes sense for `f32` and `vec4f`, but not for `bool`.
    pub const FLOATS: &'static [ValueType] = &[
        ValueType::F32,
        ValueType::Vec2,
        ValueType::Vec3,
        ValueType::Vec4,
    ];

    /// The float *vector* types, in increasing width — [`Self::FLOATS`]
    /// without the scalar.
    ///
    /// The set for an operation that needs more than one component to mean
    /// anything: `dot`, `normalize`, `cross`, `reflect`.
    pub const VECTORS: &'static [ValueType] = &[ValueType::Vec2, ValueType::Vec3, ValueType::Vec4];

    /// The square float matrix types.
    pub const MATRICES: &'static [ValueType] = &[ValueType::Mat3, ValueType::Mat4];

    /// Every type WGSL's `+`, `-` and `*` accept as an operand:
    /// [`Self::FLOATS`] plus [`Self::MATRICES`].
    ///
    /// A `Vec` rather than a slice constant because it is what
    /// [`GenericParam::new`] takes, and the two halves live in separate
    /// constants that no `const fn` can concatenate.
    pub fn operands() -> Vec<ValueType> {
        let mut types = Self::FLOATS.to_vec();
        types.extend_from_slice(Self::MATRICES);
        types
    }

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
            ValueType::Texture2d => "texture_2d<f32>",
            ValueType::TextureCube => "texture_cube<f32>",
            ValueType::Sampler => "sampler",
        }
    }

    /// Whether this type names a bound resource rather than a value:
    /// a member of [`Self::RESOURCES`].
    pub fn is_resource(&self) -> bool {
        matches!(
            self,
            ValueType::Texture2d | ValueType::TextureCube | ValueType::Sampler
        )
    }

    /// The short suffix used where a node id does name a type
    /// (`convert.split.vec3f`, whose socket *count* is part of the type).
    ///
    /// The same as [`Self::wxsl_type`] except for the resource types, whose
    /// spelling carries a `<f32>` that has no business in an identifier.
    pub fn suffix(&self) -> &'static str {
        match self {
            ValueType::Texture2d => "texture_2d",
            ValueType::TextureCube => "texture_cube",
            other => other.wxsl_type(),
        }
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

    /// Whether this is a float *vector* — `is_float` without the scalar.
    pub fn is_vector(&self) -> bool {
        matches!(self.component_count(), Some(2..=4))
    }

    /// Whether this is a float matrix.
    pub fn is_matrix(&self) -> bool {
        matches!(self, ValueType::Mat3 | ValueType::Mat4)
    }

    /// WGSL's typing of `+`, `-`, `/` and `%`: two equal types combine to
    /// that type (including matrix with matrix, for `+`/`-`), and an `f32`
    /// against a float vector spreads over its components in either order
    /// (`f32 + vec3f` and `vec3f + f32` both give `vec3f`). Nothing else
    /// combines — not two different vector widths, and not a scalar against
    /// a matrix.
    ///
    /// `/` and `%` reject matrices outright where `+`/`-` accept two of the
    /// same shape; that difference is expressed by what the node's
    /// [`GenericParam::allowed`] lists, not here, so this stays one rule.
    /// See [`TypeRule`].
    pub fn componentwise(self, other: ValueType) -> Option<ValueType> {
        // A texture plus a texture is not a texture, and the equality
        // shortcut below would otherwise say it was.
        if self.is_resource() || other.is_resource() {
            return None;
        }
        if self == other {
            return Some(self);
        }
        match (self, other) {
            (ValueType::F32, wide) | (wide, ValueType::F32) if wide.is_vector() => Some(wide),
            _ => None,
        }
    }

    /// WGSL's typing of `*`: everything [`Self::componentwise`] allows, plus
    /// the linear algebra — a scalar against a matrix gives that matrix, and
    /// a matrix against a vector of its own size gives that vector, in
    /// either order (`mat3x3f * vec3f` and `vec3f * mat3x3f` are both
    /// `vec3f`).
    ///
    /// Two matrices combine only when equal, which for the square types
    /// this enum carries is exactly WGSL's `matKxR * matCxK -> matCxR`.
    /// See [`TypeRule`].
    pub fn product(self, other: ValueType) -> Option<ValueType> {
        if let Some(ty) = self.componentwise(other) {
            return Some(ty);
        }
        match (self, other) {
            (ValueType::F32, m) | (m, ValueType::F32) if m.is_matrix() => Some(m),
            (ValueType::Mat3, ValueType::Vec3) | (ValueType::Vec3, ValueType::Mat3) => {
                Some(ValueType::Vec3)
            }
            (ValueType::Mat4, ValueType::Vec4) | (ValueType::Vec4, ValueType::Mat4) => {
                Some(ValueType::Vec4)
            }
            _ => None,
        }
    }

    /// The all-zero value of this type, or `None` for a resource type,
    /// which has no [`Value`] at all.
    pub fn zero(&self) -> Option<Value> {
        Some(match self {
            ValueType::Bool => Value::Bool(false),
            ValueType::I32 => Value::I32(0),
            ValueType::U32 => Value::U32(0),
            ValueType::F32 => Value::F32(0.0),
            ValueType::Vec2 => Value::Vec2([0.0; 2]),
            ValueType::Vec3 => Value::Vec3([0.0; 3]),
            ValueType::Vec4 => Value::Vec4([0.0; 4]),
            ValueType::Mat3 => Value::Mat3([0.0; 9]),
            ValueType::Mat4 => Value::Mat4([0.0; 16]),
            ValueType::Texture2d | ValueType::TextureCube | ValueType::Sampler => return None,
        })
    }

    /// This type built from the single scalar `value`: every component of a
    /// float scalar or vector, and the *diagonal* of a matrix.
    ///
    /// The diagonal is the only reading that makes sense for a matrix —
    /// `mat3x3f(v)` is not even WGSL, and a matrix of all `v` is not a
    /// useful value of anything, whereas `splat(1.0)` being the identity is
    /// exactly the default a matrix socket wants.
    ///
    /// `bool`, `i32` and `u32` take the obvious reading — nonzero, and the
    /// truncated integer — so that one generic node can offer a default at
    /// every type it allows, which is what `param.value` needs to be one
    /// node rather than four. Only the resource types answer `None`: a
    /// texture has no value to build.
    pub fn splat(&self, value: f32) -> Option<Value> {
        /// A square matrix with `value` on the diagonal, column-major.
        fn diagonal<const N: usize, const CELLS: usize>(value: f32) -> [f32; CELLS] {
            let mut cells = [0.0; CELLS];
            for index in 0..N {
                cells[index * N + index] = value;
            }
            cells
        }

        match self {
            ValueType::F32 => Some(Value::F32(value)),
            ValueType::Vec2 => Some(Value::Vec2([value; 2])),
            ValueType::Vec3 => Some(Value::Vec3([value; 3])),
            ValueType::Vec4 => Some(Value::Vec4([value; 4])),
            ValueType::Mat3 => Some(Value::Mat3(diagonal::<3, 9>(value))),
            ValueType::Mat4 => Some(Value::Mat4(diagonal::<4, 16>(value))),
            ValueType::Bool => Some(Value::Bool(value != 0.0)),
            ValueType::I32 => Some(Value::I32(value as i32)),
            ValueType::U32 => Some(Value::U32(value.max(0.0) as u32)),
            ValueType::Texture2d | ValueType::TextureCube | ValueType::Sampler => None,
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` so `{:<8}` in a listing actually aligns.
        f.pad(self.wxsl_type())
    }
}

/// Which of WGSL's operand-combining rules a [`Socket::combine`] socket
/// derives its type by.
///
/// A generic socket's type *is* one resolved [`GenericParam`]; a combined
/// socket's type is a *function* of two, and this says which function.
/// There are exactly two because WGSL's arithmetic operators type in exactly
/// two ways — `*` does linear algebra, the rest do not — and both are
/// backed by a method on [`ValueType`] so the rule is stated once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeRule {
    /// [`ValueType::componentwise`]: WGSL's `+`, `-`, `/` and `%`.
    Componentwise,
    /// [`ValueType::product`]: WGSL's `*`.
    Product,
}

impl TypeRule {
    /// The type `a` and `b` combine to under this rule, or `None` if they do
    /// not combine at all.
    pub fn apply(self, a: ValueType, b: ValueType) -> Option<ValueType> {
        match self {
            TypeRule::Componentwise => a.componentwise(b),
            TypeRule::Product => a.product(b),
        }
    }

    /// What this rule requires of its two operands, phrased for the error
    /// message [`crate::error::GraphError::IncompatibleGenerics`] carries.
    pub fn requirement(self) -> &'static str {
        match self {
            TypeRule::Componentwise => {
                "the two must be the same type, or one of them f32 against a float vector"
            }
            TypeRule::Product => {
                "the two must be the same type, one of them f32, or a matrix against a \
                 vector of its own size"
            }
        }
    }
}

impl fmt::Display for TypeRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TypeRule::Componentwise => "componentwise",
            TypeRule::Product => "product",
        })
    }
}

/// A socket type derived from two [`GenericParam`]s by a [`TypeRule`], for a
/// port whose type depends on two independently-resolved operands rather
/// than being equal to either one — `multiply`'s output, which is `vec3f`
/// for `f32 * vec3f` just as much as for `vec3f * vec3f`.
///
/// See [`Socket::combine`].
#[derive(Clone, Debug, PartialEq)]
pub struct Combined {
    /// Which of WGSL's rules combines the two.
    pub rule: TypeRule,
    /// The left operand's parameter name.
    pub a: WxslIdent,
    /// The right operand's parameter name.
    pub b: WxslIdent,
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

    /// This value's float components, or `None` for the types that have
    /// none (`bool`, `i32`, `u32`). A matrix's are column-major.
    pub fn components(&self) -> Option<&[f32]> {
        match self {
            Value::F32(value) => Some(core::slice::from_ref(value)),
            Value::Vec2(values) => Some(values),
            Value::Vec3(values) => Some(values),
            Value::Vec4(values) => Some(values),
            Value::Mat3(values) => Some(values),
            Value::Mat4(values) => Some(values),
            Value::Bool(_) | Value::I32(_) | Value::U32(_) => None,
        }
    }

    /// This value re-expressed as `ty`, carrying across what it can.
    ///
    /// Components carry over in order and anything missing repeats the last
    /// one, so `0.5` widens to `vec3f(0.5, 0.5, 0.5)` and `vec3f(1, 2, 3)`
    /// narrows to `vec2f(1, 2)`. A matrix takes the first component on its
    /// diagonal, which does not try to preserve a rotation — retyping a
    /// matrix is rare, and pretending to keep a basis that no longer fits
    /// would be worse than plainly starting from a scaled identity. A value
    /// with no float components at all converts to `ty`'s zero, and a
    /// resource type converts to nothing at all — there is no value of one.
    ///
    /// This is what keeps a pinned parameter meaningful when the socket it
    /// sits on changes type — picking `vec3f` on a `math.add` whose operands
    /// were `0.5` should leave them at `vec3f(0.5)`, not report a type
    /// mismatch. See [`crate::graph::Graph::set_generic`].
    pub fn converted_to(self, ty: ValueType) -> Option<Value> {
        if self.ty() == ty {
            return Some(self);
        }
        let Some(components) = self.components() else {
            return ty.zero();
        };
        let Some(&first) = components.first() else {
            return ty.zero();
        };
        let last = *components.last().unwrap_or(&first);
        let at = |index: usize| *components.get(index).unwrap_or(&last);
        Some(match ty {
            ValueType::F32 => Value::F32(first),
            ValueType::Vec2 => Value::Vec2([at(0), at(1)]),
            ValueType::Vec3 => Value::Vec3([at(0), at(1), at(2)]),
            ValueType::Vec4 => Value::Vec4([at(0), at(1), at(2), at(3)]),
            // `splat` is the diagonal for a matrix, so this is `first` times
            // the identity.
            ValueType::Mat3 | ValueType::Mat4 => return ty.splat(first).or_else(|| ty.zero()),
            ValueType::Bool | ValueType::I32 | ValueType::U32 => return ty.zero(),
            ValueType::Texture2d | ValueType::TextureCube | ValueType::Sampler => return None,
        })
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
    ///
    /// For a socket with [`Socket::generic`] set, this is only the
    /// *placeholder* shown before any node instance has resolved that
    /// parameter (see [`GenericParam`]) — never read by codegen, which reads
    /// the instance's resolved type instead.
    pub ty: ValueType,
    /// Value used when an input is left unconnected and the node instance
    /// pins no parameter. `None` on a non-optional input makes it mandatory.
    ///
    /// Always `None` on a generic or combined socket: a fixed [`Value`]
    /// would be the wrong type for every resolution but one, so such an
    /// input is mandatory instead — connect something, or (via
    /// [`crate::graph::Graph::set_generic`]) pick the type explicitly, and
    /// only then does an unconnected value become meaningful.
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
    /// The [`GenericParam::name`] this socket's *effective* type is governed
    /// by, if any. `None` means `ty` is this socket's fixed, final type —
    /// the common case. See [`GenericParam`] for what a generic socket means
    /// and how its type is resolved per graph node. Mutually exclusive with
    /// [`Socket::combine`].
    pub generic: Option<WxslIdent>,
    /// A default expressed as one `f32` to spread over every component of
    /// the socket's *resolved* type, for a socket whose type only a node
    /// instance knows.
    ///
    /// [`Socket::default`] cannot serve there — a fixed [`Value`] is the
    /// wrong type for every resolution but one — but "1.0, whatever width
    /// that turns out to be" is perfectly well defined, and it is exactly
    /// what `clamp`'s `high` or `mix`'s `b` wants. Set by
    /// [`Socket::with_splat_default`] on a generic or combined socket
    /// (which stores the fixed [`Value`] instead when the type is already
    /// concrete), and read through [`Socket::default_for`].
    pub splat_default: Option<f32>,
    /// How this socket's *effective* type is derived from two
    /// [`GenericParam`]s, if it is — for a port that depends on two
    /// independently-resolved operands rather than being equal to either
    /// one. See [`Socket::combine`]. Mutually exclusive with
    /// [`Socket::generic`].
    pub combine: Option<Combined>,
    /// Whether this input takes a pinned literal and nothing else: no port,
    /// no edge, only the value the node instance carries.
    ///
    /// For an input that is not a value flowing *into* the node but a
    /// property *of* it. `param.value`'s `value` socket is the parameter's
    /// **default** — what the host initialises the uniform buffer with — so
    /// wiring a computed expression into it could not mean anything: the
    /// default is read once, on the CPU, before any shader runs.
    ///
    /// It is still a socket rather than a [`SettingDef`] because it is a
    /// typed [`Value`] with an editor widget already built for it, and
    /// because it is generic — the default of a `vec3f` parameter is a
    /// `vec3f`. [`crate::graph::Graph::connect`] refuses an edge into one.
    pub constant: bool,
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
            generic: None,
            splat_default: None,
            combine: None,
            constant: false,
        }
    }

    /// Mark this input as taking a pinned literal and never an edge.
    ///
    /// See [`Socket::constant`] for when that is the right shape.
    pub fn constant(mut self) -> Self {
        self.constant = true;
        self
    }

    /// Mark this input as skippable when nothing feeds it.
    ///
    /// See [`Socket::optional`] for the one body kind this is valid on.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Make this socket's effective type governed by generic parameter
    /// `param` instead of fixed at `ty` — see [`GenericParam`]. The
    /// declaring [`NodeDefinition`] must declare a matching
    /// [`GenericParam::name`], which the builder checks.
    ///
    /// # Panics
    ///
    /// Panics if `param` is not a valid WXSL identifier, or if
    /// [`Socket::combine`] is already set — a socket's effective type
    /// comes from exactly one of the two.
    pub fn generic(mut self, param: &str) -> Self {
        assert!(
            self.combine.is_none(),
            "socket `{}` already combines two parameters and cannot also be generic over one",
            self.name
        );
        self.generic =
            Some(WxslIdent::new(param).expect("generic parameter name must be a valid identifier"));
        self
    }

    /// Make this socket's effective type `rule` applied to generic
    /// parameters `a` and `b`, instead of fixed at `ty` or governed by a
    /// single parameter — for a port whose type depends on two
    /// independently-resolved operands, e.g. `multiply`'s result being
    /// `vec3f` for `f32 * vec3f` as much as for `vec3f * vec3f`, but equal
    /// to neither `a` nor `b` alone. The declaring [`NodeDefinition`] must
    /// declare matching [`GenericParam`]s, which the builder checks.
    ///
    /// Nothing *adopts* a type into a combined socket: its type is derived,
    /// so connecting to it resolves nothing, and it stays unresolved until
    /// both parameters it reads are. [`crate::graph::Graph::connect`] allows
    /// such an edge and [`crate::graph::Graph::validate`] re-checks it once
    /// there is something to check.
    ///
    /// # Panics
    ///
    /// Panics if `a` or `b` are not valid WXSL identifiers, or if
    /// [`Socket::generic`] is already set.
    pub fn combine(mut self, rule: TypeRule, a: &str, b: &str) -> Self {
        assert!(
            self.generic.is_none(),
            "socket `{}` is already generic over one parameter and cannot also combine two",
            self.name
        );
        self.combine = Some(Combined {
            rule,
            a: WxslIdent::new(a).expect("generic parameter name must be a valid identifier"),
            b: WxslIdent::new(b).expect("generic parameter name must be a valid identifier"),
        });
        self
    }

    /// Every [`GenericParam::name`] this socket's effective type reads: one
    /// for [`Socket::generic`], two for [`Socket::combine`], none for a
    /// socket fixed at `ty`.
    pub fn referenced_params(&self) -> impl Iterator<Item = &WxslIdent> {
        self.generic
            .iter()
            .chain(self.combine.iter().flat_map(|c| [&c.a, &c.b]))
    }

    /// Whether this input must be fed by an edge, a parameter or a default.
    pub fn is_required(&self) -> bool {
        !self.optional && self.default.is_none() && self.splat_default.is_none()
    }

    /// This socket's default once its type is known: the fixed
    /// [`Socket::default`], or [`Socket::splat_default`] spread over `ty`.
    ///
    /// `ty` is the socket's *effective* type — what
    /// [`crate::graph::Graph::effective_type`] answers for the node
    /// instance — so for an ordinary fixed socket this is just
    /// [`Socket::default`] and the argument is ignored.
    pub fn default_for(&self, ty: ValueType) -> Option<Value> {
        self.default
            .or_else(|| self.splat_default.and_then(|value| ty.splat(value)))
    }

    /// Give this socket a default value.
    ///
    /// # Panics
    ///
    /// Panics if `value`'s type is not the socket's type, or if the socket
    /// is generic or combined — a fixed [`Value`] would be the wrong type
    /// for every resolution but one, so neither has a static default (see
    /// [`Socket::generic`]/[`Socket::combine`]).
    pub fn with_default(mut self, value: Value) -> Self {
        assert!(
            self.generic.is_none(),
            "socket `{}` is generic over `{}` and cannot have a fixed default",
            self.name,
            self.generic.as_ref().map(WxslIdent::as_str).unwrap_or(""),
        );
        assert!(
            self.combine.is_none(),
            "socket `{}` combines two parameters and cannot have a fixed default",
            self.name,
        );
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
    /// On a generic or combined socket this stores the scalar itself
    /// ([`Socket::splat_default`]) and spreads it over whatever type the
    /// node instance resolves to; on a socket with a concrete type it
    /// resolves to a fixed [`Value`] immediately. Callers do not need to
    /// care which: "half, whatever width this turns out to be" is the same
    /// intent either way.
    ///
    /// # Panics
    ///
    /// Panics if the socket has a concrete type [`ValueType::splat`] cannot
    /// build (`bool`, `i32`, `u32`). A generic socket cannot be checked
    /// here — its parameter's allowed set is on the [`NodeDefinition`], not
    /// the socket — so a splat default simply does not apply at a
    /// resolution that has no float components (see
    /// [`Socket::default_for`]).
    pub fn with_splat_default(mut self, value: f32) -> Self {
        if self.generic.is_some() || self.combine.is_some() {
            self.splat_default = Some(value);
            return self;
        }
        let splat = self.ty.splat(value).unwrap_or_else(|| {
            panic!(
                "socket `{}` has no float components to splat into",
                self.name
            )
        });
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
    ///
    /// Boxed: [`WxslFunction`] (a parameter list plus a return shape) is far
    /// larger than every other variant here (a `Vec<String>`, one
    /// [`WxslIdent`], or nothing), so leaving it inline would size every
    /// `NodeBody` — most of which are not a call at all — to match.
    Call(Box<WxslFunction>),
    /// Reads one field of the per-fragment surface context the renderer hands
    /// to the material function (world position, UV, view direction, …). The
    /// field set is part of the shader ABI, see [`crate::abi`].
    ContextRead(WxslIdent),
    /// The terminal node: its inputs are the fields of the surface struct the
    /// material function returns. A graph has exactly one of these.
    SurfaceOutput,
    /// Reads a **uniform parameter** of the material's own bind group: a
    /// field of the one buffer `wxsl-core` lays out from every reachable
    /// node of this kind.
    ///
    /// The setting named [`SETTING_NAME`] is the parameter's name, and the
    /// input socket named [`SOCKET_VALUE`] is its default. Nothing else
    /// distinguishes it from [`NodeBody::Expr`] with a `{value}` template
    /// — which is exactly what a `const` node is, and exactly the
    /// difference this body exists to draw: editing a `const` compiles a
    /// new variant, editing a parameter writes bytes
    /// ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
    Param,
    /// Declares a **texture or sampler** in the material's bind group and
    /// hands out the binding.
    ///
    /// The setting named [`SETTING_NAME`] is the resource's name; the
    /// node's single output socket's type — one of
    /// [`ValueType::RESOURCES`] — is what kind of resource it is.
    Resource,
    /// Reads one field of the **application's** uniform block, in
    /// `abi::GROUP_USER`.
    ///
    /// The setting named [`SETTING_FIELD`] names the field, and the graph's
    /// own block declaration
    /// ([`crate::graph::Graph::user_block`]) says what type it has. This is
    /// the one body whose type comes from the *graph* rather than from the
    /// definition or an edge, because the block is a property of the
    /// document, not of the node kind.
    UserRead,
    /// Reads one **attribute the geometry supplies**: a per-vertex stream
    /// the mesh carries, or a per-instance field the draw supplies.
    ///
    /// The setting named [`SETTING_NAME`] says which, and the graph's own
    /// declaration ([`crate::graph::Graph::attributes`]) says what type it
    /// has and at what frequency. One node kind carrying a name, rather
    /// than a generated node kind per attribute: the registry is global
    /// and shared, and a per-graph registry is a much larger thing to own
    /// than a validated string
    /// ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
    ///
    /// Which frequency a name has is *not* on the node, so moving an
    /// attribute from per-vertex to per-instance rewires nothing.
    AttributeRead,
}

/// Setting name every [`NodeBody::Param`] and [`NodeBody::Resource`] node
/// carries: what the parameter or resource is called in the shader, and the
/// name the host writes it by.
pub const SETTING_NAME: &str = "name";
/// Setting name every [`NodeBody::UserRead`] node carries: which field of
/// the application's block it reads.
pub const SETTING_FIELD: &str = "field";
/// The input socket a [`NodeBody::Param`] node holds its default in.
pub const SOCKET_VALUE: &str = "value";

/// A string-valued property of a *node instance* that changes what the node
/// compiles to.
///
/// Distinct from a [`Socket`], which carries a typed value along an edge,
/// and from [`crate::graph::Node::label`], which is display metadata a
/// reader chooses and codegen never sees (ADR 0019). A setting is neither:
/// it is a name, and the name is the thing being declared. `param.value`'s
/// `name` *is* the uniform's identity — rename it and the host writes a
/// different field.
///
/// String-valued because every use of it so far is an identifier chosen by
/// the author: a parameter name, a texture name, a field of the
/// application's block, and (M4) a vertex attribute. Nothing here validates
/// the string; [`crate::graph::Graph::validate`] does, because what makes a
/// setting valid depends on the body reading it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingDef {
    /// The setting's name, unique among the definition's settings.
    pub name: WxslIdent,
    /// Short label for the editor's field.
    pub label: String,
    /// One-line description.
    pub doc: String,
    /// Value used when the node instance pins none.
    pub default: String,
}

impl SettingDef {
    /// Declare a setting.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid WXSL identifier. Definitions are
    /// authored in Rust, so this is a programming error.
    pub fn new(name: &str, label: impl Into<String>, doc: impl Into<String>) -> Self {
        SettingDef {
            name: WxslIdent::new(name).expect("setting name must be a valid WXSL identifier"),
            label: label.into(),
            doc: doc.into(),
            default: String::new(),
        }
    }

    /// Give the setting a value to start from.
    pub fn with_default(mut self, default: impl Into<String>) -> Self {
        self.default = default.into();
        self
    }
}

/// A named type parameter a [`NodeDefinition`] declares, constrained to a
/// fixed set of concrete types.
///
/// This is what lets one node kind stand in for several: instead of
/// `math.add.f32`, `math.add.vec2f`, `math.add.vec3f` and `math.add.vec4f` as
/// four separate registry entries, `math.add` declares one generic parameter
/// `T: f32 | vec2f | vec3f | vec4f`, and its `a`, `b` and `out` sockets all
/// reference it via [`Socket::generic`].
///
/// A parameter is *declared* here, once, on the definition every node
/// instance of this kind shares; it is *resolved* — given a concrete
/// [`crate::node::ValueType`] — separately per instance, in
/// [`crate::graph::Node::generics`]. Resolution happens automatically the
/// first time an edge connects a socket sharing this parameter to something
/// concretely typed (or another already-resolved generic socket), or
/// explicitly via [`crate::graph::Graph::set_generic`]; a node whose
/// declared parameter has no resolution yet is reported as
/// [`crate::error::GraphError::UnresolvedGeneric`] by
/// [`crate::graph::Graph::validate`] — codegen has no concrete WGSL type to
/// emit for it, exactly as a mandatory socket with nothing feeding it is
/// [`crate::error::GraphError::MissingInput`].
#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    /// The parameter's name, e.g. `T`. Referenced by [`Socket::generic`].
    pub name: WxslIdent,
    /// The concrete types this parameter may resolve to, in the order a
    /// "pick a type" control offers them.
    pub allowed: Vec<ValueType>,
}

impl GenericParam {
    /// Declare a parameter named `name`, resolvable to any of `allowed`.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid WXSL identifier, or `allowed` is
    /// empty (a parameter with nothing it can resolve to can never be
    /// satisfied).
    pub fn new(name: &str, allowed: Vec<ValueType>) -> Self {
        assert!(
            !allowed.is_empty(),
            "generic parameter `{name}` must allow at least one type"
        );
        GenericParam {
            name: WxslIdent::new(name).expect("generic parameter name must be a valid identifier"),
            allowed,
        }
    }
}

/// The kind of a node: its typed interface plus the WXSL it emits.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeDefinition {
    /// Registry id, dotted and stable: `math.add`, `lighting.pbr`.
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
    /// Type parameters this definition's sockets may reference via
    /// [`Socket::generic`]. See [`GenericParam`].
    pub generics: Vec<GenericParam>,
    /// Extra `(module, item)` imports the body needs, beyond those implied by
    /// a [`NodeBody::Call`].
    pub imports: Vec<(ModulePath, WxslIdent)>,
    /// String-valued properties a node *instance* of this kind carries. See
    /// [`SettingDef`].
    pub settings: Vec<SettingDef>,
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
                generics: Vec::new(),
                imports: Vec::new(),
                settings: Vec::new(),
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
        def.body = NodeBody::Call(Box::new(func));
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

    /// Look up a declared setting by name.
    pub fn setting(&self, name: &str) -> Option<&SettingDef> {
        self.settings
            .iter()
            .find(|setting| setting.name.as_str() == name)
    }

    /// Look up a declared generic parameter by name.
    pub fn generic(&self, name: &str) -> Option<&GenericParam> {
        self.generics
            .iter()
            .find(|param| param.name.as_str() == name)
    }

    /// The type every declared [`GenericParam`] resolves to when nothing has
    /// said otherwise: the first of its allowed types.
    ///
    /// A node dropped on a canvas is complete rather than half-typed this
    /// way — an unresolved parameter is an error
    /// ([`crate::error::GraphError::UnresolvedGeneric`]) and a generic socket
    /// whose type is unknown cannot even show a value to edit, so a fresh
    /// node with no resolution at all is a node that reports problems before
    /// it has been used. The first allowed type is a *default*, not a
    /// commitment: connecting anything else retypes it
    /// ([`crate::graph::Graph::connect`]), as does picking a type
    /// ([`crate::graph::Graph::set_generic`]).
    ///
    /// The order of [`GenericParam::allowed`] is therefore load-bearing, and
    /// so is picking allowed sets whose *first* entries actually combine:
    /// `vector.transform`'s `M` starts at `mat3x3f` and its `V` at `vec3f`
    /// because `mat3x3f * vec2f` is nothing at all.
    pub fn default_generics(&self) -> BTreeMap<String, ValueType> {
        self.generics
            .iter()
            .map(|param| {
                let first = *param
                    .allowed
                    .first()
                    .expect("`GenericParam::new` rejects an empty allowed set");
                (param.name.as_str().to_string(), first)
            })
            .collect()
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

    /// Declare a generic type parameter, so a [`Socket::generic`] on this
    /// definition may reference it. See [`GenericParam`].
    ///
    /// # Panics
    ///
    /// Panics on a duplicate parameter name.
    pub fn generic_param(mut self, param: GenericParam) -> Self {
        assert!(
            self.def.generic(param.name.as_str()).is_none(),
            "duplicate generic parameter `{}` on `{}`",
            param.name,
            self.def.id
        );
        self.def.generics.push(param);
        self
    }

    /// Declare a string-valued setting this node's instances carry. See
    /// [`SettingDef`].
    ///
    /// # Panics
    ///
    /// Panics on a duplicate setting name.
    pub fn setting(mut self, setting: SettingDef) -> Self {
        assert!(
            self.def.setting(setting.name.as_str()).is_none(),
            "duplicate setting `{}` on `{}`",
            setting.name,
            self.def.id
        );
        self.def.settings.push(setting);
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
        self.def.body = NodeBody::Call(Box::new(func));
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

    /// Finish with a [`NodeBody::Param`], [`NodeBody::Resource`],
    /// [`NodeBody::UserRead`] or [`NodeBody::AttributeRead`] body — the
    /// four that carry no payload of their own, because everything they
    /// need is the node instance's [`SettingDef`] value and its one output
    /// socket.
    ///
    /// # Panics
    ///
    /// Panics unless the body is one of those four (the others have a
    /// finisher that fills their payload in), the definition has exactly
    /// one output, or the required setting is not declared — all
    /// programming errors in a definition, which is Rust.
    pub fn declaration(mut self, body: NodeBody) -> NodeDefinition {
        let setting = match body {
            NodeBody::Param | NodeBody::Resource | NodeBody::AttributeRead => SETTING_NAME,
            NodeBody::UserRead => SETTING_FIELD,
            other => panic!("`{other:?}` is not a declaration body"),
        };
        assert_eq!(
            self.def.outputs.len(),
            1,
            "declaration node `{}` must have exactly one output",
            self.def.id
        );
        assert!(
            self.def.setting(setting).is_some(),
            "declaration node `{}` must declare a `{setting}` setting",
            self.def.id
        );
        self.def.body = body;
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
            // `{$T}` is substituted with the resolved WGSL spelling of
            // generic parameter `T` (see `crate::codegen`), so a typo in one
            // would otherwise only surface as a codegen error the first time
            // someone used the node.
            for expr in exprs {
                for param in type_placeholders(expr) {
                    assert!(
                        self.def.generic(param).is_some(),
                        "node `{}` references type placeholder `{{${param}}}`, \
                         which is not a generic parameter it declares",
                        self.def.id
                    );
                }
            }
        }
        for socket in self.def.inputs.iter().chain(&self.def.outputs) {
            if let Some(param) = &socket.generic {
                assert!(
                    self.def.generic(param.as_str()).is_some(),
                    "socket `{}` of `{}` references generic parameter `{param}`, \
                     which was never declared with `.generic_param(...)`",
                    socket.name,
                    self.def.id
                );
            }
            if let Some(combined) = &socket.combine {
                for param in [&combined.a, &combined.b] {
                    assert!(
                        self.def.generic(param.as_str()).is_some(),
                        "socket `{}` of `{}` combines generic parameter `{param}`, \
                         which was never declared with `.generic_param(...)`",
                        socket.name,
                        self.def.id
                    );
                }
            }
        }
        self.def
    }
}

/// Every `{$name}` type placeholder in an expression template, in order.
///
/// Deliberately the same scan `crate::codegen`'s template expansion does,
/// down to treating `{{` as an escaped brace, so what the builder checks and
/// what codegen substitutes cannot drift apart.
fn type_placeholders(template: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        if let Some(stripped) = rest.strip_prefix('{') {
            rest = stripped;
            continue;
        }
        let Some(close) = rest.find('}') else { break };
        if let Some(param) = rest[..close].strip_prefix('$') {
            found.push(param);
        }
        rest = &rest[close + 1..];
    }
    found
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
    fn componentwise_spreads_a_scalar_and_rejects_everything_else() {
        // Exactly the set `crates/wxsl/tests/graph_to_wgsl.rs` proves wgpu
        // accepts for `+`, `-`, `/` and `%`.
        for ty in ValueType::ALL {
            assert_eq!(ty.componentwise(*ty), Some(*ty), "{ty} with itself");
        }
        for wide in ValueType::VECTORS {
            assert_eq!(ValueType::F32.componentwise(*wide), Some(*wide));
            assert_eq!(wide.componentwise(ValueType::F32), Some(*wide));
        }
        assert_eq!(ValueType::Vec2.componentwise(ValueType::Vec3), None);
        // A scalar does *not* spread into a matrix: `mat3x3f + f32` is not
        // WGSL, however much `vec3f + f32` is.
        assert_eq!(ValueType::F32.componentwise(ValueType::Mat3), None);
        assert_eq!(ValueType::Mat3.componentwise(ValueType::Mat4), None);
    }

    #[test]
    fn product_adds_the_linear_algebra_on_top_of_componentwise() {
        // Everything componentwise allows still holds.
        assert_eq!(
            ValueType::F32.product(ValueType::Vec3),
            Some(ValueType::Vec3)
        );
        assert_eq!(
            ValueType::Vec3.product(ValueType::Vec3),
            Some(ValueType::Vec3)
        );
        // Plus what only `*` allows.
        for m in ValueType::MATRICES {
            assert_eq!(ValueType::F32.product(*m), Some(*m));
            assert_eq!(m.product(ValueType::F32), Some(*m));
            assert_eq!(m.product(*m), Some(*m));
        }
        assert_eq!(
            ValueType::Mat3.product(ValueType::Vec3),
            Some(ValueType::Vec3)
        );
        assert_eq!(
            ValueType::Vec3.product(ValueType::Mat3),
            Some(ValueType::Vec3)
        );
        assert_eq!(
            ValueType::Mat4.product(ValueType::Vec4),
            Some(ValueType::Vec4)
        );
        // And nothing more: a matrix only meets a vector of its own size.
        assert_eq!(ValueType::Mat3.product(ValueType::Vec4), None);
        assert_eq!(ValueType::Mat4.product(ValueType::Vec3), None);
        assert_eq!(ValueType::Mat3.product(ValueType::Mat4), None);
    }

    #[test]
    #[should_panic(expected = "cannot also be generic")]
    fn a_combined_socket_cannot_also_be_generic() {
        Socket::new("out", ValueType::F32)
            .combine(TypeRule::Product, "A", "B")
            .generic("A");
    }

    #[test]
    #[should_panic(expected = "cannot also combine")]
    fn a_generic_socket_cannot_also_combine() {
        Socket::new("out", ValueType::F32)
            .generic("A")
            .combine(TypeRule::Product, "A", "B");
    }

    #[test]
    #[should_panic(expected = "combines generic parameter `B`")]
    fn a_combined_socket_must_reference_declared_parameters() {
        NodeDefinition::builder("math.bogus_combined", "Bogus")
            .generic_param(GenericParam::new("A", vec![ValueType::F32]))
            .input(Socket::new("a", ValueType::F32).generic("A"))
            .output(Socket::new("out", ValueType::F32).combine(TypeRule::Product, "A", "B"))
            .expr("{a}");
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
