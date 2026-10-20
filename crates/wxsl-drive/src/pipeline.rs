//! The frame: the stock forward document with a velocity pass, motion
//! blur and a bloom pyramid composed onto it as nodes, then, on the
//! compiled pass list, the
//! sky baked into the environment maps and drawn behind everything, the
//! car's glass, depth of field focused on the car, the tonemap and FXAA.
//!
//! ```text
//! shadows → prepass → forward ─┬─ background (sky) ─ glass ─ motion blur ─ bloom ─ dof ─ tonemap ─ fxaa
//!                               │   (the demo's own, `shaders/motion_blur.wxsl`)
//!                     velocity ┘
//! sky (once per hour) → IBL bake (once) ┘
//! ```
//!
//! The car (body, glass and driver) mirrors a probe at its centre rather
//! than the sky (ADR 0102): every frame, six forward passes of everything
//! but the car, filtered like the sky's maps. The forward pass draws the
//! rest of the world; a second one on the same targets draws the car with
//! the probe's cubes, and the glass refracts the probe's radiance. The
//! shadow passes draw the car's decimated stand-in instead of the car, and
//! no other pass draws it.
//!
//! Deferred, for comparison, starts from the stock deferred document
//! instead: the G-buffer pass stands where the prepass was, and its depth
//! feeds the velocity pass the same way, and the lighting pass lights the
//! car's body with the sky, since it lights every pixel at once; only its
//! glass reads the probe. Without FXAA, the tonemap presents.

use std::error::Error;

use wxsl::core::abi::MaterialStage;
use wxsl::core::graph::{Graph, Node, SocketRef};
use wxsl::core::pipeline as doc;
use wxsl::core::scene::TagExpr;
use wxsl::core::Value;
use wxsl::render::effect::{
    ibl, refraction, Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter,
    EffectShader,
};
use wxsl::render::pass::{
    Attachment, DrawSource, Extent, Load, PassDesc, PassKind, Policy, Read, ResourceDesc,
};
use wxsl::render::types::{Color, TextureFormat};
use wxsl::render::{Camera, Renderer, StockPipeline};

use crate::lights::{CAR_SHADOW_TAG, CAR_TAG};
use crate::sky::{SKY, SKY_HEIGHT, SKY_PASS, SKY_WIDTH};

/// The pass the lens' numbers go to.
const DOF_CIRCLES: &str = "drive dof circles";
/// The widest blur disc, in pixels.
const MAX_COC: f32 = 20.0;
/// The motion blur's pass label, which `strength` is set on.
pub const MOTION_BLUR: &str = "motion blur";
/// The sky's environment cubes: the edge of the radiance and specular
/// cubes, the diffuse cube's, and the specular mips. The car's probe has
/// the same, as a pass's own cubes must (ADR 0102).
const CUBE_SIZE: u32 = 128;
const DIFFUSE_SIZE: u32 = 16;
const SPECULAR_MIPS: u32 = 8;
/// The probe's label, and the importance samples of its sharp GGX lobes:
/// few enough for every frame, the filtered source's mips doing the rest.
const PROBE: &str = "drive probe";
const PROBE_SAMPLES: u32 = 64;

/// The bloom pyramid's settings: four levels for a faint, tight halo
/// around the lamps and the headlights, from radiance well above a lit
/// road's.
const BLOOM: [(&str, &str); 4] = [
    ("levels", "4"),
    ("threshold", "3"),
    ("knee", "2"),
    ("strength", "0.07"),
];

/// `wxsl.motion_blur`'s gather, refusing taps that did not move with the
/// pixel: the car the camera follows leaves no trail on the road.
const MOTION_BLUR_EFFECT: Effect = Effect {
    id: "drive.motion_blur",
    label: "motion blur",
    description: "Smear the frame along its velocity, without what stayed put in front of it.",
    kind: EffectKind::Screen {
        vertex_entry: "drive_motion_blur_vs",
        fragment_entry: "drive_motion_blur_fs",
    },
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The frame, as linear radiance.",
            history: 0,
        },
        EffectInput {
            name: "velocity",
            kind: EffectInputKind::Image,
            description: "The velocity stage's screen motion, uv per frame.",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "strength",
        default: Value::F32(1.0),
    }],
    shader: EffectShader::Source {
        path: "package::drive::motion_blur",
        wxsl: include_str!("../shaders/motion_blur.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The effects the frame names beyond the shipped ones.
pub fn add_effects(renderer: &mut Renderer) -> Result<(), Box<dyn Error>> {
    for effect in [
        ibl::EQUIRECT_TO_CUBE,
        ibl::DIFFUSE,
        ibl::SPECULAR,
        ibl::RESAMPLE,
        ibl::PROBE_FACE,
        SKY,
        MOTION_BLUR_EFFECT,
    ] {
        renderer.add_effect(effect);
    }
    renderer.add_effect(wxsl::effects::fxaa(&wxsl::stdlib::registry())?);
    Ok(())
}

/// The stock forward (or deferred) document with a velocity pass over the
/// prepass' (or the G-buffer's) depth, then a motion blur and a bloom
/// between the shaded colour and the tonemap.
fn motion_blur_document(renderer: &Renderer, deferred: bool) -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::render::document_registry(renderer.effects());
    let (stock, preset, depth_stage) = match deferred {
        true => (StockPipeline::Deferred, "deferred", "gbuffer"),
        false => (StockPipeline::Forward, "forward", "depth_only"),
    };
    let mut document = stock.document();
    document.set_name("drive");
    let find = |document: &Graph, test: &dyn Fn(&Node) -> bool, what: &str| {
        document
            .nodes()
            .find(|(_, node)| test(node))
            .map(|(id, _)| id)
            .ok_or_else(|| format!("the {preset} preset has no {what}"))
    };
    let scene = find(&document, &|node| node.def == doc::SOURCE_SCENE, "scene")?;
    let prepass = find(
        &document,
        &|node| {
            node.def == doc::PASS_GEOMETRY
                && node
                    .settings
                    .get(doc::SETTING_STAGE)
                    .is_some_and(|stage| stage.as_str() == depth_stage)
        },
        "depth pass",
    )?;
    let color = find(
        &document,
        &|node| node.def == doc::RESOURCE_COLOR,
        "colour target",
    )?;
    let tonemap = find(
        &document,
        &|node| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        },
        "tonemap",
    )?;
    let velocity = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let blurred = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("blurred")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let blur = document.add(
        Node::new(format!(
            "{}{}",
            doc::PASS_SCREEN_PREFIX,
            MOTION_BLUR_EFFECT.id
        ))
        .with_label(MOTION_BLUR),
    );
    let glowing = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("glowing")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bloom = BLOOM.iter().fold(
        Node::new(doc::PASS_BLOOM).with_label("drive bloom"),
        |node, (name, value)| node.with_setting(*name, *value),
    );
    let bloom = document.add(bloom);
    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((prepass, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((color, "color"), (blur, "color")),
        ((velocity, "color"), (blur, "velocity")),
        ((blurred, "color"), (blur, "into")),
        ((blurred, "color"), (bloom, "image")),
        ((glowing, "color"), (bloom, "into")),
        ((glowing, "color"), (tonemap, "image")),
    ] {
        document
            .wire(&registry, from, to)
            .map_err(|error| format!("motion blur wiring: {error}"))?;
    }
    Ok(document)
}

/// Build the frame, forward or `deferred`, with or without `fxaa`, and give
/// it to `renderer`. With `refractive`, the car's glass leaves the opaque
/// passes for the refraction tier (ADR 0067).
pub fn install(
    renderer: &mut Renderer,
    refractive: bool,
    deferred: bool,
    fxaa: bool,
) -> Result<(), Box<dyn Error>> {
    let document = motion_blur_document(renderer, deferred)?;
    let mut graph = wxsl::render::compile_pipeline(
        &document,
        &wxsl::render::document_registry(renderer.effects()),
        renderer.effects(),
        // The renderer's own: its lighting set shapes the G-buffer.
        renderer.pipeline_config(),
    )
    .map_err(|error| format!("the drive document does not compile: {error}"))?;
    if refractive {
        for index in 0..graph.passes().len() {
            if let Some(PassKind::Geometry { source, .. }) =
                graph.pass_mut(index).map(|pass| &mut pass.kind)
            {
                *source = DrawSource::Scene(TagExpr::parse("!refractive")?);
            }
        }
    }

    // The sky, baked into the environment maps whenever the hour changes
    // (`Renderer::invalidate_bakes`), and drawn behind the scene.
    let sky = graph.resource(
        ResourceDesc::color("drive sky", TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: SKY_WIDTH,
                height: SKY_HEIGHT,
            })
            .persistent(0),
    );
    graph.pass(
        PassDesc::screen(SKY_PASS, SKY.id)
            .with_color(Attachment::clear(sky, Color::BLACK))
            .with_policy(Policy::Once),
    );
    let maps = ibl::append_filtered_bake(
        &mut graph,
        sky,
        "drive IBL",
        CUBE_SIZE,
        DIFFUSE_SIZE,
        SPECULAR_MIPS,
    );
    graph.declare_environment_maps(maps.diffuse, maps.specular);
    graph.set_environment_scale(1.0);

    let effect_pass = |graph: &wxsl::render::RenderGraph, id: &str| {
        graph
            .passes()
            .iter()
            .position(|pass| matches!(&pass.kind, PassKind::Screen { effect } if effect == id))
            .ok_or_else(|| format!("the frame has no {id}"))
    };
    let blur = effect_pass(&graph, MOTION_BLUR_EFFECT.id)?;
    let tonemap = effect_pass(&graph, "wxsl.tonemap")?;
    let shaded = graph.passes()[blur].reads[0].resource;
    let depth = graph
        .passes()
        .iter()
        .filter_map(|pass| pass.depth.as_ref().map(|attachment| attachment.resource))
        .next_back()
        .ok_or("the frame has no depth")?;

    // The car's probe, after the lookups above: its faces have depth too.
    let probe = ibl::append_probe(
        &mut graph,
        TagExpr::parse(&format!("!{CAR_TAG}"))?,
        maps.radiance,
        PROBE,
        CUBE_SIZE,
        DIFFUSE_SIZE,
        SPECULAR_MIPS,
        PROBE_SAMPLES,
    );
    if !deferred {
        // The world, then the car on the same targets with the probe.
        let forward = graph
            .passes()
            .iter()
            .position(|pass| pass.label == "forward")
            .ok_or("the frame has no forward pass")?;
        let glass = if refractive { " && !refractive" } else { "" };
        let draw = |pass: &mut PassDesc, expression: &str| -> Result<(), Box<dyn Error>> {
            if let PassKind::Geometry { source, .. } = &mut pass.kind {
                *source = DrawSource::Scene(TagExpr::parse(expression)?);
            }
            Ok(())
        };
        let mut car = graph.passes()[forward].clone();
        draw(
            graph.pass_mut(forward).ok_or("forward pass")?,
            &format!("!{CAR_TAG}{glass}"),
        )?;
        car.label = "drive car".into();
        draw(&mut car, &format!("{CAR_TAG}{glass}"))?;
        for attachment in &mut car.color {
            attachment.load = Load::Load;
        }
        if let Some(depth) = &mut car.depth {
            depth.clear = None;
        }
        graph.pass(car.with_environment(probe.diffuse, probe.specular));
    }
    let mut background =
        ibl::append_background(&mut graph, shaded, depth, maps.radiance, "drive background");
    if refractive {
        background = refraction::append(
            &mut graph,
            background,
            depth,
            probe.radiance,
            "drive glass",
            2,
        );
    }
    graph.pass_mut(blur).ok_or("motion blur pass")?.reads[0] = Read::current(background);

    // Depth of field over the blurred, glowing image, before the tonemap.
    let hdr = |name: &str| ResourceDesc::color(name, TextureFormat::Rgba16Float);
    let blurred = graph.passes()[tonemap].reads[0].resource;
    let coc = graph.resource(hdr("drive dof coc"));
    let far = graph.resource(hdr("drive dof far"));
    let focused = graph.resource(hdr("drive dof"));
    let clear = |resource| Attachment::clear(resource, Color::TRANSPARENT);
    graph.pass(
        PassDesc::screen(DOF_CIRCLES, "wxsl.dof_coc")
            .with_color(clear(coc))
            .with_reads([Read::current(depth)]),
    );
    graph.pass(
        PassDesc::screen("drive dof far", "wxsl.dof_far")
            .with_color(clear(far))
            .with_reads([Read::current(blurred), Read::current(coc)])
            .with_parameter("max_coc", Value::F32(MAX_COC)),
    );
    graph.pass(
        PassDesc::screen("drive dof near", "wxsl.dof_near")
            .with_color(clear(focused))
            .with_reads([
                Read::current(blurred),
                Read::current(coc),
                Read::current(far),
            ])
            .with_parameter("max_coc", Value::F32(MAX_COC)),
    );
    graph.pass_mut(tonemap).ok_or("tonemap pass")?.reads[0] = Read::current(focused);
    // FXAA after the tonemap, on display values.
    if fxaa {
        let encoded = graph.resource(hdr("drive encoded"));
        let pass = graph.pass_mut(tonemap).ok_or("tonemap pass")?;
        let presented = std::mem::replace(&mut pass.color[0], clear(encoded));
        graph.pass(
            PassDesc::screen("drive fxaa", "wxsl.fxaa")
                .with_color(presented)
                .with_reads([Read::current(encoded)]),
        );
    }
    // The car's stand-in is drawn by the shadow passes and by no other.
    let hidden = TagExpr::tag(CAR_SHADOW_TAG).not();
    for index in 0..graph.passes().len() {
        if let Some(PassKind::Geometry {
            source: DrawSource::Scene(selector),
            stage,
        }) = graph.pass_mut(index).map(|pass| &mut pass.kind)
        {
            if *stage != MaterialStage::SHADOW {
                *selector = selector.clone().and(hidden.clone());
            }
        }
    }
    renderer.set_graph(graph)?;
    Ok(())
}

/// Focus on a point `focus` metres away, through a lens of `focal` metres
/// at `f_number`.
pub fn set_lens(
    renderer: &mut Renderer,
    camera: &Camera,
    focus: f32,
    f_number: f32,
) -> Result<(), Box<dyn Error>> {
    for (name, value) in [
        ("near", camera.near),
        ("far", camera.far),
        ("focus", focus),
        ("f_number", f_number),
        ("focal", 0.05),
    ] {
        renderer.set_pass_param(DOF_CIRCLES, name, Value::F32(value))?;
    }
    Ok(())
}
