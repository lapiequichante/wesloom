//! Fixed host layouts and their Rust/C/WXSL views (ADR 0050).

use std::fmt::Write as _;

use crate::abi;
use crate::node::ValueType;

/// A field type in a fixed host buffer.
#[derive(Clone, Copy, Debug)]
pub enum Type {
    /// 32-bit float.
    F32,
    /// 32-bit signed integer.
    I32,
    /// 32-bit unsigned integer.
    U32,
    /// Float vector (two, three or four components).
    Vector(usize),
    /// Unsigned integer vector.
    UVector(usize),
    /// Column-major four-by-four matrix.
    Matrix,
    /// Fixed array of another declared struct.
    Array(&'static str, usize),
}

/// One declared field; padding names preserve the existing Rust API.
#[derive(Clone, Debug)]
pub struct Field {
    /// Host spelling.
    pub name: &'static str,
    /// Shader spelling.
    pub shader_name: &'static str,
    /// Storage type.
    pub ty: Type,
}

/// One fixed layout, or a tightly packed vertex stream.
#[derive(Clone, Debug)]
pub struct Structure {
    /// Rust type (C uses the same name prefixed by Wxsl).
    pub name: &'static str,
    /// WXSL struct name.
    pub shader_name: &'static str,
    /// True for vertex attributes, whose packing is not uniform layout.
    pub vertex: bool,
    /// Storage structs do not round their overall alignment up to 16.
    pub storage: bool,
    /// Fields in declaration order.
    pub fields: Vec<Field>,
}

fn field(name: &'static str, shader_name: &'static str, ty: Type) -> Field {
    Field {
        name,
        shader_name,
        ty,
    }
}

fn value_type(ty: ValueType) -> Type {
    match ty {
        ValueType::F32 => Type::F32,
        ValueType::U32 => Type::U32,
        ValueType::Vec2 => Type::Vector(2),
        ValueType::Vec3 => Type::Vector(3),
        ValueType::Vec4 => Type::Vector(4),
        ValueType::Mat4 => Type::Matrix,
        _ => panic!("unsupported fixed vertex type {ty}"),
    }
}

/// The single declaration of the fixed frame fields; vertex/UI reuse ABI tables.
pub fn structures() -> Vec<Structure> {
    use Type::*;
    vec![
        Structure {
            name: "CameraUniform",
            shader_name: "Camera",
            vertex: false,
            storage: false,
            fields: vec![
                field("view_proj", "view_proj", Matrix),
                field("inverse_view_proj", "inverse_view_proj", Matrix),
                field("position", "position", Vector(3)),
                field("_padding", "_pad0", F32),
                field("previous_view_proj", "previous_view_proj", Matrix),
                field("previous_position", "previous_position", Vector(3)),
                field("_padding1", "_pad1", F32),
            ],
        },
        Structure {
            name: "LightUniform",
            shader_name: "Light",
            vertex: false,
            storage: false,
            fields: vec![
                field("position_or_direction", "position_or_direction", Vector(3)),
                field("kind", "kind", F32),
                field("color", "color", Vector(3)),
                field("intensity", "intensity", F32),
                field("shadow_view_proj", "shadow_view_proj", Matrix),
                field("shadow_slice", "shadow_slice", I32),
                field("shadow_normal_bias", "shadow_normal_bias", F32),
                field("_padding", "_pad0", Vector(2)),
            ],
        },
        Structure {
            name: "SceneUniform",
            shader_name: "Scene",
            vertex: false,
            storage: false,
            fields: vec![
                field("lights", "lights", Array("LightUniform", abi::MAX_LIGHTS)),
                field("ambient_sky", "ambient_sky", Vector(3)),
                field("environment_enabled", "environment_enabled", F32),
                field("ambient_ground", "ambient_ground", Vector(3)),
                field("environment_scale", "environment_scale", F32),
                field("light_count", "light_count", U32),
                field("time", "time", F32),
                field("exposure", "exposure", F32),
                field("previous_time", "previous_time", F32),
            ],
        },
        Structure {
            name: "InstanceTransform",
            shader_name: "Instance",
            vertex: false,
            storage: true,
            fields: abi::INSTANCE_BASE_FIELDS
                .iter()
                .map(|entry| field(entry.name, entry.name, value_type(entry.ty)))
                .collect(),
        },
        Structure {
            name: "Vertex",
            shader_name: "VertexIn",
            vertex: true,
            storage: false,
            fields: abi::VERTEX_IN_FIELDS
                .iter()
                .map(|entry| field(entry.name, entry.name, value_type(entry.ty)))
                .collect(),
        },
        Structure {
            name: "UiInstance",
            shader_name: "UiInstanceIn",
            vertex: true,
            storage: false,
            fields: abi::UI_ATTRIBUTES
                .iter()
                .map(|entry| {
                    field(
                        entry.name,
                        entry.name,
                        match entry.ty {
                            "vec2f" => Vector(2),
                            "vec4f" => Vector(4),
                            "u32" => U32,
                            _ => panic!("unsupported UI attribute type {}", entry.ty),
                        },
                    )
                })
                .collect(),
        },
        Structure {
            name: "GpuEdge",
            shader_name: "MsdfEdge",
            vertex: false,
            storage: true,
            fields: vec![
                field("p0", "p0", Vector(2)),
                field("p1", "p1", Vector(2)),
                field("p2", "p2", Vector(2)),
                field("p3", "p3", Vector(2)),
                field("kind", "kind", U32),
                field("color", "color", U32),
            ],
        },
        Structure {
            name: "GpuJob",
            shader_name: "MsdfJob",
            vertex: false,
            storage: true,
            fields: vec![
                field("translate", "translate", Vector(2)),
                field("scale", "scale", F32),
                field("range", "range", F32),
                field("size", "size", UVector(2)),
                field("edge_begin", "edge_begin", U32),
                field("edge_end", "edge_end", U32),
                field("pixel_offset", "pixel_offset", U32),
                field("_pad0", "_pad0", U32),
            ],
        },
        Structure {
            name: "UiViewport",
            shader_name: "UiViewport",
            vertex: false,
            storage: false,
            fields: vec![
                field("size", "size", Vector(2)),
                field("_pad0", "_pad0", Vector(2)),
            ],
        },
    ]
}

impl Type {
    fn shape(self) -> (usize, usize) {
        match self {
            Self::F32 | Self::I32 | Self::U32 => (4, 4),
            Self::Vector(2) | Self::UVector(2) => (8, 8),
            Self::Vector(3) | Self::UVector(3) => (16, 12),
            Self::Vector(4) | Self::UVector(4) => (16, 16),
            Self::Matrix => (16, 64),
            Self::Array(name, count) => {
                let structure = structures()
                    .into_iter()
                    .find(|item| item.name == name)
                    .expect("declared nested layout");
                (16, structure.size() * count)
            }
            Self::Vector(_) | Self::UVector(_) => {
                panic!("a host vector has two to four components")
            }
        }
    }

    fn rust(self) -> String {
        match self {
            Self::F32 => "f32".into(),
            Self::I32 => "i32".into(),
            Self::U32 => "u32".into(),
            Self::Vector(n) => format!("[f32; {n}]"),
            Self::UVector(n) => format!("[u32; {n}]"),
            Self::Matrix => "[[f32; 4]; 4]".into(),
            Self::Array(name, n) => format!("[{name}; {n}]"),
        }
    }

    fn c(self, name: &str) -> String {
        match self {
            Self::F32 => format!("float {name}"),
            Self::I32 => format!("int32_t {name}"),
            Self::U32 => format!("uint32_t {name}"),
            Self::Vector(n) => format!("float {name}[{n}]"),
            Self::UVector(n) => format!("uint32_t {name}[{n}]"),
            Self::Matrix => format!("float {name}[4][4]"),
            Self::Array(ty, n) => format!("Wxsl{ty} {name}[{n}]"),
        }
    }

    fn wxsl(self) -> String {
        match self {
            Self::F32 => "f32".into(),
            Self::I32 => "i32".into(),
            Self::U32 => "u32".into(),
            Self::Vector(n) => format!("vec{n}f"),
            Self::UVector(n) => format!("vec{n}u"),
            Self::Matrix => "mat4x4f".into(),
            Self::Array(ty, n) => format!(
                "array<{}, {n}>",
                structures()
                    .iter()
                    .find(|s| s.name == ty)
                    .expect("nested layout")
                    .shader_name
            ),
        }
    }
}

impl Structure {
    /// Computed member offsets, preserving order (not BufferLayout's name sorting).
    pub fn offsets(&self) -> Vec<usize> {
        let mut end: usize = 0;
        self.fields
            .iter()
            .map(|field| {
                let (align, size) = field.ty.shape();
                let align = if self.vertex { 4 } else { align };
                end = end.div_ceil(align) * align;
                let offset = end;
                end += size;
                offset
            })
            .collect()
    }

    /// Complete byte stride, including tail padding.
    pub fn size(&self) -> usize {
        let end = self.offsets().last().copied().unwrap_or(0)
            + self.fields.last().map_or(0, |field| field.ty.shape().1);
        let align = if self.vertex {
            4
        } else {
            self.fields
                .iter()
                .map(|field| field.ty.shape().0)
                .max()
                .unwrap_or(1)
                .max(if self.storage { 1 } else { 16 })
        };
        end.div_ceil(align) * align
    }

    /// Native adapter's attribute format at a vertex field.
    pub fn vertex_format(&self, index: usize) -> &'static str {
        assert!(self.vertex);
        match self.fields[index].ty {
            Type::Vector(2) => "Float32x2",
            Type::Vector(3) => "Float32x3",
            Type::Vector(4) => "Float32x4",
            Type::U32 => "Uint32",
            Type::F32 => "Float32",
            _ => panic!("unsupported vertex format"),
        }
    }
}

/// Generate Rust host definitions, with explicit generated padding and const layout checks.
pub fn rust_source(names: &[&str]) -> String {
    let mut out = String::from("// Generated from wxsl_core::host; do not edit.\n");
    for structure in structures().iter().filter(|s| names.contains(&s.name)) {
        writeln!(out, "/// Generated host layout for `{}` (ADR 0050).\n#[repr(C)]\n#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]\npub struct {} {{", structure.shader_name, structure.name).unwrap();
        let mut end = 0;
        for (index, (field, offset)) in structure.fields.iter().zip(structure.offsets()).enumerate()
        {
            if offset > end {
                writeln!(out, "    _host_padding{index}: [u8; {}],", offset - end).unwrap();
            }
            writeln!(
                out,
                "    /// `{}` in the shared layout.\n    {}{}: {},",
                field.shader_name,
                if field.name.starts_with('_') {
                    ""
                } else {
                    "pub "
                },
                field.name,
                field.ty.rust()
            )
            .unwrap();
            end = offset + field.ty.shape().1;
        }
        if structure.size() > end {
            writeln!(
                out,
                "    _host_tail_padding: [u8; {}],",
                structure.size() - end
            )
            .unwrap();
        }
        out.push_str("}\n");
        if structure.name != "InstanceTransform" {
            writeln!(
                out,
                "impl Default for {} {{ fn default() -> Self {{ bytemuck::Zeroable::zeroed() }} }}",
                structure.name
            )
            .unwrap();
        }
        writeln!(
            out,
            "const _: () = {{\n    assert!(core::mem::size_of::<{}>() == {});",
            structure.name,
            structure.size()
        )
        .unwrap();
        for (field, offset) in structure.fields.iter().zip(structure.offsets()) {
            writeln!(
                out,
                "    assert!(core::mem::offset_of!({}, {}) == {offset});",
                structure.name, field.name
            )
            .unwrap();
        }
        out.push_str("};\n");
    }
    out
}

/// Generate C and C++ host definitions and compiler-checked offsets.
pub fn c_header() -> String {
    let mut out = String::from("/* Generated from wxsl_core::host; do not edit. */\n#ifndef WXSL_HOST_H\n#define WXSL_HOST_H\n#include <stdint.h>\n#include <stddef.h>\n#ifdef __cplusplus\n#define WXSL_LAYOUT_ASSERT static_assert\n#else\n#define WXSL_LAYOUT_ASSERT _Static_assert\n#endif\n");
    for structure in structures() {
        writeln!(out, "typedef struct Wxsl{} {{", structure.name).unwrap();
        let mut end = 0;
        for (index, (field, offset)) in structure.fields.iter().zip(structure.offsets()).enumerate()
        {
            if offset > end {
                writeln!(out, "    uint8_t _host_padding{index}[{}];", offset - end).unwrap();
            }
            writeln!(out, "    {};", field.ty.c(field.name)).unwrap();
            end = offset + field.ty.shape().1;
        }
        if structure.size() > end {
            writeln!(
                out,
                "    uint8_t _host_tail_padding[{}];",
                structure.size() - end
            )
            .unwrap();
        }
        writeln!(
            out,
            "}} Wxsl{};\nWXSL_LAYOUT_ASSERT(sizeof(Wxsl{}) == {}, \"{} size\");",
            structure.name,
            structure.name,
            structure.size(),
            structure.name
        )
        .unwrap();
        for (field, offset) in structure.fields.iter().zip(structure.offsets()) {
            writeln!(
                out,
                "WXSL_LAYOUT_ASSERT(offsetof(Wxsl{}, {}) == {offset}, \"{}.{} offset\");",
                structure.name, field.name, structure.name, field.name
            )
            .unwrap();
        }
    }
    for (name, value) in [
        ("SHADER_ABI_REVISION", abi::REVISION),
        ("GROUP_FRAME", abi::GROUP_FRAME),
        ("GROUP_MATERIAL", abi::GROUP_MATERIAL),
        ("GROUP_USER", abi::GROUP_USER),
        ("GROUP_PASS", abi::GROUP_PASS),
        ("BINDING_CAMERA", abi::BINDING_CAMERA),
        ("BINDING_SCENE", abi::BINDING_SCENE),
        ("BINDING_INSTANCES", abi::BINDING_INSTANCES),
        (
            "BINDING_INSTANCE_ATTRIBUTES",
            abi::BINDING_INSTANCE_ATTRIBUTES,
        ),
        ("BINDING_SHADOW_MAPS", abi::BINDING_SHADOW_MAPS),
        ("BINDING_SHADOW_SAMPLER", abi::BINDING_SHADOW_SAMPLER),
        ("BINDING_ENVIRONMENT_LUT", abi::BINDING_ENVIRONMENT_LUT),
        (
            "BINDING_ENVIRONMENT_DIFFUSE",
            abi::BINDING_ENVIRONMENT_DIFFUSE,
        ),
        (
            "BINDING_ENVIRONMENT_SPECULAR",
            abi::BINDING_ENVIRONMENT_SPECULAR,
        ),
        (
            "BINDING_ENVIRONMENT_SAMPLER",
            abi::BINDING_ENVIRONMENT_SAMPLER,
        ),
        (
            "BINDING_PREVIOUS_INSTANCES",
            abi::BINDING_PREVIOUS_INSTANCES,
        ),
    ] {
        writeln!(out, "#define WXSL_{name} {value}u").unwrap();
    }
    writeln!(out, "#define WXSL_HOST_LAYOUT_ID \"{}\"", schema_id()).unwrap();
    out.push_str("#undef WXSL_LAYOUT_ASSERT\n#endif\n");
    out
}

/// Semantic identity of fixed host fields, their computed offsets and sizes.
pub fn schema_id() -> u64 {
    let mut signature = format!("{:?}", structures());
    for structure in structures() {
        write!(signature, "{:?}:{};", structure.offsets(), structure.size()).unwrap();
    }
    write!(
        signature,
        "{:?}",
        [
            abi::BINDING_CAMERA,
            abi::BINDING_SCENE,
            abi::BINDING_INSTANCES,
            abi::BINDING_INSTANCE_ATTRIBUTES,
            abi::BINDING_SHADOW_MAPS,
            abi::BINDING_SHADOW_SAMPLER,
            abi::BINDING_ENVIRONMENT_LUT,
            abi::BINDING_ENVIRONMENT_SAMPLER,
            abi::BINDING_PREVIOUS_INSTANCES,
            abi::BINDING_ENVIRONMENT_DIFFUSE,
            abi::BINDING_ENVIRONMENT_SPECULAR
        ]
    )
    .unwrap();
    crate::wxsl::stable_hash(signature.as_bytes())
}

/// Generate shader declarations (vertex locations come from the ABI table).
pub fn shader_source(names: &[&str]) -> String {
    let mut out = String::from("// Generated from wxsl_core::host; do not edit.\n");
    if names.contains(&"SceneUniform") {
        writeln!(out, "const WXSL_MAX_LIGHTS: u32 = {}u;", abi::MAX_LIGHTS).unwrap();
    }
    for structure in structures().iter().filter(|s| names.contains(&s.name)) {
        writeln!(out, "struct {} {{", structure.shader_name).unwrap();
        for (index, field) in structure.fields.iter().enumerate() {
            writeln!(
                out,
                "    {}{}: {},",
                if structure.vertex {
                    format!("@location({index}) ")
                } else {
                    String::new()
                },
                field.shader_name,
                field.ty.wxsl()
            )
            .unwrap();
        }
        if structure.name == "Vertex" {
            out.push_str("    @builtin(instance_index) instance: u32,\n");
        }
        out.push_str("}\n");
    }
    out
}

/// Expand the fixed-layout marker in a shipped ABI shader template.
pub fn shader_template(name: &str, source: &str) -> String {
    let names: &[&str] = match name {
        "bindings" => &[
            "CameraUniform",
            "LightUniform",
            "SceneUniform",
            "InstanceTransform",
        ],
        "vertex" => &["Vertex"],
        "ui" => &["UiInstance", "UiViewport"],
        "msdf" => &["GpuEdge", "GpuJob"],
        _ => return source.to_string(),
    };
    crate::template::fill(source, &[("HOST_LAYOUTS", &shader_source(names))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_layouts_keep_their_sizes_and_packing() {
        let all = structures();
        for (name, size) in [
            ("CameraUniform", 224),
            ("LightUniform", 112),
            ("SceneUniform", 496),
            ("InstanceTransform", 128),
            ("Vertex", 48),
            ("UiInstance", 68),
            ("GpuEdge", 40),
            ("GpuJob", 40),
            ("UiViewport", 16),
        ] {
            assert_eq!(
                all.iter().find(|s| s.name == name).unwrap().size(),
                size,
                "{name}"
            );
        }
        assert_eq!(all[0].offsets(), vec![0, 64, 128, 140, 144, 208, 220]);
        assert_eq!(all[4].offsets(), vec![0, 12, 24, 40]);
    }
}
