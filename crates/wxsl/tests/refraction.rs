//! Optical transmission is layered geometry, not alpha compositing (ADR 0068).
mod probe;

use glam::{Mat4, Vec3};
use wxsl::core::{
    graph::Graph,
    node::Value,
    scene::{TagExpr, Tags},
};
use wxsl::render::{
    effect,
    material::MaterialConfig,
    pass::{DrawSource, PassKind, Read, ResourceDesc},
    types::TextureFormat,
    Camera, DrawItem, DrawList, Environment, Material, Mesh, RenderRequest, StockPipeline,
};

#[test]
fn optical_effects_compile_without_a_device() {
    for effect in [
        effect::refraction::BOUNDS,
        effect::refraction::RESOLVE,
        effect::refraction::SCENE_DEPTH,
        effect::refraction::COMPOSE,
        effect::refraction::COMPOSE_DEPTH,
    ] {
        wxsl::render::variants::EffectRequest::new(
            effect,
            &Default::default(),
            &Default::default(),
            &[],
        )
        .compile(&wxsl::stdlib_library())
        .1
        .unwrap();
    }
}

fn glass(h: &probe::Harness, ior: f32, tint: [f32; 3]) -> Material {
    let mut graph = Graph::new("test glass");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "ior", Value::F32(ior));
    graph.set_param(out, "base_color", Value::Vec3(tint));
    Material::with_config(
        &graph,
        &h.registry,
        &MaterialConfig::default()
            .with_shadows(false, false)
            .with_tags(Tags::from_iter(["refractive"])),
    )
    .unwrap()
}

/// Thin-walled (`thickness` 0) or solid glass with a transmission factor
/// (ADR 0093).
fn sheet(h: &probe::Harness, tint: [f32; 3], transmission: f32, thickness: f32) -> Material {
    let mut graph = Graph::new("test sheet");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "ior", Value::F32(1.5));
    graph.set_param(out, "base_color", Value::Vec3(tint));
    graph.set_param(out, "transmission", Value::F32(transmission));
    graph.set_param(out, "thickness", Value::F32(thickness));
    Material::with_config(
        &graph,
        &h.registry,
        &MaterialConfig::default()
            .with_shadows(false, false)
            .with_tags(Tags::from_iter(["refractive"])),
    )
    .unwrap()
}

fn setup(h: &mut probe::Harness) {
    setup_count(h, 3);
}

fn setup_count(h: &mut probe::Harness, surface_count: u32) {
    h.renderer.set_pipeline(StockPipeline::Forward);
    let mut graph = h.renderer.render_graph().clone();
    for i in 0..graph.passes().len() {
        if let PassKind::Geometry { source, .. } = &mut graph.pass_mut(i).unwrap().kind {
            *source = DrawSource::Scene(TagExpr::parse("!refractive").unwrap());
        }
    }
    let hdr = graph.resource(ResourceDesc::imported(
        "test HDR",
        TextureFormat::Rgba32Float,
    ));
    let maps = effect::ibl::append_filtered_bake(&mut graph, hdr, "test IBL", 32, 4, 6);
    let tonemap = graph
        .passes()
        .iter()
        .position(|p| matches!(&p.kind, PassKind::Screen { effect } if effect == "wxsl.tonemap"))
        .unwrap();
    let color = graph.passes()[tonemap].reads[0].resource;
    let depth = graph
        .passes()
        .iter()
        .filter_map(|p| p.depth.as_ref().map(|d| d.resource))
        .next_back()
        .unwrap();
    let background =
        effect::ibl::append_background(&mut graph, color, depth, maps.radiance, "background");
    let optical = effect::refraction::append(
        &mut graph,
        background,
        depth,
        maps.radiance,
        "optical",
        surface_count,
    );
    graph.pass_mut(tonemap).unwrap().reads[0] = Read::current(optical);
    h.renderer.set_graph(graph).unwrap();
    // A deterministic coloured horizon and longitude bars, no local HDR dependency.
    let pixels: Vec<_> = (0..128)
        .flat_map(|y| {
            (0..256).map(move |x| {
                let top = y < 64;
                let bar = if (x / 16) % 2 == 0 { 0.8 } else { 0.15 };
                [
                    if top { bar } else { 0.08 },
                    0.2,
                    if top { 0.05 } else { bar },
                ]
            })
        })
        .collect();
    let image = wxsl::render::environment::upload_environment_image(
        &h.gpu.device,
        &h.gpu.queue,
        256,
        128,
        &pixels,
    )
    .unwrap();
    h.renderer.import_resource("test HDR", image.view);
}

fn render(h: &mut probe::Harness, draws: &DrawList<'_>) -> Vec<u8> {
    let env = Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 0.0, 6.8),
            target: Vec3::ZERO,
            aspect: 1.0,
            ..Default::default()
        },
        lights: vec![],
        ambient_sky: Vec3::ZERO,
        ambient_ground: Vec3::ZERO,
        ..Default::default()
    };
    h.renderer
        .render(
            &h.gpu.device,
            &h.gpu.queue,
            &RenderRequest {
                view: h.target.view(),
                environment: &env,
                draws,
            },
        )
        .unwrap();
    h.gpu.wait();
    h.target.read_rgba8(&h.gpu.device, &h.gpu.queue)
}

fn changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(a, b)| (0..3).any(|i| a[i].abs_diff(b[i]) > 10))
        .count()
}

#[test]
fn two_consecutive_solids_refract_independently_and_ignore_submission_order() {
    let Some(gpu) = probe::gpu() else { return };
    let mut h = probe::Harness::new(gpu);
    setup(&mut h);
    let sphere = Mesh::sphere(&h.gpu.device, 0.85);
    let neutral = glass(&h, 1.0, [1.0; 3]);
    let optical = glass(&h, 1.5, [1.0; 3]);
    let smoke = glass(&h, 1.5, [0.65, 0.8, 0.95]);
    let background = render(&mut h, &DrawList::new());
    let transforms = [
        Mat4::from_translation(Vec3::new(-0.15, 0.0, 1.1)),
        Mat4::from_translation(Vec3::new(0.15, 0.0, -1.1)),
    ];
    let draws = |near, far, reverse: bool| {
        let mut items = [
            DrawItem::new(&sphere, near).with_transform(transforms[0]),
            DrawItem::new(&sphere, far).with_transform(transforms[1]),
        ];
        if reverse {
            items.reverse();
        }
        items.into_iter().collect::<DrawList>()
    };
    let identity = render(&mut h, &draws(&neutral, &neutral, false));
    assert_eq!(
        changed(&identity, &background),
        0,
        "IOR=1 is an optical identity"
    );
    let near_only = render(&mut h, &draws(&optical, &neutral, false));
    let both = render(&mut h, &draws(&optical, &optical, false));
    assert!(
        changed(&near_only, &identity) > 40,
        "near sphere must bend HDR"
    );
    assert!(
        changed(&both, &near_only) > 20,
        "far sphere must modify already refracted rays"
    );
    assert_eq!(both, render(&mut h, &draws(&optical, &optical, true)));
    let smoked = render(&mut h, &draws(&optical, &smoke, false));
    assert!(
        changed(&both, &smoked) > 20,
        "absorption must be optical path dependent"
    );
    assert_eq!(
        smoked,
        render(&mut h, &draws(&optical, &smoke, false)),
        "no temporal noise"
    );
}

#[test]
fn opaque_depth_hides_glass_before_it_can_change_the_background() {
    let Some(gpu) = probe::gpu() else { return };
    let mut h = probe::Harness::new(gpu);
    setup(&mut h);
    let sphere = Mesh::sphere(&h.gpu.device, 0.85);
    let optical = glass(&h, 1.5, [0.6, 0.8, 1.0]);
    let cube = Mesh::cube(&h.gpu.device, 2.4);
    let mut graph = Graph::new("opaque green");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "base_color", Value::Vec3([0.0; 3]));
    graph.set_param(out, "emissive", Value::Vec3([0.0, 0.6, 0.0]));
    let opaque = h.material(&graph);
    let front = || {
        DrawItem::new(&cube, &opaque)
            .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0)))
    };
    let baseline = render(&mut h, &std::iter::once(front()).collect());
    let with_glass = render(
        &mut h,
        &[front(), DrawItem::new(&sphere, &optical)]
            .into_iter()
            .collect(),
    );
    assert_eq!(baseline, with_glass);
}

#[test]
fn a_thicker_torus_absorbs_more_red_than_blue() {
    let Some(gpu) = probe::gpu() else { return };
    let mut h = probe::Harness::new(gpu);
    setup(&mut h);
    let white = wxsl::render::environment::upload_environment_image(
        &h.gpu.device,
        &h.gpu.queue,
        1,
        1,
        &[[0.8; 3]],
    )
    .unwrap();
    h.renderer.import_resource("test HDR", white.view);
    let smoke = glass(&h, 1.5, [0.18, 0.45, 0.9]);
    let mut samples = Vec::new();
    for thickness in [0.2, 0.55] {
        let mesh = Mesh::torus(&h.gpu.device, 1.15, thickness);
        // Face the ring towards the camera; sample the tube's centre chord.
        let draws = std::iter::once(
            DrawItem::new(&mesh, &smoke)
                .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        )
        .collect();
        let image = render(&mut h, &draws);
        let mut rgb = [0.0; 3];
        for y in 30..34 {
            for x in 44..47 {
                let pixel = probe::pixel(&image, x, y);
                for i in 0..3 {
                    rgb[i] += pixel[i] as f32 / 12.0;
                }
            }
        }
        samples.push(rgb);
    }
    assert!(
        samples[1][0] + 15.0 < samples[0][0],
        "thicker glass must absorb more red: {samples:?}"
    );
    assert!(
        samples[1][2] / samples[1][0] > samples[0][2] / samples[0][0] + 0.2,
        "blue/red ratio must grow with optical distance: {samples:?}"
    );
}

#[test]
fn a_torus_and_a_sphere_need_three_crossings_six_interfaces() {
    let Some(gpu) = probe::gpu() else { return };
    let mut h = probe::Harness::new(gpu);
    let torus = Mesh::torus(&h.gpu.device, 1.15, 0.38);
    let sphere = Mesh::sphere(&h.gpu.device, 0.85);
    let clear = glass(&h, 1.5, [1.0; 3]);
    let blue = glass(&h, 1.5, [0.18, 0.45, 0.9]);
    // Viewed edge-on, the ray crosses both tube sections, then the sphere.
    let draws = [
        DrawItem::new(&torus, &clear),
        DrawItem::new(&sphere, &blue)
            .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, -3.0))),
    ]
    .into_iter()
    .collect();
    setup_count(&mut h, 2);
    let truncated = render(&mut h, &draws);
    setup_count(&mut h, 3);
    let complete = render(&mut h, &draws);
    assert!(
        changed(&truncated, &complete) > 20,
        "the sphere beyond the two torus crossings must remain in the peel"
    );
}

#[test]
fn peeled_torus_normals_alternate_entry_exit_on_all_four_interfaces() {
    use wxsl::render::effect::{
        Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
    };
    let Some(gpu) = probe::gpu() else { return };
    let mut h = probe::Harness::new(gpu);
    setup_count(&mut h, 2);
    let torus = Mesh::torus(&h.gpu.device, 1.15, 0.38);
    let clear = glass(&h, 1.5, [1.0; 3]);
    let draws = std::iter::once(DrawItem::new(&torus, &clear)).collect();
    h.renderer.add_effect(Effect {
        id: "test.optical_facing", label: "facing", description: "Measure interface orientation", kind: EffectKind::Screen { vertex_entry: "vertex", fragment_entry: "fragment" },
        inputs: &[EffectInput { name: "image", kind: EffectInputKind::Image, description: "normal/depth array", history: 0 }], outputs: &[],
        parameters: &[EffectParameter { name: "layer", default: Value::U32(0) }],
        shader: EffectShader::Source { path: "test::optical_facing", wxsl: "import package::wxsl::bindings::{camera};\n@group(3) @binding(0) var image: texture_2d_array<f32>;\n@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { return vec4f(f32(i32(i & 1u) * 4 - 1), f32(i32(i >> 1u) * 4 - 1), 0.0, 1.0); }\n@fragment fn fragment(@builtin(position) p: vec4f) -> @location(0) vec4f { let n = textureLoad(image, vec2i(p.xy), i32(params.layer), 0); if dot(n.xyz, n.xyz) < 0.5 { return vec4f(0.5, 0.5, 0.5, 1.0); } let uv = p.xy / vec2f(textureDimensions(image)); let h = camera.inverse_view_proj * vec4f(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, n.w, 1.0); let facing = dot(n.xyz, normalize(camera.position.xyz - h.xyz / h.w)); return select(vec4f(0.0, 1.0, 0.0, 1.0), vec4f(1.0, 0.0, 0.0, 1.0), (facing > 0.0)); }" },
        downlevel: wxsl::render::effect::Downlevel::Portable,
    });
    let mut graph = h.renderer.render_graph().clone();
    let normals = graph.resource_by_label("optical normals").unwrap();
    let tone = graph
        .passes()
        .iter()
        .position(|p| matches!(&p.kind, PassKind::Screen { effect } if effect == "wxsl.tonemap"))
        .unwrap();
    graph.pass_mut(tone).unwrap().kind = PassKind::Screen {
        effect: "test.optical_facing".into(),
    };
    graph.pass_mut(tone).unwrap().reads = vec![Read::current(normals)];
    h.renderer.set_graph(graph).unwrap();
    for layer in 0..4 {
        h.renderer
            .set_pass_param("tonemap", "layer", Value::U32(layer))
            .unwrap();
        let pixels = render(&mut h, &draws);
        let front = pixels
            .chunks_exact(4)
            .filter(|p| p[0] == 255 && p[1] == 0)
            .count();
        let back = pixels
            .chunks_exact(4)
            .filter(|p| p[0] == 0 && p[1] == 255)
            .count();
        let (correct, wrong) = if layer % 2 == 0 {
            (front, back)
        } else {
            (back, front)
        };
        assert!(
            correct > 10,
            "layer {layer} was not captured: front={front}, back={back}"
        );
        // Interpolated shading normals can disagree with a polygon's winding
        // at a few grazing silhouette texels, not across an interface.
        assert!(
            wrong <= 4,
            "layer {layer} has reversed orientation: front={front}, back={back}"
        );
    }
}

#[test]
fn thin_walled_glass_tints_without_bending_and_transmission_mixes_in_its_base_colour() {
    let Some(gpu) = probe::gpu() else { return };
    let mut h = probe::Harness::new(gpu);
    setup(&mut h);
    let sphere = Mesh::sphere(&h.gpu.device, 0.85);
    let [solid, thin, red, milky, solid_milky] = [
        sheet(&h, [1.0; 3], 1.0, 1.0),
        sheet(&h, [1.0; 3], 1.0, 0.0),
        sheet(&h, [1.0, 0.1, 0.1], 1.0, 0.0),
        sheet(&h, [1.0, 0.1, 0.1], 0.0, 0.0),
        sheet(&h, [1.0; 3], 0.0, 1.0),
    ];
    let mut draw = |material| {
        let draws = std::iter::once(DrawItem::new(&sphere, material)).collect();
        render(&mut h, &draws)
    };
    let (solid, thin, red, milky, solid_milky) = (
        draw(&solid),
        draw(&thin),
        draw(&red),
        draw(&milky),
        draw(&solid_milky),
    );
    let background = render(&mut h, &DrawList::new());
    // Only Fresnel changes a clear thin sheet; a solid also bends the bars.
    let error = |image: &[u8]| -> u64 {
        image
            .iter()
            .zip(&background)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum()
    };
    assert!(
        error(&thin) * 3 < error(&solid),
        "thin {} solid {}",
        error(&thin),
        error(&solid)
    );
    let centre = probe::SIZE / 2;
    let (behind, tinted) = (
        probe::pixel(&background, centre, centre),
        probe::pixel(&red, centre, centre),
    );
    assert!(
        tinted[1] + 20 < behind[1] && tinted[0] + 20 > behind[0],
        "base colour tints thin glass: {behind:?} -> {tinted:?}"
    );
    assert!(
        changed(&milky, &red) > 40,
        "transmission 0 shows the base colour, not what is behind"
    );
    assert!(changed(&solid_milky, &solid) > 40);
}
