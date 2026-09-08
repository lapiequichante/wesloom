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
    fn context_nodes_cover_every_context_field() {
        let defs = context_node_defs();
        assert_eq!(defs.len(), CONTEXT_FIELDS.len());
        for (def, field) in defs.iter().zip(CONTEXT_FIELDS) {
            assert_eq!(def.id, context_node_id(field.name));
            assert_eq!(def.outputs[0].ty, field.ty);
        }
    }
}
