//! A local probe captured in the frame and read by one pass (ADR 0102): a
//! mirror sphere at the probe's centre must show, in each direction, what
//! the probe saw there, at the frame's exposure, and the sky where nothing
//! was. A face turned or flipped the wrong way puts a colour on the wrong
//! side of the sphere; a capture left exposed darkens it; a pass that
//! ignored its own cubes shows the sky everywhere.
// Only the constant sky of the bake fixture.
#[allow(dead_code)]
#[path = "probe/ibl.rs"]
mod fixture;
mod probe;

use glam::{Mat4, Vec3};
use wxsl::core::abi::MaterialStage;
use wxsl::core::node::Value;
use wxsl::core::scene::{TagExpr, Tags};
use wxsl::render::effect::ibl;
use wxsl::render::environment::LocalProbe;
use wxsl::render::graph::RenderGraph;
use wxsl::render::pass::{
    Attachment, DepthAttachment, DrawSource, Extent, PassDesc, Policy, ResourceDesc, DEPTH_FORMAT,
};
use wxsl::render::types::{Color, TextureFormat};
use wxsl::render::{DrawItem, DrawList, Mesh};

/// The constant sky of the bake fixture, the frame's exposure, and the
/// boxes' emission.
const SKY: [f32; 3] = [2.0, 3.0, 4.0];
const EXPOSURE: f32 = 0.1;
const GLOW: f32 = 5.0;

/// The constant sky baked into the frame's maps, a probe of everything
/// but the mirror, and the mirror drawn with the probe's cubes, or with
/// the frame's.
fn plan(format: wxsl::render::wgpu::TextureFormat, own_cubes: bool) -> RenderGraph {
    let mut graph = RenderGraph::new(format);
    let source = graph.resource(
        ResourceDesc::color("sky", TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 128,
                height: 64,
            })
            .persistent(0),
    );
    graph.pass(
        PassDesc::screen("sky", fixture::SOURCE.id)
            .with_color(Attachment::clear(source, Color::BLACK))
            .with_policy(Policy::Once),
    );
    let maps = ibl::append_filtered_bake(&mut graph, source, "sky IBL", 32, 8, 6);
    graph.declare_environment_maps(maps.diffuse, maps.specular);
    let probe = ibl::append_probe(
        &mut graph,
        TagExpr::parse("!mirror").unwrap(),
        maps.radiance,
        "probe",
        32,
        8,
        6,
        64,
    );
    let depth = graph.resource(ResourceDesc::color("depth", DEPTH_FORMAT));
    let mut mirror = PassDesc::geometry(
        "mirror",
        DrawSource::Scene(TagExpr::parse("mirror").unwrap()),
        MaterialStage::FORWARD_LIT,
    )
    .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK))
    .with_depth(DepthAttachment::clear(depth, 1.0));
    if own_cubes {
        mirror = mirror.with_environment(probe.diffuse, probe.specular);
    }
    graph.pass(mirror);
    graph
}

fn surface(metallic: f32, base: [f32; 3], emissive: [f32; 3]) -> wxsl::core::graph::Graph {
    let mut graph = wxsl::core::graph::Graph::new("probe surface");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "metallic", Value::F32(metallic));
    graph.set_param(out, "roughness", Value::F32(0.0));
    graph.set_param(out, "base_color", Value::Vec3(base));
    graph.set_param(out, "emissive", Value::Vec3(emissive));
    graph
}

#[test]
fn a_mirror_at_the_probe_shows_each_direction_and_only_with_its_own_cubes() {
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    harness.renderer.add_effect(fixture::SOURCE);
    let mirror = harness.material(&surface(1.0, [1.0; 3], [0.0; 3]));
    let glowing = [
        (Vec3::X, [GLOW, 0.0, 0.0]),
        (Vec3::Y, [0.0, 0.0, GLOW]),
        (Vec3::Z, [0.0, GLOW, 0.0]),
    ]
    .map(|(axis, emissive)| (axis, harness.material(&surface(0.0, [0.0; 3], emissive))));
    let sphere = Mesh::sphere(&harness.gpu.device, 1.0);
    let cube = Mesh::cube(&harness.gpu.device, 1.0);
    let tags = Tags::from_iter(["mirror"]);
    let mut draws = DrawList::new();
    draws.push(DrawItem::new(&sphere, &mirror).with_tags(&tags));
    // Six-metre boxes ten metres out: the +Z one behind the camera, which
    // only the probe sees.
    for (axis, material) in &glowing {
        draws.push(DrawItem::new(&cube, material).with_transform(
            Mat4::from_translation(*axis * 10.0) * Mat4::from_scale(Vec3::splat(6.0)),
        ));
    }
    let mut environment = probe::unlit();
    environment.exposure = EXPOSURE;
    environment.probe = Some(LocalProbe {
        center: Vec3::ZERO,
        near: 0.05,
        far: 50.0,
    });
    let camera = environment.camera;
    // Where on the sphere a view ray reflects along `axis`: the normal
    // halfway between it and the way back to the eye from that point.
    let reflecting = |axis: Vec3| {
        let mut normal = (camera.eye.normalize() + axis).normalize();
        for _ in 0..16 {
            normal = ((camera.eye - normal).normalize() + axis).normalize();
        }
        probe::pixel_of(camera, normal * 0.999)
    };
    let linear = |value: f32| (value * 255.0) as i32;
    let near = |pixel: [u8; 4], expected: [f32; 3], what: &str| {
        for channel in 0..3 {
            let (got, want) = (i32::from(pixel[channel]), linear(expected[channel]));
            assert!(
                (got - want).abs() <= 24,
                "{what}: {pixel:?} against {:?}",
                expected.map(linear)
            );
        }
    };
    let sky = SKY.map(|channel| channel * EXPOSURE);
    let glow = GLOW * EXPOSURE;

    harness
        .renderer
        .set_graph(plan(harness.target.format(), true))
        .unwrap();
    let image = probe::render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &environment,
    )
    .unwrap();
    let at = |axis: Vec3| {
        let (x, y) = reflecting(axis);
        probe::pixel(&image, x, y)
    };
    near(
        at(Vec3::Z),
        [0.0, glow, 0.0],
        "+Z, the box behind the camera",
    );
    near(at(Vec3::X), [glow, 0.0, 0.0], "+X, the right of the sphere");
    near(at(Vec3::Y), [0.0, 0.0, glow], "+Y, the top of the sphere");
    near(at(Vec3::NEG_X), sky, "-X, nothing but the sky");
    near(at(Vec3::NEG_Y), sky, "-Y, nothing but the sky");

    // The same mirror with the frame's cubes sees only the sky.
    harness
        .renderer
        .set_graph(plan(harness.target.format(), false))
        .unwrap();
    let image = probe::render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &environment,
    )
    .unwrap();
    let (x, y) = reflecting(Vec3::Z);
    near(
        probe::pixel(&image, x, y),
        sky,
        "+Z without the probe's cubes",
    );
}
