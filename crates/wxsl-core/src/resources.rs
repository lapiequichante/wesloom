//! What a material graph needs from outside itself: uniform parameters,
//! textures and samplers, and the block it expects the application to
//! supply.
//!
//! A graph does not only *compute*. Three kinds of declaration land here,
//! and [`MaterialInterface`] is all of them together — the answer to "what
//! must be bound before this shader can run", computed from the graph and
//! read by both halves: `codegen` emits the declarations, and `wxsl-render`
//! builds the bind groups and writes the bytes
//! ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
//!
//! # Why the layout is computed rather than mirrored
//!
//! Everywhere else in this repo a host-shared buffer layout is written
//! twice — a `#[repr(C)]` struct in Rust, a `struct` in WXSL — and kept
//! honest by a test on the sizes
//! ([ADR 0008](../../../docs/adr/0008-surface-graphs-and-a-named-shader-abi.md)).
//! That cannot work here: the fields are whatever the graph declares, so
//! there is no Rust struct to write. [`BufferLayout`] computes the offsets
//! instead, and is the only thing that knows them — the shader's struct is
//! *generated from it*, and the host writes *through it*, so the two halves
//! cannot disagree because there is only one half.
//!
//! The trap this exists to close is `vec3f`: it aligns to 16 bytes but
//! occupies 12, so `f32` then `vec3f` puts the vector at 16 and not at 4.
//! A hand-written mirror gets that wrong silently, every frame.
//!
//! # WGSL's uniform address space, in one table
//!
//! | Type | Align | Size |
//! |---|---|---|
//! | `bool` (stored as `u32`) | 4 | 4 |
//! | `i32`, `u32`, `f32` | 4 | 4 |
//! | `vec2f` | 8 | 8 |
//! | `vec3f` | 16 | 12 |
//! | `vec4f` | 16 | 16 |
//! | `mat3x3f` | 16 | 48 |
//! | `mat4x4f` | 16 | 64 |
//!
//! A struct's alignment is the largest of its members', rounded up to 16 in
//! the uniform address space; its size is the end of its last member
//! rounded up to that alignment. The *storage* address space is the same
//! table without that round-up, which is the only difference between
//! [`BufferLayout::uniform`] and [`BufferLayout::storage`] — and the whole
//! change the widened instance row needed
//! ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).

use core::fmt;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::abi;
use crate::node::{Value, ValueType};
use crate::wxsl::WxslIdent;

impl ValueType {
    /// This type's alignment in a host-shared buffer, in bytes, or `None`
    /// for a type that cannot be in one at all (a resource).
    pub fn buffer_align(&self) -> Option<u32> {
        Some(match self {
            ValueType::Bool | ValueType::I32 | ValueType::U32 | ValueType::F32 => 4,
            ValueType::Vec2 => 8,
            ValueType::Vec3 | ValueType::Vec4 | ValueType::Mat3 | ValueType::Mat4 => 16,
            ValueType::Texture2d | ValueType::TextureCube | ValueType::Sampler => return None,
        })
    }

    /// This type's size in a host-shared buffer, in bytes, or `None` for a
    /// resource.
    ///
    /// Note `vec3f`: 12, not 16. The 4 bytes after it are padding only if
    /// nothing small enough follows.
    pub fn buffer_size(&self) -> Option<u32> {
        Some(match self {
            ValueType::Bool | ValueType::I32 | ValueType::U32 | ValueType::F32 => 4,
            ValueType::Vec2 => 8,
            ValueType::Vec3 => 12,
            ValueType::Vec4 => 16,
            // Three columns, each a `vec3f` padded to its 16-byte alignment.
            ValueType::Mat3 => 48,
            ValueType::Mat4 => 64,
            ValueType::Texture2d | ValueType::TextureCube | ValueType::Sampler => return None,
        })
    }

    /// How this type is *spelled* in a host-shared struct.
    ///
    /// The same as [`ValueType::wxsl_type`] except for `bool`, which WGSL
    /// does not allow in the uniform address space at all — it has no
    /// defined host representation — so a boolean parameter is stored as a
    /// `u32` and compared against zero where it is read. See
    /// [`BufferLayout::read_expr`].
    pub fn buffer_type(&self) -> Option<&'static str> {
        match self {
            ValueType::Bool => Some("u32"),
            other if other.is_resource() => None,
            other => Some(other.wxsl_type()),
        }
    }
}

/// One field of a [`BufferLayout`]: where it is and how big it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldLayout {
    /// The field's name, as written in the generated struct and as the host
    /// addresses it.
    pub name: WxslIdent,
    /// The graph type it carries.
    pub ty: ValueType,
    /// Byte offset from the start of the buffer.
    pub offset: u32,
    /// Bytes occupied, excluding any padding that follows.
    pub size: u32,
}

/// A host-shared uniform buffer whose fields the graph decided.
///
/// Ordered by alignment, widest first, then by name: deterministic, so two
/// runs over the same graph produce the same offsets and the same variant
/// key, and tightly packed, so the `f32`-then-`vec3f` case costs nothing
/// rather than wasting 12 bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BufferLayout {
    fields: Vec<FieldLayout>,
    size: u32,
    align: u32,
}

impl BufferLayout {
    /// Lay `fields` out under WGSL's uniform address space rules.
    ///
    /// Duplicate names are the caller's problem to have rejected already
    /// (`Graph::validate` does); the last one of a name wins here, which
    /// keeps this function total.
    pub fn uniform(fields: impl IntoIterator<Item = (WxslIdent, ValueType)>) -> Self {
        let mut entries: Vec<(WxslIdent, ValueType)> = fields.into_iter().collect();
        sort_by_alignment(&mut entries);
        entries.dedup_by(|(a, _), (b, _)| a == b);
        // A struct in the uniform address space aligns to at least 16, and
        // `wgpu` wants the binding size to be a multiple of 16 too.
        lay_out(entries, 16)
    }

    /// Lay `prefix` out first, in the order given, then `fields` under
    /// WGSL's **storage** address space rules.
    ///
    /// Two differences from [`BufferLayout::uniform`], and only two.
    /// A storage struct's alignment is the largest of its members' and is
    /// *not* rounded up to 16. And `prefix` keeps its declaration order
    /// ahead of everything else, because it is ABI: the widened instance
    /// row begins with the model and normal matrices whatever a material
    /// appends, and a declared attribute that happened to sort in front of
    /// `model` would move the transform out from under the vertex stage
    /// that reads it through the narrow struct.
    pub fn storage(
        prefix: impl IntoIterator<Item = (WxslIdent, ValueType)>,
        fields: impl IntoIterator<Item = (WxslIdent, ValueType)>,
    ) -> Self {
        let prefix: Vec<(WxslIdent, ValueType)> = prefix.into_iter().collect();
        let mut rest: Vec<(WxslIdent, ValueType)> = fields
            .into_iter()
            .filter(|(name, _)| !prefix.iter().any(|(taken, _)| taken == name))
            .collect();
        sort_by_alignment(&mut rest);
        let mut entries = prefix;
        entries.extend(rest);
        entries.dedup_by(|(a, _), (b, _)| a == b);
        lay_out(entries, 0)
    }

    /// The fields, in layout order.
    pub fn fields(&self) -> &[FieldLayout] {
        &self.fields
    }

    /// Look one up by name.
    pub fn field(&self, name: &str) -> Option<&FieldLayout> {
        self.fields.iter().find(|field| field.name.as_str() == name)
    }

    /// Total size in bytes, rounded up to the struct's alignment. Zero for
    /// an empty layout, which is the "this material declares no parameters"
    /// case and gets no buffer at all.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The struct's alignment in bytes.
    pub fn align(&self) -> u32 {
        self.align
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// A zero-filled backing store of exactly the right size.
    pub fn zeroed(&self) -> Vec<u8> {
        vec![0; self.size as usize]
    }

    /// A backing store filled with `defaults`, and zero where a field has
    /// none.
    ///
    /// A value of the wrong type is skipped rather than reported: the
    /// defaults come from the same graph the layout does, and
    /// `Graph::validate` has already had its say about a mistyped one.
    pub fn filled(&self, defaults: &BTreeMap<String, Value>) -> Vec<u8> {
        let mut bytes = self.zeroed();
        for field in &self.fields {
            if let Some(&value) = defaults.get(field.name.as_str()) {
                let _ = self.write(&mut bytes, field.name.as_str(), value);
            }
        }
        bytes
    }

    /// The WGSL declaration of this layout as a struct named `name`.
    ///
    /// No `@align`/`@size` attributes: the field order is chosen so that
    /// WGSL's own layout rules put every field exactly where
    /// [`FieldLayout::offset`] says. Emitting attributes would state the
    /// same thing twice and give it somewhere to disagree.
    pub fn wgsl_struct(&self, name: &str) -> String {
        let mut out = String::with_capacity(64 + self.fields.len() * 32);
        let _ = writeln!(out, "struct {name} {{");
        for field in &self.fields {
            let ty = field
                .ty
                .buffer_type()
                .expect("a laid-out field is never a resource");
            let _ = writeln!(out, "    {}: {ty},", field.name);
        }
        out.push_str("}\n");
        out
    }

    /// The WGSL expression reading field `name` off a variable of this
    /// layout's struct type, or `None` if there is no such field.
    ///
    /// Not simply `var.name`: a `bool` parameter is stored as a `u32`,
    /// because WGSL's uniform address space has no `bool`, and this is the
    /// one place that knows it.
    pub fn read_expr(&self, var: &str, name: &str) -> Option<String> {
        let field = self.field(name)?;
        Some(match field.ty {
            ValueType::Bool => format!("({var}.{name} != 0u)"),
            _ => format!("{var}.{name}"),
        })
    }

    /// Write `value` into `bytes` at field `name`'s offset.
    ///
    /// `bytes` is a buffer of [`BufferLayout::size`] bytes — what
    /// [`BufferLayout::zeroed`] hands out. This is the only writer, which
    /// is what makes the offsets trustworthy: the host cannot compute them
    /// a second, different way.
    pub fn write(&self, bytes: &mut [u8], name: &str, value: Value) -> Result<(), LayoutError> {
        let field = self.field(name).ok_or_else(|| LayoutError::UnknownField {
            field: name.to_string(),
        })?;
        if value.ty() != field.ty {
            return Err(LayoutError::TypeMismatch {
                field: name.to_string(),
                supplied: value.ty(),
                expected: field.ty,
            });
        }
        let start = field.offset as usize;
        if bytes.len() < start + field.size as usize {
            return Err(LayoutError::BufferTooSmall {
                field: name.to_string(),
                needed: start + field.size as usize,
                len: bytes.len(),
            });
        }
        let put = |bytes: &mut [u8], at: usize, word: [u8; 4]| {
            bytes[at..at + 4].copy_from_slice(&word);
        };
        match value {
            Value::Bool(flag) => put(bytes, start, u32::from(flag).to_le_bytes()),
            Value::I32(number) => put(bytes, start, number.to_le_bytes()),
            Value::U32(number) => put(bytes, start, number.to_le_bytes()),
            Value::F32(number) => put(bytes, start, number.to_le_bytes()),
            Value::Vec2(_) | Value::Vec3(_) | Value::Vec4(_) | Value::Mat4(_) => {
                let components = value.components().expect("every float type has components");
                for (index, component) in components.iter().enumerate() {
                    put(bytes, start + index * 4, component.to_le_bytes());
                }
            }
            // The one type whose host and shader layouts differ: three
            // `vec3f` columns, each padded to 16 bytes. Written column by
            // column rather than as nine contiguous floats, which is what a
            // `#[repr(C)] [f32; 9]` mirror would have got wrong.
            Value::Mat3(cells) => {
                for column in 0..3 {
                    for row in 0..3 {
                        put(
                            bytes,
                            start + column * 16 + row * 4,
                            cells[column * 3 + row].to_le_bytes(),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Read field `name` back out of `bytes`, or `None` if there is no
    /// such field or the buffer is too short.
    ///
    /// The inverse of [`BufferLayout::write`], through the same offsets —
    /// so a value written and read back is a genuine round trip rather
    /// than a remembered copy, which is what makes it worth asserting on.
    pub fn read(&self, bytes: &[u8], name: &str) -> Option<Value> {
        let field = self.field(name)?;
        let start = field.offset as usize;
        if bytes.len() < start + field.size as usize {
            return None;
        }
        let word = |at: usize| -> [u8; 4] { bytes[at..at + 4].try_into().expect("four bytes") };
        let float = |at: usize| f32::from_le_bytes(word(at));
        let components = |count: usize| -> Vec<f32> {
            (0..count).map(|index| float(start + index * 4)).collect()
        };
        Some(match field.ty {
            ValueType::Bool => Value::Bool(u32::from_le_bytes(word(start)) != 0),
            ValueType::I32 => Value::I32(i32::from_le_bytes(word(start))),
            ValueType::U32 => Value::U32(u32::from_le_bytes(word(start))),
            ValueType::F32 => Value::F32(float(start)),
            ValueType::Vec2 => Value::Vec2(components(2).try_into().ok()?),
            ValueType::Vec3 => Value::Vec3(components(3).try_into().ok()?),
            ValueType::Vec4 => Value::Vec4(components(4).try_into().ok()?),
            ValueType::Mat4 => Value::Mat4(components(16).try_into().ok()?),
            ValueType::Mat3 => {
                let mut cells = [0.0; 9];
                for column in 0..3 {
                    for row in 0..3 {
                        cells[column * 3 + row] = float(start + column * 16 + row * 4);
                    }
                }
                Value::Mat3(cells)
            }
            _ => return None,
        })
    }

    /// A stable one-line description, for a cache key that has to
    /// distinguish two layouts.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for field in &self.fields {
            let _ = write!(out, "{}:{}@{};", field.name, field.ty, field.offset);
        }
        out
    }
}

/// Widest alignment first, then by name.
///
/// Deterministic, so two runs over the same graph produce the same offsets
/// and the same variant key, and tight, so the `f32`-then-`vec3f` case
/// costs nothing rather than wasting 12 bytes.
fn sort_by_alignment(entries: &mut [(WxslIdent, ValueType)]) {
    entries.sort_by(|(a_name, a_ty), (b_name, b_ty)| {
        b_ty.buffer_align()
            .cmp(&a_ty.buffer_align())
            .then_with(|| a_name.as_str().cmp(b_name.as_str()))
    });
}

/// Assign offsets to `entries` in the order given, with the struct's
/// alignment at least `min_align`.
fn lay_out(entries: Vec<(WxslIdent, ValueType)>, min_align: u32) -> BufferLayout {
    let mut laid_out = Vec::with_capacity(entries.len());
    let mut offset = 0u32;
    let mut align = 0u32;
    for (name, ty) in entries {
        let (Some(field_align), Some(size)) = (ty.buffer_align(), ty.buffer_size()) else {
            // A resource is not a buffer field. It reaches here only from
            // a caller that ignored `ValueType::is_resource`.
            continue;
        };
        offset = round_up(field_align, offset);
        laid_out.push(FieldLayout {
            name,
            ty,
            offset,
            size,
        });
        offset += size;
        align = align.max(field_align);
    }
    if laid_out.is_empty() {
        return BufferLayout::default();
    }
    let align = align.max(min_align);
    BufferLayout {
        fields: laid_out,
        size: round_up(align, offset),
        align,
    }
}

/// A texture or a sampler the material declares, and where it is bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceBinding {
    /// The name the graph gave it. Also the variable's name in the
    /// generated WGSL, and how the host addresses it.
    pub name: WxslIdent,
    /// Which kind of resource: one of [`ValueType::RESOURCES`].
    pub ty: ValueType,
    /// The `@binding(N)` within `abi::GROUP_MATERIAL`.
    pub binding: u32,
}

/// The uniform block a material *requires the application to supply*, in
/// `abi::GROUP_USER`.
///
/// Different in kind from everything else here: the material does not own
/// this resource and cannot fill it. It states the shape, hands out the
/// `BindGroupLayout`, and the application hands back a `BindGroup`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserBlock {
    /// The variable name, which is what a node reads through.
    pub name: WxslIdent,
    /// The generated struct's name, `{name}_uniforms`.
    pub struct_name: String,
    /// Its fields, laid out the same way a material's own parameters are.
    pub layout: BufferLayout,
}

impl UserBlock {
    /// The struct name for a block called `name`.
    pub fn struct_name_for(name: &WxslIdent) -> String {
        format!("{name}_uniforms")
    }
}

/// One per-vertex attribute a material declares, and every number that
/// follows from it.
///
/// Three of them, because a declared attribute lands in three places: a
/// `@location` in the vertex entry's second parameter, a vertex buffer
/// slot the mesh's stream is bound at, and a `@location` in the extended
/// varyings that carry it to the fragment stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexAttributeBinding {
    /// The name the graph gave it, which is also the name of the mesh
    /// stream that must supply it.
    pub name: WxslIdent,
    /// Its type. Never a resource, never a matrix, never an integer:
    /// `Graph::validate` has already had its say.
    pub ty: ValueType,
    /// `@location` in `abi::MATERIAL_VERTEX_IN_STRUCT`, numbered upwards
    /// from `abi::VERTEX_IN_FIELDS.len()`.
    pub location: u32,
    /// Which vertex buffer slot the mesh's stream is bound at. Slot 0 is
    /// the base interleaved vertex, so these start at 1.
    pub slot: u32,
    /// `@location` in `abi::MATERIAL_VERTEX_OUT_STRUCT` and
    /// `abi::MATERIAL_VARYINGS_STRUCT`.
    pub varying: u32,
}

/// What a material requires of the *geometry* it is drawn on: per-vertex
/// streams the mesh must carry, and per-instance fields the draw must
/// supply.
///
/// The mirror image of [`MaterialInterface`]'s other three declarations. A
/// uniform parameter is something the material owns and fills; an
/// attribute is something it *requires*, exactly as the application's block
/// is — so it gets the same declare-then-validate shape, and a mesh that
/// cannot supply one is a named error rather than a frame that looks wrong
/// ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometryInterface {
    vertex: Vec<VertexAttributeBinding>,
    instance: BufferLayout,
    computed: Vec<VaryingBinding>,
    instance_index_location: Option<u32>,
}

/// One interpolant the graph's own vertex stage computes.
///
/// Not something the geometry supplies — so not, strictly, part of what a
/// material *requires* of it — but it is an inter-stage location, and
/// there is exactly one accountant for those.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaryingBinding {
    /// Its name: what the writing node names, what the reading node
    /// names, and the field name in the IO structs.
    pub name: WxslIdent,
    /// Its type.
    pub ty: ValueType,
    /// `@location` in `abi::MATERIAL_VERTEX_OUT_STRUCT` and
    /// `abi::MATERIAL_VARYINGS_STRUCT`.
    pub varying: u32,
}

impl GeometryInterface {
    /// Number the declared attributes.
    ///
    /// Both sets are taken in name order, not declaration order: a
    /// location that moved because somebody reordered a list is a
    /// repipeline and a re-upload for no visible change.
    pub fn new(
        vertex: impl IntoIterator<Item = (WxslIdent, ValueType)>,
        instance: impl IntoIterator<Item = (WxslIdent, ValueType)>,
        computed: impl IntoIterator<Item = (WxslIdent, ValueType)>,
    ) -> Self {
        let instance = BufferLayout::storage([], instance);

        // The index first, so adding one more per-vertex attribute never
        // moves it — and it is the varying every instance attribute
        // shares, so it is the one worth pinning.
        let base_varying = abi::VERTEX_OUT_FIELDS.len() as u32;
        let instance_index_location = (!instance.is_empty()).then_some(base_varying);
        let first_extra = base_varying + u32::from(instance_index_location.is_some());

        let mut declared: Vec<(WxslIdent, ValueType)> = vertex.into_iter().collect();
        declared.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        declared.dedup_by(|(a, _), (b, _)| a == b);
        let vertex: Vec<VertexAttributeBinding> = declared
            .into_iter()
            .enumerate()
            .map(|(index, (name, ty))| VertexAttributeBinding {
                name,
                ty,
                location: abi::VERTEX_IN_FIELDS.len() as u32 + index as u32,
                slot: 1 + index as u32,
                varying: first_extra + index as u32,
            })
            .collect();

        // After the per-vertex ones, so that adding an interpolant never
        // moves an attribute's location — and therefore never invalidates
        // a pipeline built for a mesh that has not changed.
        let mut declared: Vec<(WxslIdent, ValueType)> = computed.into_iter().collect();
        declared.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        declared.dedup_by(|(a, _), (b, _)| a == b);
        let first_computed = first_extra + vertex.len() as u32;
        let computed = declared
            .into_iter()
            .enumerate()
            .map(|(index, (name, ty))| VaryingBinding {
                name,
                ty,
                varying: first_computed + index as u32,
            })
            .collect();

        GeometryInterface {
            vertex,
            instance,
            computed,
            instance_index_location,
        }
    }

    /// The interpolants the graph computes, in location order.
    pub fn computed(&self) -> &[VaryingBinding] {
        &self.computed
    }

    /// Look one up by name.
    pub fn computed_varying(&self, name: &str) -> Option<&VaryingBinding> {
        self.computed
            .iter()
            .find(|entry| entry.name.as_str() == name)
    }

    /// The per-vertex attributes, in location order.
    pub fn vertex(&self) -> &[VertexAttributeBinding] {
        &self.vertex
    }

    /// Look one up by name.
    pub fn vertex_attribute(&self, name: &str) -> Option<&VertexAttributeBinding> {
        self.vertex.iter().find(|entry| entry.name.as_str() == name)
    }

    /// One row of the declared per-instance attributes, at
    /// `abi::BINDING_INSTANCE_ATTRIBUTES`. Empty when the graph declares
    /// none, and then there is no buffer.
    ///
    /// Not the transform: that is `abi::INSTANCE_BASE_FIELDS` at
    /// `abi::BINDING_INSTANCES`, ABI-fixed and mirrored in Rust. Two
    /// arrays indexed by the same instance index, so neither has to know
    /// the other's stride.
    pub fn instance(&self) -> &BufferLayout {
        &self.instance
    }

    /// The declared per-instance attributes, in layout order.
    pub fn instance_attributes(&self) -> &[FieldLayout] {
        self.instance.fields()
    }

    /// `@location` the flat instance index travels at, or `None` when
    /// nothing needs it.
    pub fn instance_index_location(&self) -> Option<u32> {
        self.instance_index_location
    }

    /// Whether the material declares nothing at all, and so wants exactly
    /// the geometry every material has always wanted.
    pub fn is_empty(&self) -> bool {
        self.vertex.is_empty() && self.instance.is_empty() && self.computed.is_empty()
    }

    /// How many of `abi::MAX_VARYING_LOCATIONS` this material spends.
    ///
    /// The accountant: base varyings, the instance index if it travels,
    /// and one per declared per-vertex attribute. Reported rather than
    /// discovered, so overrunning the budget is a named error and not a
    /// shader-compiler complaint about location 16.
    pub fn varyings_used(&self) -> usize {
        abi::VERTEX_OUT_FIELDS.len()
            + usize::from(self.instance_index_location.is_some())
            + self.vertex.len()
            + self.computed.len()
    }

    /// A stable description of the shape, for a cache key.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for attribute in &self.vertex {
            let _ = write!(
                out,
                "{}:{}@{};",
                attribute.name, attribute.ty, attribute.location
            );
        }
        out.push('/');
        out.push_str(&self.instance.signature());
        out.push('/');
        for varying in &self.computed {
            let _ = write!(out, "{}:{}@{};", varying.name, varying.ty, varying.varying);
        }
        out
    }
}

impl Default for GeometryInterface {
    /// The base geometry: no declared attributes, and an instance row that
    /// is exactly `abi::INSTANCE_BASE_FIELDS`.
    fn default() -> Self {
        GeometryInterface::new([], [], [])
    }
}

/// Everything a material needs bound before it can draw.
///
/// Computed from the *reachable* part of the graph, so a half-finished
/// branch parked on the canvas declares nothing — the same rule codegen
/// already follows for the nodes it emits, and it has to be the same rule
/// or the bind group and the shader would disagree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialInterface {
    /// The material's own uniform parameters, at
    /// `abi::BINDING_MATERIAL_PARAMS` of `abi::GROUP_MATERIAL`. Empty when
    /// the graph declares none, and then there is no buffer.
    pub params: BufferLayout,
    /// The value each parameter's node declared as its starting point,
    /// which is what a fresh buffer is filled with.
    ///
    /// Deliberately *beside* [`Self::params`] rather than inside it, and
    /// deliberately absent from [`Self::signature`]: the variant key and
    /// the layout caches must depend on the parameter **layout** and never
    /// on a parameter **value**, or every slider is a recompile again —
    /// which is the whole thing this milestone buys. Keeping the two apart
    /// makes that hard to get wrong by accident.
    pub defaults: BTreeMap<String, Value>,
    /// Textures and samplers, from `abi::MATERIAL_RESOURCE_BINDING_BASE`
    /// upwards in name order.
    pub resources: Vec<ResourceBinding>,
    /// The block the application supplies, if the graph declares one.
    pub user: Option<UserBlock>,
    /// What the material requires of the geometry it is drawn on.
    ///
    /// Unlike the three above it, this is not a bind group: it is a vertex
    /// buffer layout and a storage-buffer stride. It sits here anyway
    /// because it is the same kind of statement — "this will not draw
    /// until you supply that" — and because a caller that has the
    /// interface should not need a second call to find out.
    pub geometry: GeometryInterface,
}

impl MaterialInterface {
    /// Whether the material group holds nothing at all — no parameters and
    /// no resources — so a pipeline built for this material can leave the
    /// slot unbound.
    pub fn material_group_is_empty(&self) -> bool {
        self.params.is_empty() && self.resources.is_empty()
    }

    /// Look a declared resource up by name.
    pub fn resource(&self, name: &str) -> Option<&ResourceBinding> {
        self.resources
            .iter()
            .find(|entry| entry.name.as_str() == name)
    }

    /// A stable description of the *shape* — never of any value.
    ///
    /// This is what a bind-group-layout cache and a pipeline-layout cache
    /// key on. Two materials with the same signature are interchangeable to
    /// `wgpu`, which is what lets one pipeline layout serve both.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "params[{}]", self.params.signature());
        out.push_str("res[");
        for entry in &self.resources {
            let _ = write!(out, "{}:{}@{};", entry.name, entry.ty, entry.binding);
        }
        out.push(']');
        if let Some(user) = &self.user {
            let _ = write!(out, "user[{}:{}]", user.name, user.layout.signature());
        }
        // Not a bind group, but a pipeline *is* built per vertex layout,
        // so it belongs in the same key rather than in one of its own.
        if !self.geometry.is_empty() {
            let _ = write!(out, "geom[{}]", self.geometry.signature());
        }
        out
    }
}

/// What can go wrong writing a value into a computed layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// No field of that name.
    UnknownField {
        /// The name that was asked for.
        field: String,
    },
    /// The value's type is not the field's type.
    TypeMismatch {
        /// The field.
        field: String,
        /// The type of the value supplied.
        supplied: ValueType,
        /// The type the field declares.
        expected: ValueType,
    },
    /// The backing store is shorter than the field needs. Only reachable
    /// from a caller that made its own buffer instead of
    /// [`BufferLayout::zeroed`].
    BufferTooSmall {
        /// The field being written.
        field: String,
        /// Bytes the write needed.
        needed: usize,
        /// Bytes there were.
        len: usize,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::UnknownField { field } => {
                write!(f, "no parameter named `{field}` in this material")
            }
            LayoutError::TypeMismatch {
                field,
                supplied,
                expected,
            } => write!(
                f,
                "parameter `{field}` is {expected}, but a {supplied} was written to it"
            ),
            LayoutError::BufferTooSmall { field, needed, len } => write!(
                f,
                "writing `{field}` needs {needed} bytes but the buffer is {len}"
            ),
        }
    }
}

impl std::error::Error for LayoutError {}

/// `value` rounded up to the next multiple of `align`.
fn round_up(align: u32, value: u32) -> u32 {
    value.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_storage_address_space_does_not_round_a_struct_up_to_sixteen() {
        // The one difference between the two address spaces, and the
        // whole change the second customer of this code needed. A uniform
        // block of one `f32` is 16 bytes because WGSL says so; a storage
        // row of one `f32` is 4, and a row that claimed 16 would put every
        // instance after the first at the wrong offset.
        let one = |name: &str, ty| vec![(WxslIdent::new(name).expect("valid"), ty)];
        let uniform = BufferLayout::uniform(one("amount", ValueType::F32));
        let storage = BufferLayout::storage([], one("amount", ValueType::F32));
        assert_eq!(uniform.size(), 16);
        assert_eq!(storage.size(), 4);
        assert_eq!(storage.align(), 4);
        assert_eq!(storage.field("amount").expect("declared").offset, 0);
    }

    #[test]
    fn a_storage_prefix_keeps_its_place_whatever_sorts_in_front_of_it() {
        // The prefix is ABI and the rest is sorted, so a field whose name
        // sorts before the prefix's must not overtake it.
        let ident = |name: &str| WxslIdent::new(name).expect("valid");
        let layout = BufferLayout::storage(
            [(ident("model"), ValueType::Mat4)],
            [
                (ident("aaa"), ValueType::Mat4),
                (ident("zzz"), ValueType::F32),
            ],
        );
        let names: Vec<&str> = layout
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["model", "aaa", "zzz"]);
        assert_eq!(layout.field("model").expect("declared").offset, 0);
    }

    #[test]
    fn declaring_nothing_is_the_geometry_every_material_always_had() {
        let geometry = GeometryInterface::default();
        assert!(geometry.is_empty());
        assert!(geometry.vertex().is_empty());
        // No attribute row at all, so nothing to upload and nothing to
        // pass down: a material that asks nothing of its geometry costs
        // nothing.
        assert!(geometry.instance().is_empty());
        assert_eq!(geometry.instance_index_location(), None);
        assert_eq!(geometry.varyings_used(), abi::VERTEX_OUT_FIELDS.len());
    }

    #[test]
    fn the_instance_index_travels_once_however_many_attributes() {
        // The reason the *index* goes down the pipe rather than the
        // values: one inter-stage location covers any number of them.
        let ident = |name: &str| WxslIdent::new(name).expect("valid");
        let one = GeometryInterface::new([], [(ident("tint"), ValueType::Vec3)], []);
        let many = GeometryInterface::new(
            [],
            [
                (ident("tint"), ValueType::Vec3),
                (ident("age"), ValueType::F32),
                (ident("phase"), ValueType::Vec2),
            ],
            [],
        );
        assert_eq!(one.varyings_used(), many.varyings_used());
        assert_eq!(one.varyings_used(), abi::VERTEX_OUT_FIELDS.len() + 1);
        assert_eq!(
            many.instance_index_location(),
            Some(abi::VERTEX_OUT_FIELDS.len() as u32)
        );
    }

    #[test]
    fn a_vertex_attribute_is_numbered_in_three_places_at_once() {
        // A location in the vertex entry, a buffer slot to bind at, and a
        // location in the varyings — three numbers that have to agree
        // between codegen, the pipeline and the mesh, so they are decided
        // once here.
        let ident = |name: &str| WxslIdent::new(name).expect("valid");
        let geometry = GeometryInterface::new(
            [
                (ident("weight"), ValueType::F32),
                (ident("color"), ValueType::Vec4),
            ],
            [(ident("tint"), ValueType::Vec3)],
            [],
        );
        // Name order, not declaration order: reordering the list must not
        // renumber anything.
        let color = geometry.vertex_attribute("color").expect("declared");
        let weight = geometry.vertex_attribute("weight").expect("declared");
        assert_eq!(color.location, abi::VERTEX_IN_FIELDS.len() as u32);
        assert_eq!(color.slot, 1);
        assert_eq!(weight.location, color.location + 1);
        assert_eq!(weight.slot, 2);
        // The index is pinned in front of them, so adding a per-vertex
        // attribute never moves it.
        assert_eq!(
            geometry.instance_index_location(),
            Some(abi::VERTEX_OUT_FIELDS.len() as u32)
        );
        assert_eq!(color.varying, abi::VERTEX_OUT_FIELDS.len() as u32 + 1);
    }

    #[test]
    fn a_computed_interpolant_is_numbered_after_everything_the_geometry_brings() {
        // Adding an interpolant must not move a vertex attribute: an
        // attribute's location is in the vertex buffer layout, and moving
        // it is a repipeline and a re-upload for a mesh that did not
        // change.
        let ident = |name: &str| WxslIdent::new(name).expect("valid");
        let attributes = [(ident("color"), ValueType::Vec4)];
        let plain = GeometryInterface::new(attributes.clone(), [], []);
        let computing = GeometryInterface::new(
            attributes.clone(),
            [],
            [
                (ident("wobble"), ValueType::F32),
                (ident("bend"), ValueType::Vec2),
            ],
        );
        let color = |geometry: &GeometryInterface| {
            let entry = geometry.vertex_attribute("color").expect("declared");
            (entry.location, entry.slot, entry.varying)
        };
        assert_eq!(color(&plain), color(&computing));

        // Name order among themselves, and after the attribute.
        let bend = computing.computed_varying("bend").expect("declared");
        let wobble = computing.computed_varying("wobble").expect("declared");
        assert_eq!(bend.varying, color(&computing).2 + 1);
        assert_eq!(wobble.varying, bend.varying + 1);
        // And they are locations like any other, so the accountant counts
        // them.
        assert_eq!(computing.varyings_used(), plain.varyings_used() + 2);
        assert_ne!(plain.signature(), computing.signature());
    }

    #[test]
    fn what_the_geometry_wants_is_part_of_the_shape_a_pipeline_is_built_for() {
        // Not a bind group, but a vertex buffer layout — so two materials
        // that differ only there must not share a pipeline.
        let ident = |name: &str| WxslIdent::new(name).expect("valid");
        let plain = MaterialInterface::default();
        let declaring = MaterialInterface {
            geometry: GeometryInterface::new([(ident("color"), ValueType::Vec3)], [], []),
            ..MaterialInterface::default()
        };
        assert_ne!(plain.signature(), declaring.signature());
        // And declaring nothing leaves the signature exactly as it was
        // before there was such a thing as a declared attribute.
        assert!(!plain.signature().contains("geom"));
    }

    use super::*;

    fn ident(name: &str) -> WxslIdent {
        WxslIdent::new(name).expect("test identifier")
    }

    fn layout(fields: &[(&str, ValueType)]) -> BufferLayout {
        BufferLayout::uniform(fields.iter().map(|(name, ty)| (ident(name), *ty)))
    }

    #[test]
    fn an_empty_layout_has_no_buffer_at_all() {
        let empty = BufferLayout::uniform([]);
        assert!(empty.is_empty());
        assert_eq!(empty.size(), 0);
        assert!(empty.zeroed().is_empty());
    }

    #[test]
    fn a_vec3_never_lands_at_a_non_multiple_of_sixteen() {
        // The whole reason this module exists. Declared `f32` first, and
        // the naive answer — 0 and 4 — is wrong in a way nothing would
        // report: the shader would read the vector from byte 16.
        let laid_out = layout(&[("amount", ValueType::F32), ("tint", ValueType::Vec3)]);
        let tint = laid_out.field("tint").expect("declared");
        assert_eq!(tint.offset % 16, 0);
        // Widest first, so the `f32` fills the vector's tail padding rather
        // than being padded away from it.
        assert_eq!(tint.offset, 0);
        assert_eq!(laid_out.field("amount").expect("declared").offset, 12);
        assert_eq!(laid_out.size(), 16);
    }

    #[test]
    fn every_field_sits_at_a_multiple_of_its_own_alignment() {
        let laid_out = layout(&[
            ("flag", ValueType::Bool),
            ("count", ValueType::I32),
            ("basis", ValueType::Mat3),
            ("offset", ValueType::Vec2),
            ("color", ValueType::Vec4),
            ("transform", ValueType::Mat4),
            ("scale", ValueType::F32),
            ("normal", ValueType::Vec3),
            ("index", ValueType::U32),
        ]);
        for field in laid_out.fields() {
            let align = field.ty.buffer_align().expect("a value type");
            assert_eq!(
                field.offset % align,
                0,
                "`{}` ({}) at {}",
                field.name,
                field.ty,
                field.offset
            );
        }
        assert_eq!(laid_out.size() % 16, 0);
        // No two fields overlap, in the order they were laid out.
        for pair in laid_out.fields().windows(2) {
            assert!(pair[0].offset + pair[0].size <= pair[1].offset);
        }
    }

    #[test]
    fn the_order_is_a_property_of_the_declarations_not_of_the_walk() {
        let forwards = layout(&[
            ("a", ValueType::F32),
            ("b", ValueType::Vec3),
            ("c", ValueType::Vec2),
        ]);
        let backwards = layout(&[
            ("c", ValueType::Vec2),
            ("b", ValueType::Vec3),
            ("a", ValueType::F32),
        ]);
        assert_eq!(forwards, backwards);
        assert_eq!(forwards.signature(), backwards.signature());
    }

    #[test]
    fn a_mat3_is_written_as_three_padded_columns() {
        // A `[f32; 9]` memcpy is the obvious implementation and is wrong:
        // WGSL pads each column to 16 bytes, so the shader would read
        // column 1 from the middle of column 0.
        let laid_out = layout(&[("basis", ValueType::Mat3)]);
        let mut bytes = laid_out.zeroed();
        let cells = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
        laid_out
            .write(&mut bytes, "basis", Value::Mat3(cells))
            .expect("declared");
        assert_eq!(bytes.len(), 48);
        let read = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        for column in 0..3 {
            for row in 0..3 {
                assert_eq!(read(column * 16 + row * 4), cells[column * 3 + row]);
            }
            // The padding word each column ends with.
            assert_eq!(read(column * 16 + 12), 0.0);
        }
    }

    #[test]
    fn a_bool_is_a_u32_in_the_buffer_and_a_comparison_in_the_shader() {
        // WGSL has no `bool` in the uniform address space at all, so this
        // is not a choice so much as the only encoding available.
        let laid_out = layout(&[("enabled", ValueType::Bool)]);
        assert_eq!(
            laid_out.field("enabled").expect("declared").ty,
            ValueType::Bool
        );
        assert!(laid_out.wgsl_struct("P").contains("enabled: u32"));
        assert_eq!(
            laid_out.read_expr("material", "enabled").as_deref(),
            Some("(material.enabled != 0u)")
        );
        let mut bytes = laid_out.zeroed();
        laid_out
            .write(&mut bytes, "enabled", Value::Bool(true))
            .expect("declared");
        assert_eq!(bytes[0..4], [1, 0, 0, 0]);
    }

    #[test]
    fn every_value_type_survives_a_round_trip_through_the_layout() {
        // The check the mirrored layouts elsewhere get by construction —
        // a `#[repr(C)]` struct and a WGSL struct with the same fields —
        // and that this one cannot, because there is no struct. Every
        // type, in one buffer, so a field is read back past its
        // neighbours` padding rather than out of a buffer of its own.
        let values = [
            ("flag", Value::Bool(true)),
            ("count", Value::I32(-7)),
            ("index", Value::U32(9)),
            ("scale", Value::F32(0.25)),
            ("offset", Value::Vec2([1.0, 2.0])),
            ("tint", Value::Vec3([3.0, 4.0, 5.0])),
            ("color", Value::Vec4([6.0, 7.0, 8.0, 9.0])),
            (
                "basis",
                Value::Mat3([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]),
            ),
            ("transform", Value::Mat4(core::array::from_fn(|i| i as f32))),
        ];
        let laid_out =
            BufferLayout::uniform(values.iter().map(|(name, value)| (ident(name), value.ty())));
        let mut bytes = laid_out.zeroed();
        for (name, value) in &values {
            laid_out.write(&mut bytes, name, *value).expect("declared");
        }
        for (name, value) in &values {
            assert_eq!(laid_out.read(&bytes, name), Some(*value), "`{name}`");
        }
    }

    #[test]
    fn writing_the_wrong_type_is_an_error_rather_than_a_reinterpretation() {
        let laid_out = layout(&[("tint", ValueType::Vec3)]);
        let mut bytes = laid_out.zeroed();
        assert!(matches!(
            laid_out.write(&mut bytes, "tint", Value::F32(1.0)),
            Err(LayoutError::TypeMismatch { .. })
        ));
        assert!(matches!(
            laid_out.write(&mut bytes, "nope", Value::Vec3([0.0; 3])),
            Err(LayoutError::UnknownField { .. })
        ));
    }

    #[test]
    fn the_generated_struct_lists_the_fields_in_layout_order() {
        let laid_out = layout(&[("amount", ValueType::F32), ("tint", ValueType::Vec3)]);
        // And a fresh buffer starts at the declared values, at the
        // computed offsets.
        let defaults: BTreeMap<String, Value> = [
            ("amount".to_string(), Value::F32(0.5)),
            ("tint".to_string(), Value::Vec3([1.0, 0.0, 0.0])),
        ]
        .into_iter()
        .collect();
        let bytes = laid_out.filled(&defaults);
        let read = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        assert_eq!(read(0), 1.0);
        assert_eq!(read(12), 0.5);
        let source = laid_out.wgsl_struct("MaterialParams");
        let tint = source.find("tint").expect("declared");
        let amount = source.find("amount").expect("declared");
        assert!(tint < amount, "{source}");
    }

    #[test]
    fn resources_are_not_buffer_fields() {
        for ty in ValueType::RESOURCES {
            assert_eq!(ty.buffer_align(), None);
            assert_eq!(ty.buffer_size(), None);
            assert_eq!(ty.buffer_type(), None);
        }
        // And a caller that ignores that is not corrupted by it: the
        // resource is dropped rather than laid out at a made-up size.
        let laid_out = layout(&[("tex", ValueType::Texture2d), ("amount", ValueType::F32)]);
        assert_eq!(laid_out.fields().len(), 1);
    }

    #[test]
    fn two_interfaces_differ_by_shape_and_never_by_value() {
        let one = MaterialInterface {
            params: layout(&[("amount", ValueType::F32)]),
            defaults: [("amount".to_string(), Value::F32(0.25))]
                .into_iter()
                .collect(),
            ..MaterialInterface::default()
        };
        let same = MaterialInterface {
            params: layout(&[("amount", ValueType::F32)]),
            // A different starting value, and the same signature: this is
            // the assertion that keeps a slider from being a recompile.
            defaults: [("amount".to_string(), Value::F32(0.75))]
                .into_iter()
                .collect(),
            ..MaterialInterface::default()
        };
        let other = MaterialInterface {
            params: layout(&[("amount", ValueType::Vec3)]),
            ..MaterialInterface::default()
        };
        assert_eq!(one.signature(), same.signature());
        assert_ne!(one.signature(), other.signature());
        assert!(MaterialInterface::default().material_group_is_empty());
        assert!(!one.material_group_is_empty());
    }
}
