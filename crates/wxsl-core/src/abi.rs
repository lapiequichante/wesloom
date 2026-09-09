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
use crate::node::{NodeDefinition, Socket, Value, ValueType};

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

/// Forward path: shades a surface to a final `vec4f` colour.
pub const SHADE_SURFACE_FN: &str = "shade_surface";
/// Deferred path: the G-buffer fragment output struct.
pub const GBUFFER_STRUCT: &str = "GBuffer";
/// Deferred path: packs a surface into the G-buffer.
pub const PACK_GBUFFER_FN: &str = "pack_gbuffer";

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
        doc: "Camera, scene lighting and object transforms. Built once per frame.",
        application_owned: false,
    },
    BindGroup {
        index: GROUP_MATERIAL,
        name: "material",
        doc: "A material's parameters, textures and samplers.",
        application_owned: false,
    },
    BindGroup {
        index: GROUP_USER,
        name: "user",
        doc: "Unused by wxsl: the slot an application binds its own resources in.",
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

/// Per-frame data: camera, scene, and the object transforms
/// (`package::wxsl::bindings`). Rebound once per frame.
pub const GROUP_FRAME: u32 = 0;
/// Per-material data: the parameters a graph exposes, plus its textures.
pub const GROUP_MATERIAL: u32 = 1;
/// The application's own slot. Nothing in wxsl binds here, and it is the
/// only group a node definition may declare a binding in.
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
/// [`GROUP_FRAME`] binding of the per-object transform uniform.
pub const BINDING_OBJECT: u32 = 2;

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
/// whatever the graph pins. [`FEATURE_DEFERRED`] is deliberately absent: the
/// render path is chosen by the pipeline, not by a macro the user edits.
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
    ]
}

/// Feature flag: tonemap the shaded colour (see [`abi_macros`]).
pub const FEATURE_TONEMAP: &str = "wxsl_tonemap";
/// Feature flag: output normals instead of shading (see [`abi_macros`]).
pub const FEATURE_DEBUG_NORMALS: &str = "wxsl_debug_normals";

/// The WXSL conditional-translation feature that selects the deferred path.
///
/// Bound by `wxsl-render` from the pipeline's `RenderPath`, never stored on
/// a graph — a material is written once and compiled per path
/// ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).
pub const FEATURE_DEFERRED: &str = "wxsl_deferred";

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
/// Name of the generated vertex entry point.
pub const VERTEX_ENTRY: &str = "vs_main";
/// Name of the generated fragment entry point (both paths).
pub const FRAGMENT_ENTRY: &str = "fs_main";

/// One field of [`CONTEXT_STRUCT`], and the node that reads it.
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
    fn context_nodes_cover_every_context_field() {
        let defs = context_node_defs();
        assert_eq!(defs.len(), CONTEXT_FIELDS.len());
        for (def, field) in defs.iter().zip(CONTEXT_FIELDS) {
            assert_eq!(def.id, context_node_id(field.name));
            assert_eq!(def.outputs[0].ty, field.ty);
        }
    }
}
