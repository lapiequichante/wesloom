//! The shader ABI: the fixed WXSL vocabulary a generated material module is
//! written against.
//!
//! A material graph does not describe a whole shader — it describes the
//! *surface*: given a [`CONTEXT_STRUCT`] of per-fragment inputs, produce a
//! [`SURFACE_STRUCT`] of material properties. Everything around that (vertex
//! transform, light loop, G-buffer packing) is hand-written WXSL living in the
//! modules named here, which `wxsl-stdlib` ships.
//!
//! This module is the single place where the two halves agree on names, so
//! drift shows up as one edit here plus one in the `.wxsl` source rather than
//! as a mystifying shader-compiler error. See
//! [ADR 0008](../../../docs/adr/0008-surface-graphs-and-a-named-shader-abi.md).
//!
//! The [`context_node_defs`] and [`surface_output_def`] functions turn the
//! same tables into node definitions, so the graph's entry and exit nodes are
//! generated from the ABI instead of being declared twice.

use crate::macros::{MacroDef, MacroValue};
use crate::node::{GenericParam, NodeDefinition, SettingDef, Socket, Value, ValueType};

/// Module holding [`CONTEXT_STRUCT`], [`SURFACE_STRUCT`] and
/// [`DEFAULT_SURFACE_FN`].
pub const SURFACE_MODULE: &str = "package::wxsl::surface";
/// Module holding the vertex stage and the context builder.
pub const VERTEX_MODULE: &str = "package::wxsl::vertex";
/// Module holding the forward-path shading function.
pub const SHADING_MODULE: &str = "package::wxsl::shading";
/// Module holding the G-buffer struct and packing function.
pub const DEFERRED_MODULE: &str = "package::wxsl::deferred";
/// Per-fragment inputs handed to the material function.
pub const CONTEXT_STRUCT: &str = "SurfaceContext";
/// Parameter name a generated function receives a [`CONTEXT_STRUCT`] as.
pub const CONTEXT_VAR: &str = "ctx";
/// Parameter name a generated function receives a
/// [`VERTEX_CONTEXT_STRUCT`] as.
pub const VERTEX_CONTEXT_VAR: &str = "vtx";
/// Material properties the material function returns.
pub const SURFACE_STRUCT: &str = "Surface";
/// Returns a [`SURFACE_STRUCT`] pre-filled from a [`CONTEXT_STRUCT`]: sensible
/// material defaults, and the geometric normal. The generated material
/// function starts from this and overwrites only the fields the graph drives,
/// which is how an unconnected `normal` input keeps the geometric normal.
pub const DEFAULT_SURFACE_FN: &str = "default_surface";

/// Vertex stage input struct (matches `wxsl-render`'s vertex buffer layout).
pub const VERTEX_IN_STRUCT: &str = "VertexIn";
/// Vertex stage output / fragment stage input struct.
pub const VERTEX_OUT_STRUCT: &str = "VertexOut";
/// Vertex stage body, shared by both render paths.
pub const TRANSFORM_VERTEX_FN: &str = "transform_vertex";
/// Builds a [`CONTEXT_STRUCT`] from a [`VERTEX_OUT_STRUCT`].
pub const SURFACE_CONTEXT_FN: &str = "surface_context";

/// Per-vertex inputs handed to the generated vertex function.
///
/// The vertex stage's counterpart to [`CONTEXT_STRUCT`], and deliberately
/// a *superset* of it: every field a fragment-side node reads is here too,
/// computed before any displacement, plus the two only the vertex stage
/// has. That is what lets one `input.uv` node serve both stages instead of
/// forking the input vocabulary in half
/// ([ADR 0025](../../../docs/adr/0025-a-material-graph-spans-shader-stages.md)).
pub const VERTEX_CONTEXT_STRUCT: &str = "VertexContext";
/// Builds a [`VERTEX_CONTEXT_STRUCT`] from a [`VERTEX_IN_STRUCT`].
pub const VERTEX_CONTEXT_FN: &str = "vertex_context";
/// [`TRANSFORM_VERTEX_FN`] with an object-space position offset applied
/// before the model transform.
pub const TRANSFORM_VERTEX_OFFSET_FN: &str = "transform_vertex_offset";

/// Struct a generated module declares for the *declared* per-vertex
/// attributes, taken as the vertex entry's **second** parameter.
///
/// A second parameter rather than a wider [`VERTEX_IN_STRUCT`], which is
/// the whole reason the base vertex format stays fixed ABI text: WGSL lets
/// an entry point take several IO parameters, so what a material adds sits
/// beside what the ABI fixed instead of replacing it (ADR 0024).
pub const MATERIAL_VERTEX_IN_STRUCT: &str = "MaterialVertexIn";
/// Struct a generated module declares as its vertex entry's *return* when
/// it has anything to pass down beyond [`VERTEX_OUT_FIELDS`].
///
/// An entry point returns one value, so this one cannot be split: it
/// repeats the base varyings, at the same locations, and adds the extras
/// above them. [`VERTEX_OUT_FIELDS`] is the table both halves are written
/// from.
pub const MATERIAL_VERTEX_OUT_STRUCT: &str = "MaterialVertexOut";
/// Struct holding only the *extra* varyings, taken as the fragment entry's
/// second parameter beside a plain [`VERTEX_OUT_STRUCT`].
///
/// This is the trick that keeps [`SURFACE_CONTEXT_FN`] unchanged: the
/// fragment stage still receives the ABI's own struct, and the material's
/// additions arrive alongside it.
pub const MATERIAL_VARYINGS_STRUCT: &str = "MaterialVaryings";
/// Plain (non-IO) struct handed to the material function as its second
/// argument, holding what the geometry supplied.
pub const MATERIAL_ATTRIBUTES_STRUCT: &str = "MaterialAttributes";
/// Parameter name the material function receives a
/// [`MATERIAL_ATTRIBUTES_STRUCT`] under.
pub const MATERIAL_ATTRIBUTES_VAR: &str = "attrs";
/// Field of [`MATERIAL_ATTRIBUTES_STRUCT`] and [`MATERIAL_VARYINGS_STRUCT`]
/// carrying `@builtin(instance_index)` down to the fragment stage.
///
/// WGSL offers that builtin in the vertex stage only, so a fragment that
/// wants per-instance data has to be *told* which instance it is. One
/// `@interpolate(flat) u32` covers every declared instance attribute
/// however many there are, which is why the index travels rather than the
/// values.
pub const INSTANCE_INDEX_FIELD: &str = "wxsl_instance";
/// Struct a generated module declares for one instance's *declared*
/// attributes — the per-instance half of what a material requires of its
/// geometry.
pub const MATERIAL_INSTANCE_STRUCT: &str = "MaterialInstance";
/// Variable the declared per-instance attributes are bound as, at
/// [`BINDING_INSTANCE_ATTRIBUTES`] of [`GROUP_FRAME`].
///
/// Indexed by the same `@builtin(instance_index)` [`BINDING_INSTANCES`]
/// is, so the two arrays are one row apiece for the same object and
/// neither has to know the other's width (ADR 0024).
pub const MATERIAL_INSTANCE_VAR: &str = "wxsl_instance_attributes";

/// One `@location` of an entry-point IO struct fixed by the ABI.
///
/// The index in the table is the location, so a row inserted in the middle
/// renumbers the rest — which is what a table is for. `wxsl-render`'s
/// `mesh` module and `shaders/wxsl/vertex.wxsl` are the other two views,
/// and `wxsl-stdlib` has a test that the third agrees with this one.
pub struct VertexField {
    /// Field name in the WXSL struct.
    pub name: &'static str,
    /// Field type.
    pub ty: ValueType,
}

/// Every `@location` of [`VERTEX_IN_STRUCT`], in location order.
///
/// A material's own declared per-vertex attributes are numbered from
/// `VERTEX_IN_FIELDS.len()` upwards.
pub const VERTEX_IN_FIELDS: &[VertexField] = &[
    VertexField {
        name: "position",
        ty: ValueType::Vec3,
    },
    VertexField {
        name: "normal",
        ty: ValueType::Vec3,
    },
    VertexField {
        name: "tangent",
        ty: ValueType::Vec4,
    },
    VertexField {
        name: "uv",
        ty: ValueType::Vec2,
    },
];

/// Every `@location` of [`VERTEX_OUT_STRUCT`], in location order.
///
/// `@builtin(position)` is not in the table: it has no location and it is
/// always first. Codegen writes [`MATERIAL_VERTEX_OUT_STRUCT`] from this,
/// so the extended struct's base half cannot drift from the ABI's.
pub const VERTEX_OUT_FIELDS: &[VertexField] = &[
    VertexField {
        name: "world_position",
        ty: ValueType::Vec3,
    },
    VertexField {
        name: "world_normal",
        ty: ValueType::Vec3,
    },
    VertexField {
        name: "world_tangent",
        ty: ValueType::Vec3,
    },
    VertexField {
        name: "world_bitangent",
        ty: ValueType::Vec3,
    },
    VertexField {
        name: "uv",
        ty: ValueType::Vec2,
    },
];

/// Field of [`VERTEX_OUT_STRUCT`] carrying `@builtin(position)`.
pub const CLIP_POSITION_FIELD: &str = "clip_position";

/// What one row of [`BINDING_INSTANCES`] holds, in buffer order.
///
/// Fixed ABI, and it stays fixed: `wxsl_render::environment::
/// InstanceTransform` is its `#[repr(C)]` mirror, `Instance` in
/// `bindings.wxsl` is its WGSL half, and [`TRANSFORM_VERTEX_FN`] reads
/// the row through that. A material's own per-instance attributes are a
/// *separate* array at [`BINDING_INSTANCE_ATTRIBUTES`] rather than fields
/// appended here, because a wider row would give this array two strides
/// and the vertex stage reads it at this one (ADR 0024).
pub const INSTANCE_BASE_FIELDS: &[VertexField] = &[
    VertexField {
        name: "model",
        ty: ValueType::Mat4,
    },
    VertexField {
        name: "normal_matrix",
        ty: ValueType::Mat4,
    },
];

/// How many `@location` slots an inter-stage interface has.
///
/// WebGPU guarantees 16. [`VERTEX_OUT_FIELDS`] spends five of them, the
/// instance index one more when a material reads per-instance data, and
/// every declared per-vertex attribute one each. One accountant, so a
/// material that overruns the budget is told which line item did it
/// rather than discovering it as a shader-compiler error.
pub const MAX_VARYING_LOCATIONS: usize = 16;

/// How many per-vertex attributes a material may declare.
///
/// Each is its own vertex buffer (ADR 0024), and WebGPU guarantees eight
/// slots with the base stream taking one. Four rather than seven, so that
/// a mesh may carry a couple of streams no material asked for without the
/// budget being an accident of what happens to be bound.
pub const MAX_VERTEX_ATTRIBUTES: usize = 4;

/// Shades a surface to a final `vec4f` colour
/// ([`MaterialStage::FORWARD_LIT`]).
pub const SHADE_SURFACE_FN: &str = "shade_surface";
/// The G-buffer fragment output struct ([`MaterialStage::GBUFFER`]).
pub const GBUFFER_STRUCT: &str = "GBuffer";
/// Packs a surface into the G-buffer ([`MaterialStage::GBUFFER`]).
pub const PACK_GBUFFER_FN: &str = "pack_gbuffer";

/// Shadow map bindings and the filtered lookup over them.
///
/// Its own module rather than more of `bindings.wxsl`, because it is the
/// one part of the frame group with an algorithm attached: the PCF kernel
/// and the normal-offset bias live with the two bindings they read.
/// `shading.wxsl` imports it, so both render paths get the same lookup.
pub const SHADOW_MODULE: &str = "package::wxsl::shadow";
/// Attenuation of one light at one point, in `[0, 1]`
/// ([`SHADOW_MODULE`]).
pub const SHADOW_FACTOR_FN: &str = "shadow_factor";

/// Deferred path: the standalone module holding the lighting pass, compiled
/// as its own root (it shades the G-buffer, so it has no material graph).
pub const LIGHTING_PASS_MODULE: &str = "package::wxsl::lighting_pass";
/// Vertex entry point of [`LIGHTING_PASS_MODULE`] (a fullscreen triangle).
pub const LIGHTING_PASS_VERTEX_ENTRY: &str = "lighting_vs";
/// Fragment entry point of [`LIGHTING_PASS_MODULE`].
pub const LIGHTING_PASS_FRAGMENT_ENTRY: &str = "lighting_fs";

/// The four bind-group slots, fixed for the life of the ABI.
///
/// `maxBindGroups` is 4 in WebGPU and in `wgpu`, so this is the whole budget.
/// Slots are ordered by how often their contents change, least often first,
/// because a backend may disturb higher-numbered groups when a lower one is
/// rebound. See
/// [ADR 0010](../../../docs/adr/0010-four-bind-groups-allocated-by-update-frequency.md)
/// for why five update frequencies fit in four slots, and what it costs.
pub const BIND_GROUPS: &[BindGroup] = &[
    BindGroup {
        index: GROUP_FRAME,
        name: "frame",
        doc: "Camera, scene lighting and the instance transform buffer. Built once per frame.",
        application_owned: false,
    },
    BindGroup {
        index: GROUP_MATERIAL,
        name: "material",
        doc: "A material's uniform parameters, textures and samplers, all declared by its graph.",
        application_owned: false,
    },
    BindGroup {
        index: GROUP_USER,
        name: "user",
        doc: "The application's own slot. A material may declare the block it expects to find here, but never what is in it.",
        application_owned: true,
    },
    BindGroup {
        index: GROUP_PASS,
        name: "pass",
        doc: "Resources a pipeline shape needs, such as the G-buffer read by the deferred lighting pass.",
        application_owned: false,
    },
];

/// One entry of [`BIND_GROUPS`].
pub struct BindGroup {
    /// The `@group(N)` index.
    pub index: u32,
    /// Short name, used in labels and diagnostics.
    pub name: &'static str,
    /// What the slot holds.
    pub doc: &'static str,
    /// Whether node definitions and applications may bind into it.
    pub application_owned: bool,
}

/// Per-frame data: camera, scene, and the instance transforms
/// (`package::wxsl::bindings`). Rebound once per frame, and once only —
/// every draw in the frame indexes the same instance buffer.
pub const GROUP_FRAME: u32 = 0;
/// Per-material data: the uniform parameters a graph exposes, plus its
/// textures and samplers.
///
/// Its layout is not fixed by this module, because it is not fixed at all:
/// the graph decides it, and
/// [`crate::resources::MaterialInterface`] is the computed answer that
/// codegen emits and the renderer binds against (ADR 0023).
pub const GROUP_MATERIAL: u32 = 1;
/// The application's own slot.
///
/// Nothing in wxsl *owns* anything here. A material may declare the block
/// it expects to find at [`BINDING_USER_BLOCK`] and hand out the layout,
/// and the application hands back a bind group — so the two agree through
/// `wgpu`'s own validation rather than through a convention either side
/// could drift from (ADR 0023).
pub const GROUP_USER: u32 = 2;
/// Per-pass data, such as the G-buffer the deferred lighting pass samples.
///
/// Highest-numbered on purpose: rebinding it disturbs no other group, and it
/// is bound once per pass, where the index costs nothing. Not offered to node
/// authors — a graph must compile for either render path (ADR 0005), and a
/// node claiming this slot would work in forward and collide in deferred.
pub const GROUP_PASS: u32 = 3;

/// [`GROUP_FRAME`] binding of the camera uniform.
pub const BINDING_CAMERA: u32 = 0;
/// [`GROUP_FRAME`] binding of the scene uniform (lights, ambient, time).
pub const BINDING_SCENE: u32 = 1;
/// [`GROUP_MATERIAL`] binding of the material's uniform parameter buffer.
///
/// Always reserved, even for a material that declares no parameters: a
/// graph that gains its first parameter must not renumber the textures
/// already bound after it, because a binding index that moves is a bind
/// group that has to be rebuilt for no reason. An unused binding in a
/// pipeline layout costs nothing.
pub const BINDING_MATERIAL_PARAMS: u32 = 0;
/// First [`GROUP_MATERIAL`] binding available to a declared texture or
/// sampler; they take the indices upwards from here, in name order.
pub const MATERIAL_RESOURCE_BINDING_BASE: u32 = BINDING_MATERIAL_PARAMS + 1;
/// [`GROUP_USER`] binding of the block a material *requires the
/// application to supply*.
///
/// The one binding in the application's own group that wxsl has anything
/// to say about — and it says only what shape it expects, never what is
/// in it (ADR 0023).
pub const BINDING_USER_BLOCK: u32 = 0;

/// Name of the generated struct holding a material's uniform parameters.
pub const MATERIAL_PARAMS_STRUCT: &str = "MaterialParams";
/// Name of the generated variable holding a material's uniform parameters.
pub const MATERIAL_PARAMS_VAR: &str = "material";

/// Names a generated material module reserves for itself, which a graph
/// may therefore not give a texture, a sampler or an application block.
///
/// Short, because everything else in a generated module is either
/// prefixed (`n3_out`, which
/// [`crate::error::GraphError::InvalidSetting`] also rejects) or imported
/// under a mangled name.
pub const RESERVED_NAMES: &[&str] = &[
    MATERIAL_ATTRIBUTES_VAR,
    MATERIAL_INSTANCE_VAR,
    INSTANCE_INDEX_FIELD,
    MATERIAL_PARAMS_VAR,
    MATERIAL_PARAMS_STRUCT,
    "ctx",
    "surface",
];

/// [`GROUP_FRAME`] binding of the instance transform storage buffer.
///
/// A storage buffer, not a uniform with a dynamic offset: one binding and
/// one upload serve every draw in the frame, and each vertex finds its own
/// row with `@builtin(instance_index)`. This supersedes ADR 0010's
/// per-object dynamic offset (ADR 0021).
pub const BINDING_INSTANCES: u32 = 2;

/// [`GROUP_FRAME`] binding of the per-instance attributes a material
/// declares.
///
/// Beside [`BINDING_INSTANCES`] rather than inside it, and indexed by the
/// same `@builtin(instance_index)`. The transform array's stride is ABI
/// and the attribute array's is whatever the graph declared, so keeping
/// them apart is what lets one be hand-written and the other computed
/// without either having to know the other's width.
///
/// Always in the frame group's layout, even for a material that declares
/// none: an unused binding costs nothing, and a layout that changed shape
/// per material would invalidate every pipeline in the cache
/// ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
pub const BINDING_INSTANCE_ATTRIBUTES: u32 = 3;

/// [`GROUP_FRAME`] binding of the shadow maps, one array layer per light.
///
/// In the frame group and not the pass group, because a shadow map is
/// frame-global in exactly the way the lights it comes from are: the
/// forward stage and the deferred lighting pass read it through the same
/// binding, so `shading.wxsl` needs one lookup rather than one per path.
/// The pass group could not do that — it already holds the G-buffer in
/// the deferred path, and its bindings are numbered per pass.
pub const BINDING_SHADOW_MAPS: u32 = 4;

/// [`GROUP_FRAME`] binding of the comparison sampler used with
/// [`BINDING_SHADOW_MAPS`].
///
/// A `sampler_comparison`, so the hardware does the depth test and the
/// bilinear filter in one fetch: that is what makes a PCF kernel four
/// taps' worth of filtering per tap rather than a hand-rolled average of
/// hard comparisons.
pub const BINDING_SHADOW_SAMPLER: u32 = 5;

/// How many lights the scene uniform carries, and therefore how many
/// slices the shadow map array has.
///
/// Fixed rather than a macro variable: it is the length of an array in a
/// host-shared buffer, so it cannot vary per shader variant without the
/// Rust and WXSL halves disagreeing about the layout. Must match
/// `WXSL_MAX_LIGHTS` in `shaders/wxsl/bindings.wxsl`.
pub const MAX_LIGHTS: usize = 4;

/// Edge length in texels of one shadow map slice.
///
/// One fixed resolution for every light, and no packing code. An atlas
/// with a rect per light is the eventual answer — a shadow needs more
/// texels the closer the light is to what it falls on — and it is a
/// contained change when it comes: one [`BINDING_SHADOW_MAPS`] of a
/// different shape, plus a rect in the light.
pub const SHADOW_MAP_RESOLUTION: u32 = 1024;

/// How much precision a G-buffer target needs.
///
/// The ABI says what each target carries and therefore what precision it
/// needs; `wxsl-render` maps that onto concrete `wgpu` texture formats,
/// which is knowledge this crate deliberately does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GBufferPrecision {
    /// Values in `[0, 1]`: 8-bit normalized is enough.
    Normalized,
    /// Values outside `[0, 1]`, or needing a sign: half float.
    HighDynamicRange,
}

/// One G-buffer render target written by the deferred fragment entry.
pub struct GBufferTarget {
    /// The [`GBUFFER_STRUCT`] field written to this target.
    pub field: &'static str,
    /// What the four channels hold.
    pub doc: &'static str,
    /// Precision the contents need.
    pub precision: GBufferPrecision,
}

/// The G-buffer layout, in `@location` order.
///
/// Packing two quantities per target keeps the deferred path at three colour
/// attachments plus depth. `wxsl-stdlib`'s [`PACK_GBUFFER_FN`] and the
/// lighting pass are the two ends of this contract.
pub const GBUFFER_TARGETS: &[GBufferTarget] = &[
    GBufferTarget {
        field: "base_color",
        doc: "rgb = linear base colour, a = metallic",
        precision: GBufferPrecision::Normalized,
    },
    GBufferTarget {
        field: "normal",
        doc: "rgb = world-space shading normal, a = roughness",
        precision: GBufferPrecision::HighDynamicRange,
    },
    GBufferTarget {
        field: "emissive",
        doc: "rgb = emissive radiance, a = ambient occlusion",
        precision: GBufferPrecision::HighDynamicRange,
    },
];

/// Macro variables the ABI itself honours, with their defaults.
///
/// These are not declared by any node — they switch behaviour inside the
/// hand-written ABI modules — so a renderer seeds them as defaults under
/// whatever the graph pins. Which *stage* a material is compiled for is not
/// among them and never was: a stage is chosen by the pass, and it is not a
/// flag at all any more (ADR 0022) but a separate generated module.
pub fn abi_macros() -> Vec<MacroDef> {
    vec![
        MacroDef::new(
            FEATURE_TONEMAP,
            MacroValue::Flag(true),
            "Apply the filmic tonemap curve before writing the final colour.",
        ),
        MacroDef::new(
            FEATURE_DEBUG_NORMALS,
            MacroValue::Flag(false),
            "Shade surfaces as their world-space normal instead of lighting them.",
        ),
        MacroDef::new(
            FEATURE_RECEIVE_SHADOWS,
            MacroValue::Flag(true),
            "Attenuate each light by the shadow map it casts into.",
        ),
        MacroDef::new(
            FEATURE_RELATIVE_TO_EYE,
            MacroValue::Flag(false),
            "Measure world-space positions from the camera rather than the origin.",
        ),
        MacroDef::new(
            FEATURE_PREVIOUS_FRAME,
            MacroValue::Flag(false),
            "Read the previous frame's clock, so a time-driven graph answers for where it was.",
        ),
    ]
}

/// Feature flag: tonemap the shaded colour (see [`abi_macros`]).
pub const FEATURE_TONEMAP: &str = "wxsl_tonemap";
/// Feature flag: output normals instead of shading (see [`abi_macros`]).
pub const FEATURE_DEBUG_NORMALS: &str = "wxsl_debug_normals";
/// Feature flag: sample the shadow maps when shading (see [`abi_macros`]).
///
/// What a material's `receive_shadow` becomes. A flag rather than a
/// uniform, so a material that does not receive shadows does not compile
/// the lookup at all — and so the two paths agree, since the deferred
/// lighting pass is compiled against the same macro set.
pub const FEATURE_RECEIVE_SHADOWS: &str = "wxsl_receive_shadows";
/// Feature flag: world space is measured from the camera (see
/// [`abi_macros`]).
///
/// The ABI half of relative-to-eye rendering, landed while the vertex
/// stage was open rather than retrofitted once many materials read
/// `world_position`: with the flag on, `SurfaceContext::world_position` is
/// camera-relative and the eye is at the origin. `world_origin()` in
/// `bindings.wxsl` is the one explicit way back to absolute space, and the
/// only two things that take it are a point light's falloff and the shadow
/// lookup.
///
/// What the flag does *not* yet buy is the precision it exists for: that
/// needs model matrices pre-translated by the camera in `f64` on the host,
/// which is a milestone of its own.
pub const FEATURE_RELATIVE_TO_EYE: &str = "wxsl_relative_to_eye";
/// Feature flag: time-driven nodes read the previous frame's clock (see
/// [`abi_macros`]).
///
/// The shape a velocity stage needs, settled while the vertex stage was
/// open. A motion vector for an object the *graph* moves is wrong unless
/// the vertex offset itself is re-evaluated for the previous frame, and
/// one switch over the whole graph is the only version of that an author
/// cannot forget half of.
pub const FEATURE_PREVIOUS_FRAME: &str = "wxsl_previous_frame";

// ---------------------------------------------------------------------------
// The UI pass
// ---------------------------------------------------------------------------

/// Module holding the 2D UI pass the editor draws itself with.
///
/// The editor's own chrome goes through the WXSL compiler and the variant
/// cache like any material
/// ([ADR 0013](../../../docs/adr/0013-the-editor-draws-itself-with-wxsl-render.md)),
/// so the shader is part of the library the application supplies (ADR 0009)
/// rather than a string hidden inside `wxsl-render`.
pub const UI_MODULE: &str = "package::wxsl::ui";
/// Vertex entry point of [`UI_MODULE`].
pub const UI_VERTEX_ENTRY: &str = "ui_vs";
/// Fragment entry point of [`UI_MODULE`].
pub const UI_FRAGMENT_ENTRY: &str = "ui_fs";

/// [`GROUP_PASS`] binding of the UI viewport uniform.
///
/// The UI pass binds nothing but the pass group: it has no camera, no lights
/// and no material, and a viewport plus an atlas is precisely "resources a
/// pipeline shape needs" (ADR 0010). Reusing [`GROUP_FRAME`] for the
/// viewport would give group 0 two incompatible meanings depending on which
/// pass is recording.
pub const BINDING_UI_VIEWPORT: u32 = 0;
/// [`GROUP_PASS`] binding of the texture a UI batch samples (the glyph and
/// image atlas, or an offscreen render such as the material preview).
pub const BINDING_UI_TEXTURE: u32 = 1;
/// [`GROUP_PASS`] binding of the sampler used with [`BINDING_UI_TEXTURE`].
pub const BINDING_UI_SAMPLER: u32 = 2;

/// How many vertices one UI primitive's quad is drawn from.
///
/// The corners come from `@builtin(vertex_index)`, not from a vertex buffer:
/// every UI primitive is one *instance*, so a glyph costs one
/// [`UI_ATTRIBUTES`] record rather than four vertices and six indices. Two
/// triangles, six indices' worth of vertices, no index buffer at all.
pub const UI_QUAD_VERTICES: u32 = 6;

/// One per-instance attribute of a UI primitive, in `@location` order.
///
/// Host-shared, like [`VERTEX_IN_STRUCT`]: `wxsl_render::ui`'s
/// `#[repr(C)]` instance, this table and `shaders/wxsl/ui.wxsl` are three
/// views of one layout and are edited together.
///
/// The step mode is `Instance`, and there is no per-vertex buffer.
pub struct UiAttribute {
    /// Field name in the WXSL struct.
    pub name: &'static str,
    /// WGSL type of the attribute.
    pub ty: &'static str,
    /// What it carries.
    pub doc: &'static str,
}

/// The UI instance layout, in `@location` order.
pub const UI_ATTRIBUTES: &[UiAttribute] = &[
    UiAttribute {
        name: "center",
        ty: "vec2f",
        doc: "Centre of the primitive in physical pixels, y down from the top left.",
    },
    UiAttribute {
        name: "half_extent",
        ty: "vec2f",
        doc: "Half the primitive's size in its own frame. The signed distance is \
              evaluated against this, so a capsule is an axis-aligned rounded box \
              however the quad is rotated on screen.",
    },
    UiAttribute {
        name: "axis",
        ty: "vec2f",
        doc: "Unit vector the primitive's local x axis points along on screen. \
              (1, 0) for anything axis-aligned; a line's direction for a capsule.",
    },
    UiAttribute {
        name: "shape",
        ty: "vec2f",
        doc: "x = corner radius (glyph runs: the distance field's range in screen \
              pixels), y = border thickness, 0 for a filled primitive.",
    },
    UiAttribute {
        name: "uv_min",
        ty: "vec2f",
        doc: "Texture coordinate of the primitive's top-left corner.",
    },
    UiAttribute {
        name: "uv_max",
        ty: "vec2f",
        doc: "Texture coordinate of the primitive's bottom-right corner.",
    },
    UiAttribute {
        name: "color",
        ty: "vec4f",
        doc: "Straight (non-premultiplied) RGBA tint, in the target's colour space \
              — the UI pass converts nothing.",
    },
    UiAttribute {
        name: "kind",
        ty: "u32",
        doc: "Which of `UI_KINDS` this primitive is. Flat-interpolated.",
    },
];

/// One primitive kind the UI fragment stage knows how to shade.
pub struct UiKind {
    /// The `kind` attribute's value.
    pub value: u32,
    /// Name of the WXSL constant, and of the Rust enum variant.
    pub name: &'static str,
    /// How the fragment stage shades it.
    pub doc: &'static str,
}

/// Every UI primitive kind, by `kind` value.
///
/// Three kinds cover a node editor: a signed-distance box (which, with the
/// right half extent and radius, is also a circle, a capsule, a hairline and
/// a border ring), a textured quad, and a glyph run.
pub const UI_KINDS: &[UiKind] = &[
    UiKind {
        value: UI_KIND_SHAPE,
        name: "UI_KIND_SHAPE",
        doc: "Rounded box from the signed distance to `shape`, filled or stroked.",
    },
    UiKind {
        value: UI_KIND_TEXTURE,
        name: "UI_KIND_TEXTURE",
        doc: "The batch's texture, tinted by `color` and masked by the same \
              rounded-box distance, so an image can have rounded corners.",
    },
    UiKind {
        value: UI_KIND_TEXT,
        name: "UI_KIND_TEXT",
        doc: "A multi-channel signed distance field glyph: the median of the \
              texture's RGB, scaled by `shape.z` screen pixels of range.",
    },
];

// ---------------------------------------------------------------------------
// The MSDF generation pass
// ---------------------------------------------------------------------------

/// Module holding the compute pass that generates glyph distance fields.
///
/// The renderer can build a glyph's field on the CPU or on the GPU
/// ([ADR 0014](../../../docs/adr/0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md));
/// this is the GPU half, and it is a WXSL module in the library for the same
/// reason the UI pass is (ADR 0009). The two implementations must agree, and
/// a test compares them glyph for glyph.
pub const MSDF_MODULE: &str = "package::wxsl::msdf";
/// Compute entry point of [`MSDF_MODULE`].
pub const MSDF_ENTRY: &str = "msdf_main";
/// The `@workgroup_size` of [`MSDF_ENTRY`], as `[x, y]`: one invocation per
/// pixel, with the glyph index in `z`.
pub const MSDF_WORKGROUP: [u32; 2] = [8, 8];

/// [`GROUP_PASS`] binding of the coloured edges of every glyph in a batch.
///
/// Numbered from zero in the pass group like the UI pass's bindings, and
/// distinct from them: a compute pipeline has its own layout, so the two
/// never collide (ADR 0010).
pub const BINDING_MSDF_EDGES: u32 = 0;
/// [`GROUP_PASS`] binding of the per-glyph jobs.
pub const BINDING_MSDF_JOBS: u32 = 1;
/// [`GROUP_PASS`] binding of the packed RGBA8 output pixels.
pub const BINDING_MSDF_PIXELS: u32 = 2;

/// Edge kind in [`MSDF_MODULE`]'s edge buffer: a straight line.
pub const MSDF_EDGE_LINE: u32 = 1;
/// Edge kind: a quadratic Bézier (TrueType outlines).
pub const MSDF_EDGE_QUAD: u32 = 2;
/// Edge kind: a cubic Bézier (CFF/OpenType outlines).
pub const MSDF_EDGE_CUBIC: u32 = 3;

/// [`UI_KINDS`] value: a signed-distance rounded box.
pub const UI_KIND_SHAPE: u32 = 0;
/// [`UI_KINDS`] value: a textured quad.
pub const UI_KIND_TEXTURE: u32 = 1;
/// [`UI_KINDS`] value: an MSDF glyph.
pub const UI_KIND_TEXT: u32 = 2;

/// Name of the generated material function.
pub const MATERIAL_FN: &str = "wxsl_material";
/// Name of the generated vertex function: the object-space position
/// offset a graph's vertex output produces.
///
/// A separate function from [`MATERIAL_FN`] because it runs in a
/// different shader stage, and a separate *subgraph*: codegen partitions
/// the graph by which output each node is reachable from, and a node
/// feeding both is emitted in both (ADR 0025).
pub const VERTEX_FN: &str = "wxsl_vertex";
/// Name of the generated discard function: whether this fragment should
/// be thrown away.
///
/// Its own function, and its own partition, because it is the *only*
/// thing a depth-only or shadow stage needs from the fragment side. A
/// stage that needs it and nothing else compiles the alpha subgraph and
/// leaves the rest of the material out of the module entirely.
pub const DISCARD_FN: &str = "wxsl_discard";
/// Prefix of the generated function computing one declared interpolant.
///
/// One function per interpolant rather than one returning all of them: a
/// graph that computes two of them from unrelated subgraphs should not
/// have to evaluate both to get one, and each is its own partition
/// anyway.
pub const VARYING_FN_PREFIX: &str = "wxsl_varying_";
/// Name of the generated vertex entry point.
pub const VERTEX_ENTRY: &str = "vs_main";

// ---------------------------------------------------------------------------
// Material stages
// ---------------------------------------------------------------------------

/// What a material stage's fragment entry returns.
///
/// This is the whole of what makes one stage different from another at M2:
/// the same surface graph, wrapped in a different entry point writing a
/// different set of targets. M5 adds the other half — *which part* of the
/// graph a stage needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StageOutput {
    /// One `vec4f` final colour at `@location(0)`.
    Color,
    /// A [`GBUFFER_STRUCT`], one target per [`GBUFFER_TARGETS`] entry.
    GBuffer,
    /// Nothing: the stage has no fragment entry point at all, and a
    /// pipeline built for it has no fragment state. Depth is written by
    /// the depth test, which needs no shader.
    Nothing,
}

impl StageOutput {
    /// How many colour attachments a pass running this stage must have.
    pub fn color_targets(self) -> usize {
        match self {
            StageOutput::Color => 1,
            StageOutput::GBuffer => GBUFFER_TARGETS.len(),
            StageOutput::Nothing => 0,
        }
    }
}

/// One entry of [`MATERIAL_STAGES`].
pub struct MaterialStageDesc {
    /// Identifier, as used in labels, on a command line and in the editor.
    pub name: &'static str,
    /// Name the fragment entry point takes *if* one is emitted.
    ///
    /// Always a name, never `None`: whether a
    /// [`StageOutput::Nothing`] stage has a fragment entry at all is a
    /// property of the *material*, not of the stage — one that discards
    /// needs a fragment stage to discard in, and one that does not needs
    /// no fragment state at all. `GeneratedShader::fragment_entry` is the
    /// answer for a given material (ADR 0025).
    pub fragment_entry: &'static str,
    /// What that entry returns.
    pub output: StageOutput,
    /// What the stage is for.
    pub doc: &'static str,
}

/// Every stage a surface graph can be compiled for, in declaration order.
///
/// A table, like [`GBUFFER_TARGETS`], and for the same reason: adding a
/// stage is a row here plus the constant that names it, not a new arm in
/// every match in the workspace. This replaces the two-valued render-path
/// enum, which had room for exactly two entry points and no more
/// ([ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
///
/// The stages the plan still owes — `Shadow`, `PeelFront`/`PeelBack`,
/// `Velocity` — are rows that do not exist yet. Each needs something else
/// first (a depth bias, the peel test, a previous-frame transform), which
/// is why they are not stubbed out here.
pub const MATERIAL_STAGES: &[MaterialStageDesc] = &[
    MaterialStageDesc {
        name: "forward_lit",
        fragment_entry: "fs_forward_lit",
        output: StageOutput::Color,
        doc: "Shade the surface where it is evaluated, and write the final colour.",
    },
    MaterialStageDesc {
        name: "gbuffer",
        fragment_entry: "fs_gbuffer",
        output: StageOutput::GBuffer,
        doc: "Write the surface into the G-buffer for a later lighting pass to shade.",
    },
    MaterialStageDesc {
        name: "depth_only",
        fragment_entry: "fs_depth_only",
        output: StageOutput::Nothing,
        doc: "Write depth and nothing else: a depth prepass. A material that \
              discards still gets a fragment stage here, because a prepass that \
              filled the holes would occlude what should show through them.",
    },
    MaterialStageDesc {
        name: "shadow",
        fragment_entry: "fs_shadow",
        output: StageOutput::Nothing,
        doc: "Write depth into a shadow map slice. The same shape as a depth \
              prepass, from a light's point of view — and the reason the vertex \
              and alpha subgraphs had to become separable at all.",
    },
];

/// Which stage a material is compiled for.
///
/// An index into [`MATERIAL_STAGES`] rather than an enum, so that the table
/// stays the single declaration. `Copy` and `Hash`, because it is part of
/// the variant cache key and of every pass description.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaterialStage(u8);

impl MaterialStage {
    /// Shade in the material's own fragment pass.
    pub const FORWARD_LIT: MaterialStage = MaterialStage(0);
    /// Write the surface into the G-buffer.
    pub const GBUFFER: MaterialStage = MaterialStage(1);
    /// Write depth and nothing else.
    pub const DEPTH_ONLY: MaterialStage = MaterialStage(2);
    /// Write depth into a shadow map, from a light's point of view.
    pub const SHADOW: MaterialStage = MaterialStage(3);

    /// Every stage, in table order.
    pub const ALL: &'static [MaterialStage] = &[
        MaterialStage::FORWARD_LIT,
        MaterialStage::GBUFFER,
        MaterialStage::DEPTH_ONLY,
        MaterialStage::SHADOW,
    ];

    /// The stage at `index` in [`MATERIAL_STAGES`], if there is one.
    pub fn from_index(index: usize) -> Option<Self> {
        (index < MATERIAL_STAGES.len()).then_some(MaterialStage(index as u8))
    }

    /// This stage's index in [`MATERIAL_STAGES`].
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// This stage's table row.
    pub fn desc(self) -> &'static MaterialStageDesc {
        &MATERIAL_STAGES[self.index()]
    }

    /// The stage's name.
    pub fn name(self) -> &'static str {
        self.desc().name
    }

    /// Parse a stage from its [`MaterialStage::name`].
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        MaterialStage::ALL
            .iter()
            .copied()
            .find(|stage| stage.name().eq_ignore_ascii_case(text))
    }

    /// The name this stage's fragment entry point takes if one is
    /// emitted. See [`MaterialStageDesc::fragment_entry`].
    pub fn fragment_entry(self) -> &'static str {
        self.desc().fragment_entry
    }

    /// Whether this stage needs the material's `Surface` at all.
    ///
    /// The whole of what partitioning turns on: a stage that writes no
    /// colour needs the vertex offset and the discard test, and nothing
    /// else from the graph (ADR 0025).
    pub fn needs_surface(self) -> bool {
        self.output() != StageOutput::Nothing
    }

    /// What the fragment entry returns.
    pub fn output(self) -> StageOutput {
        self.desc().output
    }

    /// How many colour attachments a pass running this stage must have.
    pub fn color_targets(self) -> usize {
        self.output().color_targets()
    }
}

impl Default for MaterialStage {
    fn default() -> Self {
        MaterialStage::FORWARD_LIT
    }
}

impl core::fmt::Debug for MaterialStage {
    /// The name, not the index: `MaterialStage(1)` in a panic message is a
    /// lookup the reader should not have to do.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "MaterialStage({})", self.name())
    }
}

impl core::fmt::Display for MaterialStage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `pad` so `{:>12}` in a progress line actually aligns.
        f.pad(self.name())
    }
}

/// One field of [`CONTEXT_STRUCT`], and the node that reads it.
///
/// Every one of these is also a field of [`VERTEX_CONTEXT_STRUCT`], at the
/// same name and type, which is why one node definition serves both
/// stages. [`VERTEX_ONLY_FIELDS`] is what the vertex context has *on top*.
pub struct ContextField {
    /// Field name in the WXSL struct, and the node id suffix.
    pub name: &'static str,
    /// Field type.
    pub ty: ValueType,
    /// Editor label.
    pub label: &'static str,
    /// What the field holds.
    pub doc: &'static str,
}

/// Every field of [`CONTEXT_STRUCT`], in declaration order.
///
/// The `.wxsl` struct must list exactly these, in this order.
pub const CONTEXT_FIELDS: &[ContextField] = &[
    ContextField {
        name: "world_position",
        ty: ValueType::Vec3,
        label: "World position",
        doc: "Fragment position in world space.",
    },
    ContextField {
        name: "world_normal",
        ty: ValueType::Vec3,
        label: "World normal",
        doc: "Interpolated geometric normal in world space, normalized.",
    },
    ContextField {
        name: "world_tangent",
        ty: ValueType::Vec3,
        label: "World tangent",
        doc: "Interpolated tangent in world space, normalized.",
    },
    ContextField {
        name: "world_bitangent",
        ty: ValueType::Vec3,
        label: "World bitangent",
        doc: "Interpolated bitangent in world space, normalized.",
    },
    ContextField {
        name: "view_direction",
        ty: ValueType::Vec3,
        label: "View direction",
        doc: "Unit vector from the fragment towards the camera.",
    },
    ContextField {
        name: "uv",
        ty: ValueType::Vec2,
        label: "UV",
        doc: "Interpolated texture coordinates.",
    },
    ContextField {
        name: "time",
        ty: ValueType::F32,
        label: "Time",
        doc: "Seconds since the renderer started, for animated materials.",
    },
];

/// Fields [`VERTEX_CONTEXT_STRUCT`] has that [`CONTEXT_STRUCT`] does not.
///
/// Object space, which the fragment stage has no access to and no use
/// for: by then the geometry has been transformed and interpolated. A
/// node reading one of these is therefore vertex-only, and
/// `Graph::validate` says so if it is wired into the surface.
pub const VERTEX_ONLY_FIELDS: &[ContextField] = &[
    ContextField {
        name: "object_position",
        ty: ValueType::Vec3,
        label: "Object position",
        doc: "The vertex's position in object space, before the model transform.",
    },
    ContextField {
        name: "object_normal",
        ty: ValueType::Vec3,
        label: "Object normal",
        doc: "The vertex's normal in object space, unit length.",
    },
];

/// One field of [`SURFACE_STRUCT`], i.e. one input of the output node.
pub struct SurfaceField {
    /// Field name in the WXSL struct, and the input socket name.
    pub name: &'static str,
    /// Field type.
    pub ty: ValueType,
    /// What the field means.
    pub doc: &'static str,
    /// Value shown in the editor when the input is unconnected. `None` means
    /// the field keeps whatever [`DEFAULT_SURFACE_FN`] put there — used for
    /// `normal`, whose default is the geometric normal and so cannot be
    /// written as a literal.
    pub editable_default: Option<Value>,
}

/// Every field of [`SURFACE_STRUCT`], in declaration order.
///
/// The `.wxsl` struct must list exactly these, in this order.
pub const SURFACE_FIELDS: &[SurfaceField] = &[
    SurfaceField {
        name: "base_color",
        ty: ValueType::Vec3,
        doc: "Linear-space diffuse albedo (metals: reflectance tint).",
        editable_default: Some(Value::Vec3([0.8, 0.8, 0.8])),
    },
    SurfaceField {
        name: "metallic",
        ty: ValueType::F32,
        doc: "0 = dielectric, 1 = conductor.",
        editable_default: Some(Value::F32(0.0)),
    },
    SurfaceField {
        name: "roughness",
        ty: ValueType::F32,
        doc: "Perceptual roughness; squared into the GGX alpha.",
        editable_default: Some(Value::F32(0.5)),
    },
    SurfaceField {
        name: "normal",
        ty: ValueType::Vec3,
        doc: "Shading normal in world space. Unconnected: geometric normal.",
        editable_default: None,
    },
    SurfaceField {
        name: "emissive",
        ty: ValueType::Vec3,
        doc: "Light emitted by the surface, added after shading.",
        editable_default: Some(Value::Vec3([0.0, 0.0, 0.0])),
    },
    SurfaceField {
        name: "occlusion",
        ty: ValueType::F32,
        doc: "Ambient occlusion multiplier, 1 = unoccluded.",
        editable_default: Some(Value::F32(1.0)),
    },
    SurfaceField {
        name: "alpha",
        ty: ValueType::F32,
        doc: "Opacity. The deferred path ignores it (the G-buffer is opaque).",
        editable_default: Some(Value::F32(1.0)),
    },
];

/// Registry id of the surface output node.
pub const SURFACE_OUTPUT_ID: &str = "output.surface";
/// Registry id of the vertex output node.
pub const VERTEX_OUTPUT_ID: &str = "output.vertex";
/// Registry id of the discard output node.
pub const DISCARD_OUTPUT_ID: &str = "output.discard";
/// Definition id of the node that writes one declared interpolant
/// ([`varying_output_def`]).
pub const VARYING_OUTPUT_ID: &str = "output.varying";
/// The input socket [`VERTEX_OUTPUT_ID`] takes its offset on.
pub const SOCKET_POSITION_OFFSET: &str = "position_offset";
/// The input socket [`DISCARD_OUTPUT_ID`] takes its condition on.
pub const SOCKET_DISCARD: &str = "discard";
/// Input socket of [`VARYING_OUTPUT_ID`].
pub const SOCKET_VARYING: &str = "value";

/// Registry id of the context-read node for `field`.
pub fn context_node_id(field: &str) -> String {
    format!("input.{field}")
}

/// The terminal node definition, generated from [`SURFACE_FIELDS`].
///
/// Every input is optional: unconnected fields keep the value
/// [`DEFAULT_SURFACE_FN`] provided, so a graph that only drives `base_color`
/// is still a complete material.
pub fn surface_output_def() -> NodeDefinition {
    let mut builder = NodeDefinition::builder(SURFACE_OUTPUT_ID, "Surface output")
        .category("output")
        .doc(
            "The material a graph produces. Unconnected inputs keep their \
             default: the geometric normal for `normal`, the value shown in \
             the editor for the rest.",
        );
    for field in SURFACE_FIELDS {
        let mut socket = Socket::new(field.name, field.ty)
            .with_doc(field.doc)
            .optional();
        if let Some(default) = field.editable_default {
            socket = socket.with_default(default);
        }
        builder = builder.input(socket);
    }
    builder.surface_output()
}

/// One node definition per [`CONTEXT_FIELDS`] entry.
///
/// Usable in either shader stage, because every field is in both context
/// structs under the same name.
pub fn context_node_defs() -> Vec<NodeDefinition> {
    CONTEXT_FIELDS
        .iter()
        .map(|field| {
            NodeDefinition::builder(context_node_id(field.name), field.label)
                .category("input")
                .doc(field.doc)
                .output(Socket::new("out", field.ty).with_doc(field.doc))
                .context_read(field.name)
        })
        .collect()
}

/// One node definition per [`VERTEX_ONLY_FIELDS`] entry.
pub fn vertex_context_node_defs() -> Vec<NodeDefinition> {
    VERTEX_ONLY_FIELDS
        .iter()
        .map(|field| {
            NodeDefinition::builder(context_node_id(field.name), field.label)
                .category("input")
                .doc(field.doc)
                .output(Socket::new("out", field.ty).with_doc(field.doc))
                .vertex_context_read(field.name)
        })
        .collect()
}

/// The vertex-stage terminal node: an object-space position offset.
///
/// Object space, not world: it is added to the vertex before the model
/// transform, so a displaced object still follows its own transform, and
/// a mesh's own normal is the axis a displacement along the surface
/// wants. A graph with no such node compiles a vertex stage identical to
/// the one it always had.
pub fn vertex_output_def() -> NodeDefinition {
    NodeDefinition::builder(VERTEX_OUTPUT_ID, "Vertex output")
        .category("output")
        .doc(
            "Moves the vertex, in object space, before it is transformed. \
             The one place a material graph runs in the vertex stage — so \
             everything feeding it is compiled there too, including into a \
             shadow pass, which is what makes a displaced object cast a \
             displaced shadow.",
        )
        .input(
            Socket::new(SOCKET_POSITION_OFFSET, ValueType::Vec3)
                .with_doc("Object-space offset added to the vertex position.")
                .with_default(Value::Vec3([0.0, 0.0, 0.0]))
                .optional(),
        )
        .vertex_output()
}

/// The terminal that writes one declared interpolant.
///
/// The vertex half of an `AttributeFrequency::Computed` attribute: the
/// graph declares the name and type once, this node says what the vertex
/// stage puts in it, and `input.attribute` reads it back on the fragment
/// side — the same node that reads a stream the mesh carries, because
/// from the reader's side there is no difference worth a second node.
pub fn varying_output_def() -> NodeDefinition {
    NodeDefinition::builder(VARYING_OUTPUT_ID, "Interpolant output")
        .category("output")
        .doc(
            "Computes one declared interpolant in the vertex stage, to be \
             read per fragment. The graph declares the name and the type; \
             this says what goes in it. Each interpolant costs one of the \
             16 inter-stage locations, and the graph is told when it runs \
             out.",
        )
        .setting(
            SettingDef::new(
                crate::node::SETTING_NAME,
                "name",
                "Which declared interpolant this writes.",
            )
            .with_default("value"),
        )
        .generic_param(GenericParam::new("T", INTERPOLANT_TYPES.to_vec()))
        .input(
            Socket::new(SOCKET_VARYING, ValueType::F32)
                .with_doc("What the vertex stage computes for this interpolant.")
                .generic("T")
                // Unfed while its branch is being built, like every other
                // terminal's input: it writes its type's zero until
                // something is wired in.
                .optional(),
        )
        .varying_output()
}

/// The types an inter-stage location can carry.
///
/// The same four a per-vertex attribute may be, and for the neighbouring
/// reason: a `@location` is interpolated, and a matrix has no
/// interpolation and no single location to live at.
pub const INTERPOLANT_TYPES: &[ValueType] = &[
    ValueType::F32,
    ValueType::Vec2,
    ValueType::Vec3,
    ValueType::Vec4,
];

/// The fragment-stage terminal node: throw this fragment away.
///
/// Separate from [`SURFACE_FIELDS`]' `alpha` and not a substitute for it:
/// `alpha` is a blend weight the deferred path cannot honour, while this
/// removes the fragment before anything is written — including depth,
/// which is what lets a perforated material cast a perforated shadow.
pub fn discard_output_def() -> NodeDefinition {
    NodeDefinition::builder(DISCARD_OUTPUT_ID, "Discard")
        .category("output")
        .doc(
            "Throws the fragment away when true, writing neither colour \
             nor depth. This is the only part of a material a depth or \
             shadow pass compiles, so keep what feeds it cheap.",
        )
        .input(
            Socket::new(SOCKET_DISCARD, ValueType::Bool)
                .with_doc("Discard the fragment when true.")
                .with_default(Value::Bool(false))
                .optional(),
        )
        .discard_output()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_output_exposes_every_surface_field() {
        let def = surface_output_def();
        assert!(def.is_surface_output());
        assert_eq!(def.inputs.len(), SURFACE_FIELDS.len());
        for field in SURFACE_FIELDS {
            let socket = def.input(field.name).expect("field is an input");
            assert_eq!(socket.ty, field.ty);
            assert!(socket.optional);
        }
    }

    #[test]
    fn the_ui_instance_layout_is_dense_and_ordered() {
        // The table's index is the `@location`, which is what lets the
        // shader's declaration line up with the host's `#[repr(C)]` struct.
        assert!(!UI_ATTRIBUTES.is_empty());
        assert_eq!(UI_ATTRIBUTES.last().expect("attributes").name, "kind");
        assert_eq!(UI_QUAD_VERTICES, 6, "two triangles");
    }

    #[test]
    fn the_ui_kind_table_agrees_with_its_constants() {
        // The table is what `ui.wxsl` is generated against by hand and what
        // the Rust enum mirrors; the constants are what code reads. A
        // mismatch would be a UI whose primitives shade as the wrong kind.
        for (index, kind) in UI_KINDS.iter().enumerate() {
            assert_eq!(
                kind.value as usize, index,
                "`{}` is out of order",
                kind.name
            );
        }
        assert_eq!(UI_KINDS[UI_KIND_SHAPE as usize].name, "UI_KIND_SHAPE");
        assert_eq!(UI_KINDS[UI_KIND_TEXTURE as usize].name, "UI_KIND_TEXTURE");
        assert_eq!(UI_KINDS[UI_KIND_TEXT as usize].name, "UI_KIND_TEXT");
    }

    #[test]
    fn the_ui_pass_binds_only_the_pass_group() {
        // Not the frame group: the UI pass has no camera and no lights, and
        // group 0 must not mean two different layouts in two passes.
        let bindings = [BINDING_UI_VIEWPORT, BINDING_UI_TEXTURE, BINDING_UI_SAMPLER];
        let mut sorted = bindings;
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2], "the UI bindings are dense from zero");
        assert!(BIND_GROUPS
            .iter()
            .any(|slot| slot.index == GROUP_PASS && !slot.application_owned));
    }

    #[test]
    fn every_stage_constant_points_at_its_own_row() {
        // The constants are indices into the table, so a row inserted in
        // the middle without moving them would silently rename a stage.
        assert_eq!(MaterialStage::ALL.len(), MATERIAL_STAGES.len());
        for (index, stage) in MaterialStage::ALL.iter().enumerate() {
            assert_eq!(stage.index(), index);
            assert_eq!(MaterialStage::from_index(index), Some(*stage));
        }
        assert_eq!(MaterialStage::FORWARD_LIT.name(), "forward_lit");
        assert_eq!(MaterialStage::GBUFFER.name(), "gbuffer");
        assert_eq!(MaterialStage::DEPTH_ONLY.name(), "depth_only");
        assert_eq!(MaterialStage::from_index(MATERIAL_STAGES.len()), None);
    }

    #[test]
    fn stage_names_round_trip_and_are_unique() {
        for stage in MaterialStage::ALL {
            assert_eq!(MaterialStage::parse(stage.name()), Some(*stage));
        }
        assert_eq!(
            MaterialStage::parse("  GBuffer "),
            Some(MaterialStage::GBUFFER)
        );
        assert_eq!(MaterialStage::parse("velocity"), None);
        let mut names: Vec<&str> = MATERIAL_STAGES.iter().map(|stage| stage.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), MATERIAL_STAGES.len());
    }

    #[test]
    fn a_stages_target_count_follows_from_what_it_returns() {
        // This is what the render graph validates a pass against, so a
        // stage that returns three targets and a pass that attaches one is
        // a named error rather than an entry-point signature complaint.
        assert_eq!(MaterialStage::FORWARD_LIT.color_targets(), 1);
        assert_eq!(
            MaterialStage::GBUFFER.color_targets(),
            GBUFFER_TARGETS.len()
        );
        assert_eq!(MaterialStage::DEPTH_ONLY.color_targets(), 0);
        assert_eq!(MaterialStage::SHADOW.color_targets(), 0);
        // A stage that needs no surface writes no colour, and the
        // converse: those are the same stages, and it is what
        // partitioning turns on.
        for stage in MaterialStage::ALL {
            assert_eq!(stage.needs_surface(), stage.color_targets() > 0);
            assert_eq!(
                stage.needs_surface(),
                stage.output() != StageOutput::Nothing
            );
        }
        // Every stage names a fragment entry; whether one is *emitted*
        // is a property of the material (ADR 0025).
        let mut entries: Vec<&str> = MATERIAL_STAGES
            .iter()
            .map(|stage| stage.fragment_entry)
            .collect();
        entries.sort_unstable();
        entries.dedup();
        assert_eq!(entries.len(), MATERIAL_STAGES.len());
    }

    #[test]
    fn context_nodes_cover_every_context_field() {
        let defs = context_node_defs();
        assert_eq!(defs.len(), CONTEXT_FIELDS.len());
        for (def, field) in defs.iter().zip(CONTEXT_FIELDS) {
            assert_eq!(def.id, context_node_id(field.name));
            assert_eq!(def.outputs[0].ty, field.ty);
        }
    }
}
