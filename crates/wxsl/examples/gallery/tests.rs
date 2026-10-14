//! The gallery's own checks.

#[test]
fn optical_quality_cli_is_explicit_and_defaults_to_layered() {
    assert_eq!(
        Options::parse(std::iter::empty())
            .unwrap()
            .refraction_quality,
        RefractionQuality::Layered
    );
    assert_eq!(
        Options::parse(
            ["--refraction-quality", "precise"]
                .into_iter()
                .map(String::from)
        )
        .unwrap()
        .refraction_quality,
        RefractionQuality::Precise
    );
    assert!(Options::parse(
        ["--refraction-quality", "unknown"]
            .into_iter()
            .map(String::from)
    )
    .is_err());
}
#[test]
#[ignore = "manual GPU timing comparison at 1280x720"]
fn glass_quality_timing() {
    use wxsl::render::pass::PassKind;
    let gpu = pollster::block_on(GpuContext::headless()).unwrap();
    let target = OffscreenTarget::new(&gpu.device, 1280, 720);
    let mut stage = Stage::new(gpu, TargetConfig::new(1280, 720, target.format()), "sky").unwrap();
    let demos = demos();
    let demo = demos.iter().find(|d| d.name == "glass").unwrap();
    for quality in [
        RefractionQuality::Layered,
        RefractionQuality::Precise,
        RefractionQuality::Layered,
    ] {
        stage.refraction_quality = quality;
        for _ in 0..2 {
            stage
                .render(demo, target.view(), 1280, 720, 0.6, 0.0)
                .unwrap();
            stage.gpu.wait();
        }
        let misses = stage.renderer.cache_stats().misses;
        let bake_runs = stage.renderer.pass_run_count("gallery sky");
        let started = Instant::now();
        for _ in 0..5 {
            stage
                .render(demo, target.view(), 1280, 720, 0.6, 0.0)
                .unwrap();
            stage.gpu.wait();
        }
        println!(
            "{quality:?}: {:.2} ms/frame",
            started.elapsed().as_secs_f64() * 200.0
        );
        assert_eq!(misses, stage.renderer.cache_stats().misses);
        assert_eq!(bake_runs, stage.renderer.pass_run_count("gallery sky"));
        assert_eq!(bake_runs, Some(1));
        assert_eq!(
            stage.renderer.render_graph().passes().iter().any(
                |p| matches!(&p.kind, PassKind::Screen { effect } if effect == "wxsl.refraction")
            ),
            quality == RefractionQuality::Precise
        );
    }
}
#[test]
fn glass_and_mirror_switch_optical_topology_without_reusing_the_other_plan() {
    let gpu = match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            return;
        }
    };
    let target = OffscreenTarget::new(&gpu.device, 192, 144);
    let mut stage = Stage::new(gpu, TargetConfig::new(192, 144, target.format()), "sky").unwrap();
    let demos = demos();
    let mut pictures = Vec::new();
    for name in ["ibl-mirror", "glass", "ibl-mirror", "glass"] {
        let demo = demos.iter().find(|demo| demo.name == name).unwrap();
        stage
            .render(demo, target.view(), 192, 144, 0.0, 0.0)
            .unwrap();
        assert_eq!(
            stage
                .renderer
                .render_graph()
                .resource_by_label("gallery glass color")
                .is_some(),
            name == "glass"
        );
        stage.gpu.wait();
        pictures.push(target.read_rgba8(&stage.gpu.device, &stage.gpu.queue));
    }
    assert_eq!(pictures[0], pictures[2]);
    assert_eq!(pictures[1], pictures[3]);
    assert_ne!(pictures[0], pictures[1]);
}
use super::*;

#[test]
fn orbit_preserves_authored_views_and_stays_finite_at_the_poles() {
    for demo in demos() {
        let authored = demo_environment(&demo, 1.7, 0.0).camera;
        let mut orbit = OrbitCamera::new(authored);
        let initial = orbit.view(1.7);
        assert!(
            (initial.eye - authored.eye).length() < 1e-5,
            "{}",
            demo.name
        );
        assert_eq!(initial.target, authored.target);
        orbit.orbit(200.0, 100000.0);
        orbit.zoom(100000.0);
        orbit.pan(20.0, -10.0, 720.0);
        assert!(orbit.view(1.7).view_proj().is_finite());
        assert_eq!(orbit.radius, 0.05);
        orbit.zoom(-100000.0);
        assert_eq!(orbit.radius, 90.0);
        assert!(orbit.view(1.7).view_proj().is_finite());
    }
}

#[test]
fn gallery_ground_receives_a_visible_shadow_on_both_paths() {
    let gpu = match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            return;
        }
    };
    let target = OffscreenTarget::new(&gpu.device, 192, 144);
    let mut stage = Stage::new(gpu, TargetConfig::new(192, 144, target.format()), "sky").unwrap();
    let registry = wxsl::stdlib::registry();
    let mut demo = demos()
        .into_iter()
        .find(|demo| demo.name == "shadow")
        .unwrap();
    let mut shadowed = Vec::new();
    for stock in [StockPipeline::Forward, StockPipeline::Deferred] {
        demo.pipeline = Pipeline::Stock(stock);
        let mut images = Vec::new();
        for receives in [true, false] {
            stage.shadow_floor.material = wxsl::render::Material::with_config(
                &ibl_grid_floor_graph(),
                &registry,
                &wxsl::render::material::MaterialConfig::default().with_shadows(true, receives),
            )
            .unwrap();
            stage
                .render(&demo, target.view(), 192, 144, 0.0, 0.0)
                .unwrap();
            stage.gpu.wait();
            images.push(target.read_rgba8(&stage.gpu.device, &stage.gpu.queue));
        }
        let darker = images[0]
            .chunks_exact(4)
            .zip(images[1].chunks_exact(4))
            .filter(|(shadow, lit)| lit[1] as i16 - shadow[1] as i16 > 15)
            .count();
        assert!(
            darker > 100,
            "{}: only {darker} shadow pixels",
            stock.name()
        );
        shadowed.push(images.remove(0));
    }
    let difference = shadowed[0]
        .iter()
        .zip(&shadowed[1])
        .map(|(a, b)| a.abs_diff(*b) as u64)
        .sum::<u64>();
    assert!(difference as f64 / (shadowed[0].len() as f64) < 1.0);
}

#[test]
fn grid_has_exact_independent_ranges_and_a_matte_grey_floor() {
    let samples = ibl_grid_samples();
    for (index, (position, roughness, metallic)) in samples.iter().enumerate() {
        assert_eq!(*roughness, (index % 5) as f32 / 4.0);
        assert_eq!(*metallic, (index / 5) as f32 / 4.0);
        assert_eq!(position.y, 0.85);
    }
    let registry = wxsl::stdlib::registry();
    let material =
        wxsl::render::Material::from_graph(&ibl_grid_material_graph(), &registry).unwrap();
    assert_eq!(material.interface().params.fields().len(), 2);
    assert!(
        material.interface().resources.is_empty(),
        "no texture aliasing in the diagnostic grid"
    );
    let floor = ibl_grid_floor_graph();
    floor.validate(&registry).unwrap();
    let output = floor
        .node(floor.outputs(&registry).unwrap().surface)
        .unwrap();
    assert_eq!(output.params["roughness"], Value::F32(1.0));
    assert_eq!(output.params["metallic"], Value::F32(0.0));
    assert_eq!(output.params["base_color"], Value::Vec3([0.3; 3]));
    let demo = demos()
        .into_iter()
        .find(|demo| demo.name == "ibl-grid")
        .unwrap();
    let environment = demo_environment(&demo, 4.0 / 3.0, 0.0);
    assert!(environment.lights.is_empty());
    assert_eq!(environment.ambient_sky, Vec3::ZERO);
    assert_eq!(environment.ambient_ground, Vec3::ZERO);
}

#[test]
fn grid_is_stable_and_switching_sources_cannot_reuse_the_wrong_environment() {
    let gpu = match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            return;
        }
    };
    let target = OffscreenTarget::new(&gpu.device, 192, 144);
    let mut stage = Stage::new(gpu, TargetConfig::new(192, 144, target.format()), "sky").unwrap();
    let demos = demos();
    let grid = demos.iter().find(|demo| demo.name == "ibl-grid").unwrap();
    let sky_grid = demos
        .iter()
        .find(|demo| demo.name == "ibl-grid-sky")
        .unwrap();
    // A small valid upload checks source-independent sky UVs without
    // depending on a user's local HDR file or its equirectangular extent.
    stage.ibl_source = wxsl::render::environment::upload_environment_image(
        &stage.gpu.device,
        &stage.gpu.queue,
        1,
        1,
        &[[1.5, 2.0, 3.0]],
    )
    .unwrap();
    stage.ibl_sky = false;
    let mut images = Vec::new();
    for demo in [grid, sky_grid, grid] {
        stage
            .render(demo, target.view(), 192, 144, 0.0, 0.0)
            .unwrap();
        stage.gpu.wait();
        let first = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        assert_eq!(
            stage
                .renderer
                .render_graph()
                .resource_by_label("gallery sky")
                .is_some(),
            demo.name == "ibl-grid-sky"
        );
        assert_eq!(stage.renderer.render_graph().environment_scale(), 1.0);
        for ((_, roughness, metallic), bindings) in
            ibl_grid_samples().iter().zip(&stage.grid_bindings)
        {
            assert_eq!(bindings.get("roughness"), Some(Value::F32(*roughness)));
            assert_eq!(bindings.get("metallic"), Some(Value::F32(*metallic)));
        }
        stage
            .render(demo, target.view(), 192, 144, 30.0, 0.0)
            .unwrap();
        stage.gpu.wait();
        let second = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        assert_eq!(
            first, second,
            "time must change neither material values nor environment"
        );
        assert!(
            mean_luminance(&first) > 0.02,
            "{} must illuminate the grid",
            demo.name
        );
        assert_eq!(
            stage.renderer.pass_run_count("gallery IBL specular 5/7"),
            Some(1)
        );
        if demo.name == "ibl-grid-sky" {
            assert_eq!(stage.renderer.pass_run_count("gallery sky"), Some(1));
        }
        images.push(first);
    }
    assert_ne!(
        images[0], images[1],
        "changing the source must change the light"
    );
    assert_eq!(
        images[0], images[2],
        "returning to the HDR must restore the same light"
    );
}
