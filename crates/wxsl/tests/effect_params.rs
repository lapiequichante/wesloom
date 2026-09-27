//! Effect parameters on the GPU (plan3 N4, ADR 0042).
//!
//! The device-free half — the layout the descriptor computes, the struct
//! header the variant prepends, the variant key that folds the layout and
//! never a value — is pinned in `wxsl-render`'s own tests. This is the
//! proof that needs a device: a pass parameter set through
//! [`Renderer::set_pass_param`] reaches the shader at the next frame,
//! moves the picture, and costs nothing — no variant recompiled, no
//! pipeline rebuilt, `cache_stats()` unmoved.
//!
//! The scene is the emissive plane the material tests use, through a
//! bloom chain: a surface radiating more than white is exactly the thing
//! bloom's threshold selects on, in linear radiance, before the display
//! transform (ADR 0039) — so the threshold *is* the parameter a slider
//! moves.

use glam::Mat4;
use wxsl::core::graph::{Graph, Node, NodeId};
use wxsl::core::node::Value;
use wxsl::core::pipeline as doc;
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::{
    compile_pipeline, DrawItem, EffectRegistry, Mesh, PipelineConfig, Renderer, TargetConfig,
};

mod probe;

use probe::{gpu, render_list_in, unlit};

/// The bloom chain as a document: a forward pass writing an HDR colour
/// target, bloom over it, and the display transform presenting. The same
/// shape as the gallery's deferred-bloom document, without the G-buffer —
/// the plane is emissive, so there is nothing for a light to do.
fn bloom_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("bloom parameters");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let pass = graph.add_node(doc::PASS_GEOMETRY);
    let scene_color = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bloom = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("bloom")
            .with_setting(doc::SETTING_EFFECT, "bloom"),
    );
    let linear = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("linear")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let tonemap = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "tonemap"),
    );
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("bloom wiring");
    };
    wire((scene, "draws"), (pass, "draws"));
    wire((depth, "depth"), (pass, "depth"));
    wire((scene_color, "color"), (pass, "into"));
    wire((scene_color, "color"), (bloom, "image"));
    wire((linear, "color"), (bloom, "into"));
    wire((linear, "color"), (tonemap, "image"));
    wire((tonemap, "color"), (present, "surface"));
    graph
}

/// A plane radiating well above white — above `threshold` plus `knee`, so
/// the default bloom keeps it whole — facing the camera on black.
fn bright_material() -> Graph {
    let mut graph = Graph::new("bright plane");
    let glow =
        graph.add(Node::new("param.value").with_param("value", Value::Vec3([3.0, 3.0, 3.0])));
    let output = graph.add_node(wxsl::core::abi::SURFACE_OUTPUT_ID);
    graph
        .wire(
            &wxsl::stdlib::registry(),
            (glow, "out"),
            (output, "emissive"),
        )
        .expect("vec3f into emissive");
    graph
}

/// The mean of the image, as one number a glow moves upward.
fn mean(image: &[u8]) -> f64 {
    image.iter().map(|byte| u32::from(*byte)).sum::<u32>() as f64 / image.len() as f64
}

#[test]
fn a_parameter_moves_blooms_threshold_live_and_costs_nothing() {
    let Some(gpu) = gpu() else { return };
    let size = 128;
    let target = OffscreenTarget::new(&gpu.device, size, size);

    let effects = EffectRegistry::shipped();
    let document = bloom_document();
    let graph = compile_pipeline(
        &document,
        &wxsl::render::document_registry(&effects),
        &effects,
        &PipelineConfig::new(TargetConfig::new(size, size, target.format())),
    )
    .expect("the bloom document compiles");
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size, size, target.format()),
    )
    .expect("renderer");
    renderer.set_graph(graph).expect("schedules");

    let registry = wxsl::stdlib::registry();
    let material = wxsl::render::Material::from_graph(&bright_material(), &registry)
        .expect("the material compiles");
    let mesh = Mesh::plane(&gpu.device, 2.0);
    // The plane's graph declares one parameter, so the draw carries its
    // bindings — at the declared defaults, which is all it needs.
    let mut bindings = renderer.material_bindings(&gpu.device, &material);
    bindings
        .upload(&gpu.device, &gpu.queue)
        .expect("nothing unbound");
    let item = DrawItem::new(&mesh, &material)
        .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2))
        .with_bindings(&bindings);
    let draws = wxsl::render::single_draw(item);

    // As authored: the descriptor's default threshold, the image the
    // `const`s used to produce.
    let bright =
        render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()).expect("the frame renders");
    let bright = mean(&bright);

    // Threshold above everything there is: the bright pass keeps nothing,
    // the blur is zero, and the pass is an identity — the glow is gone.
    renderer
        .set_pass_param("bloom", "threshold", Value::F32(1000.0))
        .expect("the pass and the parameter both exist");
    let dull =
        render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()).expect("the frame renders");
    let dull = mean(&dull);
    assert!(
        dull < bright - 0.5,
        "the glow should be gone with the threshold above every radiance: \
         {dull} vs {bright}"
    );

    // And the picture is the parameter's, not a one-way switch: back down
    // under the surface's radiance, the glow returns.
    renderer
        .set_pass_param("bloom", "threshold", Value::F32(0.5))
        .expect("set");
    let wide =
        render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()).expect("the frame renders");
    let wide = mean(&wide);
    assert!(
        wide > dull + 0.5,
        "a threshold below the surface's radiance glows again: {wide} vs {dull}"
    );

    // The value a slider would show round-trips through the same offsets
    // it was written at.
    assert_eq!(
        renderer.pass_param("bloom", "threshold"),
        Some(Value::F32(0.5)),
        "the value reads back as it was set"
    );

    // The whole point (the material parameters' bar, pass-level): eight
    // more slider moves, a frame each, and nothing compiles. A `const`
    // made every one of these a recompile. Misses are the count to hold
    // still — *hits* climb every frame, because a live slider's pipeline
    // is still being asked for.
    let stats = renderer.cache_stats();
    let pipelines = renderer.pipeline_count();
    for threshold in [0.1f32, 0.7, 1.2, 2.0, 2.9, 1.6, 0.3, 0.9] {
        renderer
            .set_pass_param("bloom", "threshold", Value::F32(threshold))
            .expect("set");
        render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()).expect("the frame renders");
    }
    assert_eq!(
        renderer.cache_stats().misses,
        stats.misses,
        "not one variant recompiled"
    );
    assert_eq!(
        renderer.pipeline_count(),
        pipelines,
        "not one pipeline rebuilt"
    );

    // A parameter the descriptor does not declare is a named error, as a
    // typo'd material parameter is.
    let error = renderer
        .set_pass_param("bloom", "glow", Value::F32(1.0))
        .expect_err("no such parameter");
    assert!(error.to_string().contains("glow"), "{error}");
    assert!(error.to_string().contains("bloom"), "{error}");

    // So is a label nothing declares parameters for — the same name
    // `mark_pass` would address.
    let error = renderer
        .set_pass_param("no such pass", "threshold", Value::F32(1.0))
        .expect_err("no such pass");
    assert!(error.to_string().contains("no such pass"), "{error}");
}

#[test]
fn defaults_render_as_authored_and_a_retuned_effect_keeps_its_values() {
    let Some(gpu) = gpu() else { return };
    let size = 128;
    let target = OffscreenTarget::new(&gpu.device, size, size);
    let effects = EffectRegistry::shipped();
    let graph = compile_pipeline(
        &bloom_document(),
        &wxsl::render::document_registry(&effects),
        &effects,
        &PipelineConfig::new(TargetConfig::new(size, size, target.format())),
    )
    .expect("the bloom document compiles");
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size, size, target.format()),
    )
    .expect("renderer");
    renderer.set_graph(graph).expect("schedules");

    // Before anything is set, the block is the descriptor's declaration —
    // the values its shader used to `const` into itself.
    assert_eq!(
        renderer.pass_param("bloom", "threshold"),
        Some(Value::F32(1.0)),
        "the declared default"
    );
    assert_eq!(
        renderer.pass_param("bloom", "strength"),
        Some(Value::F32(0.85))
    );
    // An effect with no knobs has no block to ask about.
    assert_eq!(renderer.pass_param("tonemap", "strength"), None);
    assert!(renderer.pass_parameter_layout("tonemap").is_none());

    // A pass list replaced by the same document — what a document edit
    // compiles to when nothing changed — keeps the tuned value under the
    // same label: a re-tune survives a re-apply.
    renderer
        .set_pass_param("bloom", "threshold", Value::F32(0.25))
        .expect("set");
    let graph = compile_pipeline(
        &bloom_document(),
        &wxsl::render::document_registry(&effects),
        &effects,
        &PipelineConfig::new(TargetConfig::new(size, size, target.format())),
    )
    .expect("compiles again");
    renderer.set_graph(graph).expect("schedules");
    assert_eq!(
        renderer.pass_param("bloom", "threshold"),
        Some(Value::F32(0.25)),
        "the tuned value survives the pass list changing under it"
    );
}
