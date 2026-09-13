//! Execution policies on the GPU (plan2 P10, ADR 0035).
//!
//! The device-free half of the policy story — the `due` arithmetic and the
//! stable-storage rule — is tested in `wxsl-render`'s graph tests. This is
//! the half that needs a device: that a `once` pass really records once,
//! that its output is what later frames read, and that a reallocation
//! bakes it again. The pass list is the BRDF LUT proof, hand-built: a
//! compute effect baking the LUT once into a persistent target, and a
//! screen effect displaying it.

use wxsl::core::graph::{Graph, Node};
use wxsl::core::pipeline as doc;
use wxsl::render::effect::{EffectRegistry, BRDF_LUT, LUT_VIEW};
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::wgpu;
use wxsl::render::{
    Attachment, DrawList, Extent, PassDesc, Persistence, PipelineConfig, Policy, Read, RenderGraph,
    ResourceDesc, TargetConfig,
};

mod probe;

use probe::{gpu, render_list_in, unlit};

/// The LUT graph: bake once, view every frame.
fn lut_graph(bake_policy: Policy) -> RenderGraph {
    let mut graph = RenderGraph::new(wgpu::TextureFormat::Rgba8Unorm);
    let lut = graph.resource(
        ResourceDesc::color("brdf lut", wgpu::TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 64,
                height: 64,
            })
            // Written by the bake as storage, read by the view as texture.
            .with_usage(wgpu::TextureUsages::TEXTURE_BINDING)
            .persistent(0),
    );
    graph.pass(
        PassDesc::compute("lut bake", "brdf_lut")
            .with_write(lut)
            .with_policy(bake_policy),
    );
    graph.pass(
        PassDesc::screen("lut view", "lut_view")
            .with_color(Attachment::clear(RenderGraph::TARGET, wgpu::Color::BLACK))
            .with_reads([Read::current(lut)]),
    );
    graph
}

fn target_config(format: wgpu::TextureFormat) -> TargetConfig {
    TargetConfig::new(probe::SIZE, probe::SIZE, format)
}

#[test]
fn a_once_pass_runs_once_and_a_reallocation_runs_it_again() {
    let Some(gpu) = gpu() else { return };
    let target = OffscreenTarget::new(&gpu.device, probe::SIZE, probe::SIZE);
    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        target_config(target.format()),
    )
    .expect("renderer");
    // The proof effects ship as descriptors; an application registers
    // them — that is the whole of "add a compute pass" now.
    renderer.add_effect(BRDF_LUT);
    renderer.add_effect(LUT_VIEW);
    renderer
        .set_graph(lut_graph(Policy::Once))
        .expect("schedules");

    let draws = DrawList::new();
    let render = |renderer: &mut wxsl::render::Renderer| -> Vec<u8> {
        let environment = unlit();
        render_list_in(&gpu, renderer, &target, &draws, &environment).expect("the frame renders")
    };

    // Frame one: the bake is due, and its LUT is on screen.
    let image = render(&mut renderer);
    assert_eq!(renderer.pass_run_count("lut bake"), Some(1));
    assert_eq!(renderer.pass_run_count("lut view"), Some(1));
    let centre = ((probe::SIZE / 2 * probe::SIZE + probe::SIZE / 2) * 4) as usize;
    assert!(
        image[centre] > 40 || image[centre + 1] > 20,
        "the LUT view shows the bake's output; centre pixel {:?}",
        &image[centre..centre + 3]
    );

    // Two more frames: the view runs again — and the bake does not. The
    // image is identical, because what the view reads is exactly what the
    // one bake left behind.
    let image_two = render(&mut renderer);
    let image_three = render(&mut renderer);
    assert_eq!(
        renderer.pass_run_count("lut bake"),
        Some(1),
        "once means once"
    );
    assert_eq!(renderer.pass_run_count("lut view"), Some(3));
    assert_eq!(image, image_two, "a skipped bake changes nothing");
    assert_eq!(image_two, image_three);

    // A resize reallocates the pool — every slot's contents die with it —
    // so the bake is due one more time.
    renderer.resize(&gpu.device, 48, 48);
    render(&mut renderer);
    assert_eq!(
        renderer.pass_run_count("lut bake"),
        Some(2),
        "the reallocation invalidated the bake"
    );
}

#[test]
fn an_on_demand_pass_sleeps_until_marked() {
    // The same graph with the bake's policy rewritten: not due until the
    // application asks, then due exactly once.
    let Some(gpu) = gpu() else { return };
    let target = OffscreenTarget::new(&gpu.device, probe::SIZE, probe::SIZE);
    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        target_config(target.format()),
    )
    .expect("renderer");
    renderer.add_effect(BRDF_LUT);
    renderer.add_effect(LUT_VIEW);
    renderer
        .set_graph(lut_graph(Policy::OnDemand))
        .expect("schedules");

    let draws = DrawList::new();
    let render = |renderer: &mut wxsl::render::Renderer| {
        let environment = unlit();
        render_list_in(&gpu, renderer, &target, &draws, &environment).expect("renders")
    };

    render(&mut renderer);
    render(&mut renderer);
    assert_eq!(
        renderer.pass_run_count("lut bake"),
        Some(0),
        "nobody asked, so the bake slept — and the view still ran"
    );
    assert_eq!(renderer.pass_run_count("lut view"), Some(2));

    renderer.mark_pass("lut bake");
    render(&mut renderer);
    assert_eq!(
        renderer.pass_run_count("lut bake"),
        Some(1),
        "marked, so it ran"
    );

    render(&mut renderer);
    assert_eq!(
        renderer.pass_run_count("lut bake"),
        Some(1),
        "the mark was consumed; it sleeps again"
    );
}

#[test]
fn the_document_policy_setting_reaches_the_pass_list() {
    // The document vocabulary grew the `policy` setting. The shape: the
    // deferred chain shades once into `scene`, a `once`-policy view moves
    // it into `stable`, and a per-frame view presents that — and the
    // compiler promotes `stable` to stable storage, because a skipped
    // pass's contents must survive the frames it skips.
    let registry = doc::registry();
    let mut graph = Graph::new("once view");
    let draws = graph.add_node(doc::SOURCE_SCENE);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material =
        graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "gbuffer"));
    let scene = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_HISTORY, "0"),
    );
    let lighting = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("lighting")
            .with_setting(doc::SETTING_EFFECT, "deferred_lighting"),
    );
    let stable = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("stable")
            .with_setting(doc::SETTING_HISTORY, "0"),
    );
    let once = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("once view")
            .with_setting(doc::SETTING_EFFECT, "lut_view")
            .with_setting(doc::SETTING_POLICY, "once"),
    );
    let every = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("every frame")
            .with_setting(doc::SETTING_EFFECT, "lut_view"),
    );
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((draws, "draws"), (material, "draws")),
        ((gbuffer, "gbuffer"), (material, "gbuffer")),
        ((gbuffer, "gbuffer"), (lighting, "gbuffer")),
        ((scene, "color"), (lighting, "into")),
        // The once pass reads the shaded scene and writes `stable`; the
        // per-frame pass reads what it wrote and presents it — `into`
        // unconnected, so it writes the frame's target.
        ((scene, "color"), (once, "image")),
        ((stable, "color"), (once, "into")),
        ((stable, "color"), (every, "image")),
        ((every, "color"), (present, "surface")),
    ] {
        graph.wire(&registry, from, to).expect("wiring");
    }

    let mut effects = EffectRegistry::shipped();
    effects.add(LUT_VIEW);
    let compiled = wxsl::render::compile_pipeline(
        &graph,
        &registry,
        &effects,
        &PipelineConfig::new(target_config(wgpu::TextureFormat::Rgba8Unorm)),
    )
    .expect("the document compiles");
    let once_pass = compiled
        .passes()
        .iter()
        .find(|pass| pass.label == "once view")
        .expect("the once pass");
    assert_eq!(once_pass.policy, Policy::Once);

    // The chain's resource was promoted: it survives frames now, which is
    // what makes the pass's skipping sound.
    let promoted = compiled
        .resources()
        .iter()
        .find(|resource| resource.label == "stable")
        .expect("the chain's resource");
    assert_ne!(promoted.persistence, Persistence::Transient);

    // And it schedules.
    compiled.schedule().expect("the once-view graph schedules");
}
