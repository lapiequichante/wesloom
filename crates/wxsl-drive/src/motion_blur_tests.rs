//! GPU regression: normal-map lighting and world shadows blur, while a
//! stationary foreground and the car's isolated contact occlusion stay put.
use super::*;
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::{DrawList, RenderGraph, TargetConfig};

#[path = "../../wxsl/tests/probe/mod.rs"]
mod probe;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;

#[test]
fn blur_filters_normal_lighting_and_world_shadows_without_smearing_the_car() {
    let Some(gpu) = probe::gpu() else { return };
    let target = OffscreenTarget::new(&gpu.device, WIDTH, HEIGHT);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(WIDTH, HEIGHT, target.format()),
    )
    .unwrap();
    renderer.add_effect(MOTION_BLUR_EFFECT.clone());
    // An analytic normal map under a directional light. Its period also
    // catches sparse gathers: the old 12 taps repeatedly hit the same phase.
    let fixture = Effect {
        id: "probe.drive_blur",
        label: "drive blur fixture",
        description: "Normal lighting, a world shadow, a foreground and car contact occlusion.",
        kind: EffectKind::Screen {
            vertex_entry: "vs",
            fragment_entry: "fs",
        },
        inputs: &[],
        outputs: &[],
        parameters: &[EffectParameter {
            name: "kind",
            default: Value::F32(0.0),
        }],
        shader: EffectShader::Source {
            path: "package::probe::drive_blur",
            wxsl: r#"
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return vec4f(f32(i32(i & 1u) * 4 - 1), f32(i32(i >> 1u) * 4 - 1), 0.0, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4f) -> @location(0) vec4f {
    let contact = select(1.0, 0.3, p.y >= 64.0 && p.y < 96.0);
    if params.kind > 1.5 { return vec4f(vec3f(contact), 1.0); }
    if params.kind > 0.5 { return vec4f(select(0.75, 0.0, p.x < 32.0), 0.0, 0.0, 1.0); }
    if p.x < 32.0 { return vec4f(0.9, 0.9, 0.9, 1.0); }
    let angle = p.x * 6.2831853 * 11.0 / 96.0;
    let normal = normalize(vec3f(sin(angle), 0.0, 1.0));
    let lighting = 0.1 + 0.6 * max(dot(normal, normalize(vec3f(1.0, 0.0, 1.0))), 0.0);
    let world_shadow = select(0.15, 0.7, p.x >= 128.0);
    return vec4f(vec3f(lighting, world_shadow, 0.6) * contact, 1.0);
}
"#,
        },
        downlevel: Downlevel::Portable,
    };
    renderer.add_effect(fixture);
    let mut graph = RenderGraph::new(target.format());
    let resources = ["color", "velocity", "contact"]
        .map(|label| graph.resource(ResourceDesc::color(label, TextureFormat::Rgba16Float)));
    for (kind, into) in resources.into_iter().enumerate() {
        graph.pass(
            PassDesc::screen(format!("fixture {kind}"), "probe.drive_blur")
                .with_color(Attachment::clear(into, Color::TRANSPARENT))
                .with_parameter("kind", Value::F32(kind as f32)),
        );
    }
    graph.pass(
        PassDesc::screen("blur", MOTION_BLUR_EFFECT.id)
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::TRANSPARENT))
            .with_reads(resources.map(Read::current))
            .with_parameter("strength", Value::F32(0.0)),
    );
    renderer.set_graph(graph).unwrap();
    let mut render = |strength| {
        renderer
            .set_pass_param("blur", "strength", Value::F32(strength))
            .unwrap();
        probe::render_list_in(
            &gpu,
            &mut renderer,
            &target,
            &DrawList::new(),
            &probe::unlit(),
        )
        .unwrap()
    };
    let raw = render(0.0);
    let blurred = render(1.0);
    let channel =
        |image: &[u8], x: u32, y: u32, c: usize| image[((y * WIDTH + x) * 4) as usize + c] as f32;
    let energy = |image: &[u8]| {
        (150..235)
            .map(|x| (channel(image, x + 1, 32, 0) - channel(image, x, 32, 0)).abs())
            .sum::<f32>()
    };
    assert!(
        energy(&raw) > 500.0,
        "normal lighting must have visible fine detail"
    );
    assert!(
        energy(&blurred) < energy(&raw) * 0.15,
        "normal lighting must lose its sharp detail along motion"
    );
    let shadow = channel(&blurred, 160, 32, 1);
    assert!(
        shadow > channel(&raw, 100, 32, 1) + 20.0 && shadow < channel(&raw, 160, 32, 1) - 20.0,
        "the stationary world shadow must smear with the road"
    );
    for x in [0, 16, 31] {
        for c in 0..3 {
            assert_eq!(
                channel(&blurred, x, 32, c),
                channel(&raw, x, 32, c),
                "the car stays sharp"
            );
        }
    }
    for x in [150, 200, 255] {
        let ratio = channel(&blurred, x, 80, 2) / channel(&blurred, x, 32, 2);
        assert!(
            (ratio - 0.3).abs() < 0.02,
            "contact occlusion stays in place: {ratio}"
        );
    }
    assert!(
        (channel(&blurred, 48, 32, 2) - channel(&raw, 48, 32, 2)).abs() <= 1.0,
        "the road must not collect the stationary foreground's colour"
    );
}
