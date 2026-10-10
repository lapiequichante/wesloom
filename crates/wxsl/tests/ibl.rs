//! GPU proofs for original HDR conversion and actual GGX roughness mips.
#[path = "probe/ibl.rs"]
mod fixture;
mod probe;

#[test]
fn parameterized_ibl_shaders_compile_without_a_device() {
    let set = wxsl::core::lighting::LightingSet::default();
    let macros = wxsl::core::macros::MacroSet::new();
    let library = wxsl::stdlib_library();
    for effect in fixture::effects() {
        wxsl::render::variants::EffectRequest::new(effect, &macros, &set, &[])
            .compile(&library)
            .1
            .unwrap();
    }
    wxsl::render::variants::EffectRequest::new(
        wxsl::render::effect::ibl::BACKGROUND,
        &macros,
        &set,
        &[],
    )
    .compile(&library)
    .1
    .unwrap();
}

#[test]
fn authored_hdr_environment_background_is_depth_masked_and_matches_both_paths() {
    use wxsl::render::{DrawItem, PipelineConfig, StockPipeline, TargetConfig};
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    let config = PipelineConfig::new(TargetConfig::new(
        probe::SIZE,
        probe::SIZE,
        harness.target.format(),
    ));
    let mut surface = wxsl::core::graph::Graph::new("black opaque occluder");
    let out = surface.add_node("output.surface");
    surface.set_param(out, "metallic", wxsl::core::node::Value::F32(1.0));
    surface.set_param(out, "roughness", wxsl::core::node::Value::F32(0.0));
    surface.set_param(out, "base_color", wxsl::core::node::Value::Vec3([0.0; 3]));
    let material = harness.material(&surface);
    let mesh = wxsl::render::Mesh::cube(&harness.gpu.device, 1.5);
    let draws = wxsl::render::single_draw(DrawItem::new(&mesh, &material));
    let source = wxsl::render::environment::upload_environment_image(
        &harness.gpu.device,
        &harness.gpu.queue,
        1,
        1,
        &[[2.0, 3.0, 4.0]],
    )
    .unwrap();
    harness.renderer.import_resource("source HDR", source.view);
    let mut images = Vec::new();
    for stock in StockPipeline::ALL {
        fixture::hdr_plan(*stock, &config).schedule().unwrap();
        let doc = fixture::hdr_document(*stock);
        let roundtrip: wxsl::core::graph::Graph =
            serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        let effects = wxsl::render::effect::EffectRegistry::shipped();
        let registry = wxsl::render::pipeline_doc::document_registry(&effects);
        let plan =
            wxsl::render::pipeline_doc::compile(&roundtrip, &registry, &effects, &config).unwrap();
        plan.schedule().unwrap();
        harness.renderer.set_graph(plan).unwrap();
        let image =
            probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
                .unwrap();
        assert!(
            probe::pixel(&image, 0, 0)[2] > 200,
            "environment must fill clear depth"
        );
        assert!(
            probe::pixel(&image, 32, 32)[0] < 35,
            "black opaque geometry must not become sky"
        );
        assert_eq!(
            image,
            probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
                .unwrap()
        );
        assert_eq!(
            harness.renderer.pass_run_count("document IBL specular 5/5"),
            Some(1)
        );
        images.push(image);
    }
    assert!(images[0]
        .iter()
        .zip(&images[1])
        .all(|(a, b)| a.abs_diff(*b) <= 2));
}

#[test]
fn diffuse_integrates_a_small_bright_emitter_without_sparse_fireflies() {
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    for effect in fixture::effects() {
        harness.renderer.add_effect(effect);
    }
    harness
        .renderer
        .set_graph(fixture::hotspot_plan(harness.target.format()))
        .unwrap();
    let draws = wxsl::render::DrawList::new();
    let image =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    let cap_energy = 2000.0 * (1.0 - 0.9995_f32.powi(2));
    // Every normal here sees the entire polar cap. Its exact irradiance/PI is
    // background + cap radiance * sin(theta)^2 * N.y. Float16 conversion and
    // finite source texels/quadrature are approximate, not an exact integrator.
    for y in 14..24 {
        for x in 23..30 {
            let px = (((x as f32 + 0.5) / probe::SIZE as f32 * 6.0).fract() * 2.0) - 1.0;
            let py = (((y as f32 + 0.5) / probe::SIZE as f32 * 5.0).fract() * 2.0) - 1.0;
            let ny = (1.0 + px * px + py * py).sqrt().recip();
            let expected = 0.1 + cap_energy * ny;
            let actual = f32::from(probe::pixel(&image, x, y)[0]) / 255.0 * 8.0;
            assert!(
                (actual - expected).abs() < 0.25,
                "diffuse hotspot ({x},{y}): {actual} instead of {expected}"
            );
        }
    }
    let again =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    assert_eq!(image, again);
    assert_eq!(harness.renderer.pass_run_count("probe diffuse 2"), Some(1));
}

#[test]
fn background_tracks_camera_rotation_not_translation_and_applies_exposure_once() {
    use wxsl::render::pass::{PassKind, Read};
    use wxsl::render::{DrawList, RenderRequest, StockPipeline};
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    for effect in fixture::effects() {
        harness.renderer.add_effect(effect);
    }
    harness.renderer.set_pipeline(StockPipeline::Forward);
    let mut plan = harness.renderer.render_graph().clone();
    fixture::append_lighting_bake(&mut plan, true);
    let tonemap = plan
        .passes()
        .iter()
        .position(|p| matches!(&p.kind, PassKind::Screen { effect } if effect == "wxsl.tonemap"))
        .unwrap();
    let color = plan.passes()[tonemap].reads[0].resource;
    let depth = plan
        .passes()
        .iter()
        .filter_map(|p| p.depth.as_ref().map(|a| a.resource))
        .next_back()
        .unwrap();
    let radiance = plan.resource_by_label("lighting IBL radiance").unwrap();
    let background = wxsl::render::effect::ibl::append_background(
        &mut plan,
        color,
        depth,
        radiance,
        "camera background",
    );
    plan.pass_mut(tonemap).unwrap().reads[0] = Read::current(background);
    harness.renderer.set_graph(plan).unwrap();
    let draws = DrawList::new();
    let mut render = |env: &wxsl::render::Environment| {
        harness
            .renderer
            .render(
                &harness.gpu.device,
                &harness.gpu.queue,
                &RenderRequest {
                    view: harness.target.view(),
                    environment: env,
                    draws: &draws,
                },
            )
            .unwrap();
        harness.gpu.wait();
        harness
            .target
            .read_rgba8(&harness.gpu.device, &harness.gpu.queue)
    };
    let mut env = probe::unlit();
    env.exposure = 0.25;
    let image = render(&env);
    env.camera.eye += glam::Vec3::new(4.0, 2.0, 1.0);
    env.camera.target += glam::Vec3::new(4.0, 2.0, 1.0);
    let translated = render(&env);
    assert!(image
        .iter()
        .zip(&translated)
        .all(|(a, b)| a.abs_diff(*b) <= 2));
    env.camera = wxsl::render::Camera {
        eye: glam::Vec3::new(3.0, 0.0, 0.0),
        ..env.camera
    };
    env.camera.target = glam::Vec3::ZERO;
    assert_ne!(image, render(&env));
    env = probe::unlit();
    env.exposure = 0.125;
    let darker = render(&env);
    assert!(probe::pixel(&darker, 32, 32)[0] < probe::pixel(&image, 32, 32)[0]);
    assert_eq!(
        harness.renderer.pass_run_count("lighting IBL radiance 0"),
        Some(1)
    );
}

#[test]
fn filtered_environment_lights_both_paths_and_import_replacement_rebakes() {
    use wxsl::core::{graph::Graph, node::Value};
    use wxsl::render::{DrawItem, StockPipeline};
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    for effect in fixture::effects() {
        harness.renderer.add_effect(effect);
    }
    let mut graph = Graph::new("IBL surface");
    let output = graph.add_node("output.surface");
    graph.set_param(output, "base_color", Value::Vec3([0.1; 3]));
    graph.set_param(output, "metallic", Value::F32(0.0));
    graph.set_param(output, "roughness", Value::F32(0.45));
    let material = harness.material(&graph);
    let mesh = wxsl::render::Mesh::cube(&harness.gpu.device, 1.5);
    let draws = wxsl::render::single_draw(DrawItem::new(&mesh, &material));
    let mut images = Vec::new();
    for stock in StockPipeline::ALL {
        harness.renderer.set_pipeline(*stock);
        let mut plan = harness.renderer.render_graph().clone();
        fixture::append_lighting_bake(&mut plan, false);
        plan.set_environment_scale(2.0);
        harness.renderer.set_graph(plan).unwrap();
        let first =
            probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
                .unwrap();
        let second =
            probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
                .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            harness.renderer.pass_run_count("lighting IBL specular 5/5"),
            Some(1)
        );
        assert!(
            probe::pixel(&first, 32, 32)[0] > 90,
            "no lamps or analytic ambient: cube lighting must reach the material"
        );
        images.push(first);
    }
    assert!(images[0]
        .iter()
        .zip(&images[1])
        .all(|(a, b)| a.abs_diff(*b) <= 2));

    probe::present_linear(&mut harness.renderer);
    let mut plan = harness.renderer.render_graph().clone();
    use wxsl::render::pass::ResourceDesc;
    use wxsl::render::types::TextureFormat;
    let source = plan.resource(ResourceDesc::imported(
        "source HDR",
        TextureFormat::Rgba32Float,
    ));
    let maps =
        wxsl::render::effect::ibl::append_filtered_bake(&mut plan, source, "import IBL", 16, 4, 5);
    plan.declare_environment_maps(maps.diffuse, maps.specular);
    harness.renderer.set_graph(plan).unwrap();
    let upload = |rgb| {
        wxsl::render::environment::upload_environment_image(
            &harness.gpu.device,
            &harness.gpu.queue,
            1,
            1,
            &[rgb],
        )
        .unwrap()
    };
    let red = upload([2.0, 0.0, 0.0]);
    harness
        .renderer
        .import_resource("source HDR", red.view.clone());
    let first =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    harness
        .renderer
        .import_resource("source HDR", red.view.clone());
    let same =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    assert_eq!(first, same);
    assert_eq!(
        harness.renderer.pass_run_count("import IBL radiance 0"),
        Some(1)
    );
    let blue = upload([0.0, 0.0, 2.0]);
    harness.renderer.import_resource("source HDR", blue.view);
    let changed =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    assert_eq!(
        harness.renderer.pass_run_count("import IBL radiance 0"),
        Some(2)
    );
    assert_ne!(first, changed);
    assert_eq!(probe::pixel(&first, 32, 32)[2], 0);
    assert_eq!(probe::pixel(&changed, 32, 32)[0], 0);
    harness.renderer.invalidate_bakes();
    let refreshed =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    assert_eq!(changed, refreshed);
    assert_eq!(
        harness.renderer.pass_run_count("import IBL radiance 0"),
        Some(3)
    );
    assert!(wxsl::render::environment::upload_environment_image(
        &harness.gpu.device,
        &harness.gpu.queue,
        1,
        1,
        &[[f32::NAN; 3]]
    )
    .is_err());
    assert!(wxsl::render::environment::upload_environment_image(
        &harness.gpu.device,
        &harness.gpu.queue,
        0,
        1,
        &[]
    )
    .is_err());
    let sun = upload([118784.0, 2.0, 3.0]);
    assert_eq!(sun.scale, 7.25);
}

#[test]
fn hdr_constant_and_directional_convolutions_match_analytic_integrals() {
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    for effect in fixture::effects() {
        harness.renderer.add_effect(effect);
    }
    for directional in [false, true] {
        harness
            .renderer
            .set_graph(fixture::plan(harness.target.format(), directional))
            .unwrap();
        let draws = wxsl::render::DrawList::new();
        let first =
            probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
                .unwrap();
        let second =
            probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
                .unwrap();
        assert_eq!(first, second, "Once bake retains every face/mip");
        for row in [0, 1, 2, 4] {
            for face in 0..6 {
                let pixel = probe::pixel(
                    &first,
                    (2 * face + 1) * probe::SIZE / 12,
                    (2 * row + 1) * probe::SIZE / 10,
                );
                for (channel, byte) in pixel.iter().take(3).enumerate() {
                    let radiance = if directional {
                        let axis = face / 2;
                        let sign = if face % 2 == 0 { 1.0 } else { -1.0 };
                        let factor = if row == 1 || row == 4 { 2.0 / 3.0 } else { 1.0 };
                        2.0 + if channel == axis as usize {
                            sign * factor
                        } else {
                            0.0
                        }
                    } else {
                        2.0 + channel as f32
                    };
                    let actual = f32::from(*byte) / 255.0 * 8.0;
                    assert!((actual - radiance).abs() < 0.06, "directional={directional} row={row} face={face} channel={channel}: {actual} != {radiance}");
                }
            }
        }
        assert_eq!(harness.renderer.pass_run_count("probe radiance 0"), Some(1));
        assert_eq!(
            harness.renderer.pass_run_count("probe specular 5/2"),
            Some(1)
        );
        assert_eq!(
            harness.renderer.pass_param("probe specular 5/2", "face"),
            Some(wxsl::core::node::Value::U32(5))
        );
        assert_eq!(
            harness
                .renderer
                .pass_param("probe specular 5/2", "roughness"),
            Some(wxsl::core::node::Value::F32(1.0))
        );
    }
    // Inspect interiors and edges, not only cardinal face centres. This also
    // crosses the equirectangular longitude seam on the -X face.
    harness
        .renderer
        .set_pass_param(
            "view IBL",
            "directional_uv",
            wxsl::core::node::Value::U32(1),
        )
        .unwrap();
    let image = probe::render_list(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &wxsl::render::DrawList::new(),
    )
    .unwrap();
    for y in 0..12 {
        for x in 0..probe::SIZE {
            let u = (x as f32 + 0.5) / probe::SIZE as f32 * 6.0;
            let v = (y as f32 + 0.5) / probe::SIZE as f32 * 5.0;
            let a = u.fract() * 2.0 - 1.0;
            let b = v.fract() * 2.0 - 1.0;
            let d = match u as u32 {
                0 => glam::Vec3::new(1.0, -b, -a),
                1 => glam::Vec3::new(-1.0, -b, a),
                2 => glam::Vec3::new(a, 1.0, b),
                3 => glam::Vec3::new(a, -1.0, -b),
                4 => glam::Vec3::new(a, -b, 1.0),
                _ => glam::Vec3::new(-a, -b, -1.0),
            }
            .normalize();
            let pixel = probe::pixel(&image, x, y);
            for (channel, byte) in pixel.iter().take(3).enumerate() {
                assert!(
                    (f32::from(*byte) / 255.0 * 8.0 - (2.0 + d[channel])).abs() < 0.04,
                    "face interior/seam at {x},{y}: {pixel:?}, direction {d}"
                );
            }
        }
    }
    // A bad authored block is refused transactionally, before any graph lands.
    let mut invalid = wxsl::render::graph::RenderGraph::new(harness.target.format());
    invalid.pass(
        wxsl::render::pass::PassDesc::screen("bad initial", wxsl::render::effect::ibl::SPECULAR.id)
            .with_parameter("face", wxsl::core::node::Value::F32(1.0)),
    );
    assert!(harness
        .renderer
        .set_graph(invalid)
        .unwrap_err()
        .to_string()
        .contains("face"));
    assert_eq!(
        harness.renderer.pass_param("probe specular 5/2", "face"),
        Some(wxsl::core::node::Value::U32(5))
    );
    harness
        .renderer
        .set_pass_param(
            "probe specular 5/2",
            "roughness",
            wxsl::core::node::Value::F32(0.3),
        )
        .unwrap();
    harness
        .renderer
        .set_graph(fixture::plan(harness.target.format(), true))
        .unwrap();
    assert_eq!(
        harness
            .renderer
            .pass_param("probe specular 5/2", "roughness"),
        Some(wxsl::core::node::Value::F32(0.3)),
        "live tuning survives the same label/layout"
    );
}
