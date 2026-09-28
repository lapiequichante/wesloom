//! The capability contract, run
//! ([ADR 0044](../../docs/adr/0044-identity-versions-and-the-capability-check.md)).
//!
//! A scene document is checked against a render setup — a pipeline, its
//! effects, the config its pass lists are built under — *before anything
//! is built*, and every mismatch is named. The scene here
//! (`assets/scene_check.scene.json`) pins `wxsl_subsurface`; the deferred
//! preset without feature channels does not carry it, and the load says
//! so, by name, instead of building buffers and failing one material at a
//! time.
//!
//! The same run then publishes the setup's capabilities and, with
//! `--screenshot`, re-checks under the plan that *does* carry the channel
//! and renders the scene headless to `scene_check.png`.
//!
//! ```text
//! cargo run -p wxsl --example scene_check
//! cargo run -p wxsl --example scene_check -- --screenshot
//! ```

use std::error::Error;
use std::path::Path;

use wxsl::core::lighting::{feature_requests, LightingSet};
use wxsl::core::node::NodeRegistry;
use wxsl::core::scene::Scene;
use wxsl::render::effect::EffectRegistry;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::setup::RenderSetup;
use wxsl::render::{Camera, Environment, PipelineConfig, RenderRequest, Renderer, TargetConfig};

const SIZE: u32 = 512;

struct Options {
    screenshot: bool,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options { screenshot: false };
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--screenshot" => options.screenshot = true,
            "--help" | "-h" => {
                print!(
                    "scene_check — the capability contract run: check a scene \
                     against a pipeline setup before anything is built\n\nflags:\n  \
                     --screenshot   also load and render the matched scene to \
                     scene_check.png\n"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}` (--help lists them)")),
        }
    }
    Ok(options)
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;

    // The scene document: on disk, versioned, pinning the ABI revision it
    // was written against.
    let scene: Scene = serde_json::from_str(include_str!("../assets/scene_check.scene.json"))?;
    println!("scene document: `{}`", scene.name);

    let registry: NodeRegistry = wxsl::stdlib::registry();
    let effects = EffectRegistry::shipped();
    let config = TargetConfig::new(SIZE, SIZE, wgpu::TextureFormat::Bgra8Unorm);

    // What the deferred preset provides, published as one value.
    let setup = RenderSetup::for_stock(
        wxsl::render::StockPipeline::Deferred,
        effects,
        PipelineConfig::new(config),
    );
    let capabilities = setup.capabilities()?;
    println!(
        "capabilities: stages [{}], lighting [{}], effects [{}], plan {} bytes/sample",
        capabilities.stages.join(", "),
        capabilities
            .lighting
            .models()
            .iter()
            .map(|model| model.name)
            .collect::<Vec<_>>()
            .join(", "),
        capabilities.effects.join(", "),
        wxsl::render::pipeline::gbuffer_layout_bytes_per_sample(capabilities.plan.layout()),
    );

    // The check: the scene pins a feature the plan does not carry, and the
    // report says so by name — the same report the load itself runs before
    // building anything.
    let report = setup.check(&scene, &registry);
    println!(
        "check against the plan without features: {} mismatch(es)",
        report.len()
    );
    for incompatibility in &report {
        println!("  - {incompatibility}");
    }

    // A device is first needed here — the check ran entirely without one.
    let gpu = pollster::block_on(GpuContext::headless())?;

    // And the load, given the same scene under the same plan, refuses
    // before any buffer exists.
    let error = match wxsl::scene::SceneResources::load_with_plan(
        &gpu.device,
        &scene,
        &registry,
        None,
        &LightingSet::default(),
        &[],
    ) {
        Ok(_) => return Err("expected the load to refuse the scene; it loaded".into()),
        Err(error) => error,
    };
    println!("load without features:\n  {error}");

    if options.screenshot {
        render(&gpu, &scene, &registry)?;
    }
    Ok(())
}

fn render(gpu: &GpuContext, scene: &Scene, registry: &NodeRegistry) -> Result<(), Box<dyn Error>> {
    let features = feature_requests(&["subsurface"])?;
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )?;
    renderer.set_pipeline(wxsl::render::StockPipeline::Deferred);
    renderer.set_features(&["subsurface"])?;

    // The matched plan carries the channel; the same document loads and
    // the frame renders.
    let mut resources = wxsl::scene::SceneResources::load_with_plan(
        &gpu.device,
        scene,
        registry,
        None,
        renderer.lighting(),
        &features,
    )?;
    resources.create_bindings(&gpu.device, &mut renderer);
    resources.upload(&gpu.device, &gpu.queue)?;

    let environment = Environment {
        camera: Camera {
            eye: glam::Vec3::new(0.0, 0.6, 3.2),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: vec![wxsl::render::Light::directional(
            glam::Vec3::new(0.3, 0.8, 1.0),
            glam::Vec3::ONE,
            3.0,
        )],
        ..Environment::default()
    };
    renderer.render(
        &gpu.device,
        &gpu.queue,
        &RenderRequest {
            view: target.view(),
            environment: &environment,
            draws: &resources.draw_list(),
        },
    )?;
    gpu.wait();

    let pixels = target.read_rgba8(&gpu.device, &gpu.queue);
    let file = Path::new("scene_check.png");
    image::save_buffer(file, &pixels, SIZE, SIZE, image::ExtendedColorType::Rgba8)?;
    println!("rendered the matched scene -> {}", file.display());
    Ok(())
}
