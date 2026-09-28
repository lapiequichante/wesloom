//! Semantic channels end to end, on the GPU (plan2 P12, ADR 0037).
//!
//! The device-free half — the plan, its collisions, the generated pack and
//! the lighting pass's bindings — is tested in `wxsl-core` and
//! `wxsl-render`. This is the handshake the plan feeds: a material that
//! pins a feature's macro *demands* its channel of the pipeline, a
//! pipeline without the channel is a named error rather than a silently
//! missing feature, and a pipeline that carries it draws the material.
//!
//! Since ADR 0038 the demand is named at the *resolution* point — where
//! the material is compiled against the plan — rather than at the first
//! frame, so the first of the three facts below is a compile error and the
//! second (a material resolved against another plan) is still the frame's.

use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::core::node::{NodeRegistry, Value, ValueType};
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::material::MaterialConfig;
use wxsl::render::setup::Incompatibility;
use wxsl::render::{DrawItem, Material, RenderError, TargetConfig};
use wxsl::scene::LoadError;

mod probe;

use probe::{declare_as_parameter, gpu, pixel, render_list_in, unlit, SIZE};

/// The macros of a material that turns subsurface on.
fn subsurface_macros() -> MacroSet {
    let mut macros = MacroSet::new();
    macros.set("wxsl_subsurface", MacroValue::Flag(true));
    macros
}

#[test]
fn a_feature_pinning_material_is_matched_or_named() {
    let Some(gpu) = gpu() else { return };
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("renderer");
    renderer.set_pipeline(wxsl::render::StockPipeline::Deferred);

    let registry: NodeRegistry = wxsl::stdlib::registry();
    let graph = probe::probe_graph(
        &registry,
        ValueType::F32,
        Value::F32(0.375),
        &declare_as_parameter,
    );
    let mesh = wxsl::render::Mesh::plane(&gpu.device, 2.0);

    // A material demanding the subsurface channel with its macro pin, but
    // resolved against a plan that carries no feature channels — the
    // pipeline's default. The demand goes unanswered, which is named where
    // the material is resolved rather than silently shaded without the
    // feature.
    {
        let error = match Material::from_graph_with_macros(&graph, &registry, &subsurface_macros())
        {
            Err(RenderError::Lighting { error, .. }) => error,
            other => panic!("expected the demand to be named, got {other:?}"),
        };
        assert!(
            error.contains("wxsl_subsurface") && error.contains("subsurface"),
            "the error names the macro and the feature: {error}"
        );
    }

    // A material that demands nothing still belongs to the plan it was
    // resolved against: compiled for the empty one, it cannot be drawn
    // under a pipeline that carries the channel, because its G-buffer
    // struct has one field fewer than the pass writes.
    let stranger =
        Material::from_graph(&graph, &registry).expect("compiles against the empty plan");
    renderer
        .set_features(&["subsurface"])
        .expect("the feature fits the budget");
    {
        let mut bindings = renderer.material_bindings(&gpu.device, &stranger);
        bindings
            .upload(&gpu.device, &gpu.queue)
            .expect("the parameters upload");
        let item = DrawItem::new(&mesh, &stranger)
            .with_transform(glam::Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2))
            .with_bindings(&bindings);
        let draws = wxsl::render::single_draw(item);
        let error = match render_list_in(&gpu, &mut renderer, &target, &draws, &unlit()) {
            Err(RenderError::Lighting { error, .. }) => error,
            other => panic!("expected the plan mismatch to be named, got {other:?}"),
        };
        assert!(
            error.contains("feature channels"),
            "the plan mismatch is named: {error}"
        );
    }

    // The honest spelling: the material resolved against the same plan it
    // is drawn under. The G-buffer grows the channel, the pack fills it,
    // and the frame renders.
    let features = wxsl::core::lighting::feature_requests(&["subsurface"]).unwrap();
    let material = Material::with_lighting(
        &graph,
        &registry,
        &MaterialConfig {
            macros: subsurface_macros(),
            features,
            ..MaterialConfig::default()
        },
        renderer.lighting(),
    )
    .expect("compiles against the widened plan");
    let mut bindings = renderer.material_bindings(&gpu.device, &material);
    bindings
        .upload(&gpu.device, &gpu.queue)
        .expect("the parameters upload");
    let item = DrawItem::new(&mesh, &material)
        .with_transform(glam::Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2))
        .with_bindings(&bindings);
    let draws = wxsl::render::single_draw(item);
    let image = render_list_in(&gpu, &mut renderer, &target, &draws, &unlit())
        .expect("the matched frame renders");
    let centre = pixel(&image, SIZE / 2, SIZE / 2);
    assert!(
        centre[0] > 20 || centre[1] > 20 || centre[2] > 20,
        "the surface shades: {centre:?}"
    );
}

/// The load-time half of the same contract
/// ([ADR 0044](../../../docs/adr/0044-identity-versions-and-the-capability-check.md)):
/// a scene authored against a plan is checked before anything is built, so
/// a feature its material pins but the plan does not carry is refused at
/// load, by name — not discovered one material per attempt at the compile
/// step, and not at the first frame.
#[test]
fn a_scene_that_needs_a_missing_channel_says_so_at_load() {
    let Some(gpu) = gpu() else { return };
    let registry: NodeRegistry = wxsl::stdlib::registry();
    let graph = probe::probe_graph(
        &registry,
        ValueType::F32,
        Value::F32(0.375),
        &declare_as_parameter,
    );

    let mut scene = wxsl::core::scene::Scene::new("subsurface");
    scene.add_mesh(wxsl::core::scene::MeshEntry::new(
        "cube",
        wxsl::core::scene::MeshSource::Cube { size: 1.0 },
    ));
    let mut paint = wxsl::core::scene::MaterialEntry::new("skin", graph);
    paint.config.macros = subsurface_macros();
    scene.add_material(paint);
    scene.add_instance(wxsl::core::scene::Instance::new(0, 0));

    let lighting = wxsl::core::lighting::LightingSet::default();

    // The plan without the channel: the whole scene is refused at once.
    let Err(error) = wxsl::scene::SceneResources::load_with_plan(
        &gpu.device,
        &scene,
        &registry,
        None,
        &lighting,
        &[],
    ) else {
        panic!("expected the load to refuse the scene; it loaded instead");
    };
    let LoadError::Incompatible(found) = error else {
        panic!("expected the mismatch to be reported, got {error}");
    };
    assert_eq!(found.len(), 1, "one mismatch, named completely: {found:?}");
    assert!(matches!(
        &found[0],
        Incompatibility::Feature { material, feature, .. }
            if material == "skin" && feature == &"subsurface"
    ));

    // The same scene under the plan that carries the channel loads.
    let features =
        wxsl::core::lighting::feature_requests(&["subsurface"]).expect("subsurface ships");
    wxsl::scene::SceneResources::load_with_plan(
        &gpu.device,
        &scene,
        &registry,
        None,
        &lighting,
        &features,
    )
    .expect("the matched scene loads, and only now builds");
}
