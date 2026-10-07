//! Buffers as graph resources, on the GPU (plan2 P11, ADR 0036).
//!
//! The device-free half — ordering, the never-alias rule, the named
//! attachment-shape error — lives in `wxsl-frame`'s graph tests. This is
//! the proof that needs a device: a compute effect fills a storage
//! buffer, a screen effect reads it as storage and draws it, and the
//! picture answers "did the data arrive through the pass group?" by its
//! direction.

use wxsl::render::effect::{RAMP_FILL, RAMP_VIEW};
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::types::Color;
use wxsl::render::{Attachment, DrawList, PassDesc, Read, RenderGraph, ResourceDesc, TargetConfig};

mod probe;

use probe::{gpu, render_list_in, unlit};

#[test]
fn compute_written_indirect_arguments_render_the_same_geometry_as_a_direct_draw() {
    use wxsl::core::{
        abi::MaterialStage,
        graph::{Graph, Node},
        node::Value,
    };
    use wxsl::render::effect::{
        Effect, EffectKind, EffectOutput, EffectOutputShape, EffectParameter, EffectShader,
    };
    use wxsl::render::pass::DEPTH_FORMAT;
    use wxsl::render::{DepthAttachment, DrawItem, DrawSource, Material, Mesh};

    let Some(gpu) = gpu() else { return };
    let target = OffscreenTarget::new(&gpu.device, probe::SIZE, probe::SIZE);
    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(probe::SIZE, probe::SIZE, target.format()),
    )
    .expect("renderer");
    probe::present_linear(&mut renderer);
    let mut material_graph = Graph::new("emissive cube");
    material_graph
        .add(Node::new("output.surface").with_param("emissive", Value::Vec3([0.7, 0.2, 0.1])));
    let material =
        Material::from_graph(&material_graph, &wxsl::stdlib::registry()).expect("material");
    let mesh = Mesh::cube(&gpu.device, 1.0);
    let draws = wxsl::render::single_draw(DrawItem::new(&mesh, &material));
    let direct =
        render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()).expect("direct frame");
    assert!(
        direct.chunks_exact(4).any(|pixel| pixel[0] > 100),
        "the reference must draw geometry"
    );

    renderer.add_effect(Effect {
        id: "test.indirect_arguments",
        label: "Indirect arguments",
        description: "Write one indexed draw record",
        inputs: &[],
        outputs: &[EffectOutput {
            name: "arguments",
            shape: EffectOutputShape::Buffer,
            description: "indexed draw record",
        }],
        parameters: &[EffectParameter {
            name: "index_count",
            default: Value::U32(0),
        }],
        kind: EffectKind::Compute {
            entry: "fill_arguments",
            workgroups: [1, 1, 1],
        },
        shader: EffectShader::Source {
            path: "package::test::indirect_arguments",
            wxsl: include_str!("probe/indirect.wxsl"),
        },
    });
    let mut graph = RenderGraph::new(target.format());
    let args = graph.resource(ResourceDesc::buffer("arguments", 20));
    let depth = graph.resource(ResourceDesc::color("depth", DEPTH_FORMAT));
    // Deliberately declared before its writer: the shared scheduler must
    // order the compute output before the indirect command consumes it.
    graph.pass(
        PassDesc::geometry(
            "draw",
            DrawSource::Indirect {
                buffer: args,
                offset: 0,
                count: 1,
                draw: 0,
            },
            MaterialStage::FORWARD_LIT,
        )
        .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK))
        .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    graph.pass(PassDesc::compute("fill arguments", "test.indirect_arguments").with_write(args));
    renderer.set_graph(graph).expect("schedules");
    renderer
        .set_pass_param(
            "fill arguments",
            "index_count",
            Value::U32(mesh.index_count()),
        )
        .expect("parameter");
    let indirect =
        render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()).expect("indirect frame");
    assert_eq!(direct, indirect);
}

#[test]
fn a_compute_written_buffer_reaches_a_screen_pass_through_the_pass_group() {
    let Some(gpu) = gpu() else { return };
    let size = 256;
    let target = OffscreenTarget::new(&gpu.device, size, size);
    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size, size, target.format()),
    )
    .expect("renderer");
    renderer.add_effect(RAMP_FILL);
    renderer.add_effect(RAMP_VIEW);

    let mut graph = RenderGraph::new(target.format());
    let ramp = graph.resource(ResourceDesc::buffer("ramp", 256 * 4));
    graph.pass(PassDesc::compute("fill ramp", "wxsl.ramp_fill").with_write(ramp));
    graph.pass(
        PassDesc::screen("show ramp", "wxsl.ramp_view")
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK))
            .with_reads([Read::current(ramp)]),
    );
    renderer.set_graph(graph).expect("schedules");

    let image = render_list_in(&gpu, &mut renderer, &target, &DrawList::new(), &unlit())
        .expect("the frame renders");

    let pixel = |x: u32, y: u32| {
        let index = ((y * size + x) * 4) as usize;
        [image[index], image[index + 1], image[index + 2]]
    };
    // The ease starts low and ends high; green dominates on the left, red
    // on the right — a direction a memcpy or an empty buffer cannot fake.
    let left = pixel(8, size / 2);
    let right = pixel(size - 8, size / 2);
    assert!(
        left[1] > left[0] + 40,
        "the left edge is the ramp's low end: {left:?}"
    );
    assert!(
        right[0] > right[1] + 40,
        "the right edge is the ramp's high end: {right:?}"
    );

    // And the compute pass is due only when its policy says so: with none
    // set, it ran for this frame — `pass_run_count` is how a test watches
    // it.
    assert_eq!(renderer.pass_run_count("fill ramp"), Some(1));
    assert_eq!(renderer.pass_run_count("show ramp"), Some(1));
}

/// The same proof, compiled *from a document* (plan3 N3): a
/// `resource.buffer`, a `pass.compute.ramp_fill` writing it, and a
/// `pass.screen` running `ramp_view` reading it — the vocabulary, end to
/// end on a device.
#[test]
fn the_buffer_ramp_compiles_from_a_document_and_reaches_the_screen() {
    let Some(gpu) = gpu() else { return };
    let size = 256;
    let target = OffscreenTarget::new(&gpu.device, size, size);

    // The effects the document names, registered exactly as an
    // application registers them; the document registry is derived from
    // the same registry, so its `pass.compute` nodes type against what
    // will run.
    let effects = wxsl::render::EffectRegistry::default()
        .with(RAMP_FILL)
        .with(RAMP_VIEW);
    let registry = wxsl::render::document_registry(&effects);

    let mut document = wxsl::core::pipeline::document("buffer ramp");
    let ramp = document.add(
        wxsl::core::graph::Node::new(wxsl::core::pipeline::RESOURCE_BUFFER)
            .with_label("ramp")
            .with_setting(wxsl::core::pipeline::SETTING_BYTES, "1024"),
    );
    let fill = document.add(
        wxsl::core::graph::Node::new(format!(
            "{}wxsl.ramp_fill",
            wxsl::core::pipeline::PASS_COMPUTE_PREFIX
        ))
        .with_label("fill ramp"),
    );
    let view = document.add(
        wxsl::core::graph::Node::new(wxsl::core::pipeline::PASS_SCREEN)
            .with_label("show ramp")
            .with_setting(wxsl::core::pipeline::SETTING_EFFECT, "wxsl.ramp_view"),
    );
    let present = document.add_node(wxsl::core::pipeline::PRESENT);
    for (from, to) in [
        ((ramp, "buffer"), (fill, "ramp")),
        ((ramp, "buffer"), (view, "buffer")),
        ((view, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("ramp wiring");
    }

    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        wxsl::render::TargetConfig::new(size, size, target.format()),
    )
    .expect("renderer");
    renderer.add_effect(RAMP_FILL);
    renderer.add_effect(RAMP_VIEW);
    let graph = wxsl::render::compile_pipeline(
        &document,
        &wxsl::render::document_registry(&effects),
        &effects,
        &wxsl::render::PipelineConfig::new(wxsl::render::TargetConfig::new(
            size,
            size,
            target.format(),
        )),
    )
    .expect("the ramp document compiles");
    renderer.set_graph(graph).expect("schedules");

    let image = render_list_in(&gpu, &mut renderer, &target, &DrawList::new(), &unlit())
        .expect("the frame renders");

    let pixel = |x: u32, y: u32| {
        let index = ((y * size + x) * 4) as usize;
        [image[index], image[index + 1], image[index + 2]]
    };
    let left = pixel(8, size / 2);
    let right = pixel(size - 8, size / 2);
    assert!(
        left[1] > left[0] + 40,
        "the left edge is the ramp's low end: {left:?}"
    );
    assert!(
        right[0] > right[1] + 40,
        "the right edge is the ramp's high end: {right:?}"
    );
    assert_eq!(renderer.pass_run_count("fill ramp"), Some(1));
    assert_eq!(renderer.pass_run_count("show ramp"), Some(1));
}
