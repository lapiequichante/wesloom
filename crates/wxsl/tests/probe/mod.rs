//! The shared GPU harness, and the probe graph that checks a computed
//! buffer layout against the hardware.
//!
//! Not a test binary — Cargo only compiles top-level files in `tests/` as
//! those — but the code `material_resources.rs` (ADR 0023) and
//! `material_geometry.rs` (ADR 0024) both need. The probe is the reason it
//! is shared: the two computed layouts in this repo are the only
//! host-shared layouts with no `#[repr(C)]` mirror to be checked against,
//! so they get by test what the mirrors get by construction, and it should
//! be the *same* test.
//!
//! # Why the scene is unlit
//!
//! Every graph here drives `emissive` with no lights and no ambient, and
//! turns the tonemap off. What lands in the framebuffer is then
//! `srgb(emissive)` and nothing else, so a pixel is a readable answer
//! rather than a lighting result to eyeball.
#![allow(dead_code)]

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node, NodeId};
use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::core::node::{NodeRegistry, Value, ValueType};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::Material;
use wxsl::render::wgpu;
use wxsl::render::{
    Camera, DrawItem, Environment, MaterialBindings, RenderRequest, Renderer, TargetConfig,
};

/// The offscreen target's edge, in pixels.
pub const SIZE: u32 = 64;

/// How the probe's two values get into a graph.
///
/// Adds a node declaring `name` as `ty` with starting value `value`, and
/// answers the node whose `out` socket carries it.
pub type Declare<'a> = &'a dyn Fn(&mut Graph, &NodeRegistry, &str, ValueType, Value) -> NodeId;

/// The probe declared as a uniform parameter (ADR 0023).
pub fn declare_as_parameter(
    graph: &mut Graph,
    registry: &NodeRegistry,
    name: &str,
    ty: ValueType,
    value: Value,
) -> NodeId {
    let node = graph.add(
        Node::new("param.value")
            .with_setting("name", name)
            .with_param("value", value),
    );
    graph
        .set_generic(registry, node, "T", ty)
        .expect("every value type is allowed");
    graph.set_param(node, "value", value);
    node
}

/// A GPU context, or `None` when this machine has no usable adapter.
pub fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(context) => Some(context),
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

/// No lights, no ambient: the surface's emissive is the whole image.
pub fn unlit() -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 0.0, 3.0),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: Vec::new(),
        ambient_sky: Vec3::ZERO,
        ambient_ground: Vec3::ZERO,
        exposure: 1.0,
        time: 0.0,
        previous_time: 0.0,
    }
}

pub fn no_tonemap() -> MacroSet {
    let mut macros = MacroSet::new();
    macros.set(abi::FEATURE_TONEMAP, MacroValue::Flag(false));
    macros
}

/// Everything one of these tests needs on the GPU, once.
pub struct Harness {
    pub gpu: GpuContext,
    pub target: OffscreenTarget,
    pub renderer: Renderer,
    /// A plane standing up to face the camera, for a test that only wants
    /// one flat surface to sample.
    pub mesh: wxsl::render::Mesh,
    pub registry: NodeRegistry,
}

impl Harness {
    pub fn new(gpu: GpuContext) -> Self {
        let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
        let renderer = Renderer::new(
            &gpu.device,
            wxsl::stdlib_library(),
            TargetConfig::new(SIZE, SIZE, target.format()),
        )
        .expect("the stdlib library satisfies the ABI");
        // A plane facing the camera: every pixel of it is the same
        // surface, so one sample answers for the whole material.
        let mesh = wxsl::render::Mesh::plane(&gpu.device, 2.0);
        Harness {
            gpu,
            target,
            renderer,
            mesh,
            registry: wxsl::stdlib::registry(),
        }
    }

    pub fn material(&self, graph: &Graph) -> Material {
        Material::from_graph_with_macros(graph, &self.registry, &no_tonemap())
            .expect("the graph compiles")
    }

    pub fn bindings(&mut self, material: &Material) -> MaterialBindings {
        self.renderer.material_bindings(&self.gpu.device, material)
    }

    /// Draw one plane with this material and read the centre pixel.
    pub fn shade(
        &mut self,
        material: &Material,
        bindings: Option<&MaterialBindings>,
        user: Option<&wgpu::BindGroup>,
    ) -> [u8; 4] {
        // Lying flat by default, so stand it up to face the camera.
        let mut item = DrawItem::new(&self.mesh, material)
            .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2));
        if let Some(bindings) = bindings {
            item = item.with_bindings(bindings);
        }
        if let Some(user) = user {
            item = item.with_user(user);
        }
        let draws = wxsl::render::single_draw(item);
        let image = render_list(&self.gpu, &mut self.renderer, &self.target, &draws)
            .expect("the frame renders");
        pixel(&image, SIZE / 2, SIZE / 2)
    }
}

/// Render `draws` into `target` and read it back as RGBA8.
///
/// A free function rather than a method so a test can build a draw list
/// over meshes of its own — which the geometry tests do, because the
/// whole point of a declared attribute is that the mesh carries it.
pub fn render_list(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    target: &OffscreenTarget,
    draws: &wxsl::render::DrawList<'_>,
) -> Result<Vec<u8>, wxsl::render::RenderError> {
    render_list_in(gpu, renderer, target, draws, &unlit())
}

/// [`render_list`], in an environment of the caller's choosing.
///
/// What a test that is about the *environment* rather than the material
/// needs: the frame clock, where the eye is, what is casting.
pub fn render_list_in(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    target: &OffscreenTarget,
    draws: &wxsl::render::DrawList<'_>,
    environment: &Environment,
) -> Result<Vec<u8>, wxsl::render::RenderError> {
    renderer.render(
        &gpu.device,
        &gpu.queue,
        &RenderRequest {
            view: target.view(),
            environment,
            draws,
        },
    )?;
    gpu.wait();
    Ok(target.read_rgba8(&gpu.device, &gpu.queue))
}

/// One pixel of a [`render_list`] image.
pub fn pixel(image: &[u8], x: u32, y: u32) -> [u8; 4] {
    let index = ((y * SIZE + x) * 4) as usize;
    image[index..index + 4].try_into().expect("in bounds")
}

/// `srgb(value)` as the shader's `linear_to_srgb` computes it, so a test
/// can say what colour it expects in linear terms.
pub fn srgb(value: f32) -> u8 {
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub fn close(actual: u8, expected: u8) -> bool {
    actual.abs_diff(expected) <= 3
}
/// A graph that answers "did `probe` arrive intact?" as a white or black
/// surface.
///
/// The two halves that have to agree are the offsets `wxsl-core` computed
/// and the offsets WGSL's own layout rules give the generated struct. They
/// are checked here the only way that is really convincing: on the GPU,
/// against a literal the compiler inlined.
///
/// Every graph also carries a second `f32` value, so no probe is alone in
/// its buffer — which is where the interesting failure lives. `vec3f`
/// occupies 12 bytes and aligns to 16, so a scalar beside one is either at
/// 12 or at 16 and only one of those is right.
///
/// `declare` is what puts the two values *into* the graph, and it is the
/// only thing that differs between the two computed layouts this repo
/// has: a uniform parameter (ADR 0023) and a field of the widened
/// instance row (ADR 0024). One probe, two customers, which is what
/// justifies the layout computer being a table rather than a struct.
pub fn probe_graph(
    registry: &NodeRegistry,
    ty: ValueType,
    probe: Value,
    declare: Declare,
) -> Graph {
    let mut graph = Graph::new(format!("probe {ty}"));
    let node = declare(&mut graph, registry, "probe", ty, probe);

    // Reduce the probe to one `f32` that is zero when it arrived intact.
    let (measured, expected): (NodeId, Value) = match ty {
        ValueType::Bool | ValueType::I32 | ValueType::U32 => {
            let to_float = graph.add_node("convert.to_float");
            graph
                .wire(registry, (node, "out"), (to_float, "value"))
                .expect("an integer converts");
            let as_f32 = match probe {
                Value::Bool(flag) => f32::from(u8::from(flag)),
                Value::I32(number) => number as f32,
                Value::U32(number) => number as f32,
                _ => unreachable!("matched on the type above"),
            };
            (to_float, Value::F32(as_f32))
        }
        ValueType::Mat3 | ValueType::Mat4 => {
            // A matrix reaches a comparable value through the one node
            // that consumes one: times a vector of ones, which is the
            // row sums, which changes if any cell moved.
            let vector = if ty == ValueType::Mat3 {
                ValueType::Vec3
            } else {
                ValueType::Vec4
            };
            let transform = graph.add_node("vector.transform");
            graph
                .set_generic(registry, transform, "M", ty)
                .expect("a matrix");
            graph
                .set_generic(registry, transform, "V", vector)
                .expect("its vector");
            let ones = vector.splat(1.0).expect("a float vector");
            graph.set_param(transform, "v", ones);
            graph
                .wire(registry, (node, "out"), (transform, "m"))
                .expect("a matrix into transform");
            let cells = probe.components().expect("a matrix has components");
            let width = if ty == ValueType::Mat3 { 3 } else { 4 };
            let sums: Vec<f32> = (0..width)
                .map(|row| (0..width).map(|col| cells[col * width + row]).sum())
                .collect();
            let expected = match vector {
                ValueType::Vec3 => Value::Vec3(sums.try_into().expect("three")),
                _ => Value::Vec4(sums.try_into().expect("four")),
            };
            (transform, expected)
        }
        _ => (node, probe),
    };

    let difference = graph.add_node("math.subtract");
    let reduced_ty = expected.ty();
    graph
        .set_generic(registry, difference, "A", reduced_ty)
        .expect("allowed");
    graph
        .set_generic(registry, difference, "B", reduced_ty)
        .expect("allowed");
    graph
        .wire(
            registry,
            (measured, output_of(registry, &graph, measured)),
            (difference, "a"),
        )
        .expect("the measured value");
    graph.set_param(difference, "b", expected);

    // `|x|` for a scalar, `length` for a vector: both answer "how far".
    let error = if reduced_ty == ValueType::F32 {
        let absolute = graph.add_node("math.absolute");
        graph
            .set_generic(registry, absolute, "T", ValueType::F32)
            .expect("allowed");
        graph
            .wire(registry, (difference, "out"), (absolute, "a"))
            .expect("f32");
        (absolute, "out")
    } else {
        let length = graph.add_node("vector.length");
        graph
            .set_generic(registry, length, "T", reduced_ty)
            .expect("allowed");
        graph
            .wire(registry, (difference, "out"), (length, "v"))
            .expect("a vector");
        (length, "out")
    };

    // The second value, so the probe is never alone in the buffer.
    let pad = declare(
        &mut graph,
        registry,
        "pad",
        ValueType::F32,
        Value::F32(0.75),
    );
    let pad_difference = graph.add_node("math.subtract");
    graph
        .set_generic(registry, pad_difference, "A", ValueType::F32)
        .expect("allowed");
    graph
        .set_generic(registry, pad_difference, "B", ValueType::F32)
        .expect("allowed");
    graph
        .wire(registry, (pad, "out"), (pad_difference, "a"))
        .expect("f32");
    graph.set_param(pad_difference, "b", Value::F32(0.75));
    let pad_error = graph.add_node("math.absolute");
    graph
        .set_generic(registry, pad_error, "T", ValueType::F32)
        .expect("allowed");
    graph
        .wire(registry, (pad_difference, "out"), (pad_error, "a"))
        .expect("f32");

    let total = graph.add_node("math.add");
    graph
        .set_generic(registry, total, "A", ValueType::F32)
        .expect("allowed");
    graph
        .set_generic(registry, total, "B", ValueType::F32)
        .expect("allowed");
    graph
        .wire(registry, (error.0, error.1), (total, "a"))
        .expect("f32");
    graph
        .wire(registry, (pad_error, "out"), (total, "b"))
        .expect("f32");

    let within = graph.add_node("compare.less");
    graph
        .wire(registry, (total, "out"), (within, "a"))
        .expect("f32");
    graph.set_param(within, "b", Value::F32(1e-3));

    let select = graph.add_node("logic.select");
    graph
        .set_generic(registry, select, "T", ValueType::Vec3)
        .expect("allowed");
    graph.set_param(select, "if_true", Value::Vec3([1.0, 1.0, 1.0]));
    graph.set_param(select, "if_false", Value::Vec3([0.0, 0.0, 0.0]));
    graph
        .wire(registry, (within, "out"), (select, "condition"))
        .expect("a bool");

    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(registry, (select, "out"), (output, "emissive"))
        .expect("vec3f into emissive");
    graph
}

/// The output socket a node in the probe chain answers on.
pub fn output_of(registry: &NodeRegistry, graph: &Graph, node: NodeId) -> &'static str {
    let def = graph.definition(registry, node).expect("in the graph");
    match def.id.as_str() {
        "vector.transform" => "out",
        "convert.to_float" => "out",
        _ => "out",
    }
}

/// A distinctive value of every type, chosen so a byte read from the wrong
/// offset is a different number rather than a coincidence.
pub fn probe_values() -> Vec<(ValueType, Value)> {
    vec![
        (ValueType::Bool, Value::Bool(true)),
        (ValueType::I32, Value::I32(-13)),
        (ValueType::U32, Value::U32(29)),
        (ValueType::F32, Value::F32(0.375)),
        (ValueType::Vec2, Value::Vec2([1.25, -2.5])),
        (ValueType::Vec3, Value::Vec3([3.5, -4.25, 5.125])),
        (ValueType::Vec4, Value::Vec4([6.5, -7.25, 8.125, 9.0])),
        (
            ValueType::Mat3,
            Value::Mat3([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.5]),
        ),
        (
            ValueType::Mat4,
            Value::Mat4(core::array::from_fn(|index| index as f32 * 0.5 - 3.0)),
        ),
    ]
}
