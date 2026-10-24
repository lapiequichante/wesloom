//! GPU regression for the sun disc at mixed sky/geometry quads on Metal.
use glam::Mat4;
use wxsl::core::{abi::MaterialStage, graph::Graph, node::Value};
use wxsl::render::effect::sun;
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::pass::DEPTH_FORMAT;
use wxsl::render::types::Color;
use wxsl::render::{
    Attachment, DepthAttachment, DrawItem, DrawSource, Material, Mesh, PassDesc, RenderGraph,
    Renderer, ResourceDesc, TargetConfig,
};

mod probe;

#[test]
fn the_sun_does_not_light_occlusion_edges_when_out_of_view_or_off() {
    let Some(gpu) = probe::gpu() else { return };
    let size = 256;
    let target = OffscreenTarget::new(&gpu.device, size, size);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size, size, target.format()),
    )
    .unwrap();
    let mut graph = RenderGraph::new(target.format());
    let depth = graph.resource(ResourceDesc::color("depth", DEPTH_FORMAT));
    graph.pass(
        PassDesc::geometry(
            "opaque",
            DrawSource::Scene(Default::default()),
            MaterialStage::FORWARD_LIT,
        )
        .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK))
        .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    sun::append(&mut graph, RenderGraph::TARGET, depth, 0.08, "sun");
    renderer.set_graph(graph).unwrap();
    let mut material_graph = Graph::new("black occluder");
    material_graph.add_node("output.surface");
    let material = Material::from_graph(&material_graph, &wxsl::stdlib::registry()).unwrap();
    let mesh = Mesh::cube(&gpu.device, 1.0);
    // Slanted silhouettes cross 2x2 quads, with both occluded and sky lanes.
    let item = DrawItem::new(&mesh, &material)
        .with_transform(Mat4::from_rotation_z(0.31) * Mat4::from_rotation_y(0.4));
    let draws = wxsl::render::single_draw(item);
    let mut environment = probe::unlit();
    environment.camera.far = 9000.0;
    for radiance in [0.0, 150.0] {
        renderer
            .set_pass_param("sun", "radiance", Value::Vec4([radiance; 4]))
            .unwrap();
        renderer
            .set_pass_param("sun", "direction", Value::Vec4([1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        let image =
            probe::render_list_in(&gpu, &mut renderer, &target, &draws, &environment).unwrap();
        assert!(
            image.chunks_exact(4).all(|pixel| pixel[..3] == [0; 3]),
            "an offscreen or unlit sun must not add light at silhouettes (radiance {radiance})"
        );
    }
    // The disc still lights visible sky and is hidden by geometry.
    renderer
        .set_pass_param("sun", "direction", Value::Vec4([0.0, 0.0, -1.0, 0.0]))
        .unwrap();
    let visible = probe::render_list_in(
        &gpu,
        &mut renderer,
        &target,
        &wxsl::render::DrawList::new(),
        &environment,
    )
    .unwrap();
    assert!(visible.chunks_exact(4).any(|pixel| pixel[0] > 200));
    let hidden = probe::render_list_in(&gpu, &mut renderer, &target, &draws, &environment).unwrap();
    let centre = ((size / 2 * size + size / 2) * 4) as usize;
    assert_eq!(&hidden[centre..centre + 3], &[0; 3]);
}
