//! The frame: the stock forward document with a velocity pass, motion
//! blur and a bloom pyramid composed onto it as nodes, then, on the
//! compiled pass list, the
//! sky baked into the environment maps and drawn behind everything, a
//! height fog lit by that sky, the car's glass, depth of field focused on the car, a colour matrix, the
//! tonemap and FXAA.
//!
//! ```text
//! shadows → prepass → forward ─┬─ background (sky) ─ fog ─ glass ─ motion blur ─ bloom ─ dof ─ grade ─ tonemap ─ fxaa
//!                               │   (the demo's own, `shaders/motion_blur.wxsl`)
//!                     velocity ┘
//! sky (once per hour) → IBL bake (once) ┘
//! ```
//!
//! Under the car, before the background, a pass multiplies the road by
//! the car's occlusion: the ground grid's PRT bake lit by the car's probe
//! (`car_ground`), drawn by no other pass.
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
    Attachment, DepthAttachment, DrawSource, Extent, Load, PassDesc, PassKind, PassState, Policy,
    Read, ResourceDesc, ResourceId,
};
use wxsl::render::types::{
    BlendComponent, BlendFactor, BlendOperation, BlendState, Color, CompareFunction, TextureFormat,
};
use wxsl::render::{Camera, Renderer, StockPipeline};

use crate::car_ground::GROUND_TAG;
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

/// The bloom pyramid's settings: three levels for a faint, tight halo
/// around the lamps and the headlights (each level doubles its reach),
/// from radiance well above a lit road's.
const BLOOM: [(&str, &str); 4] = [
    ("levels", "3"),
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

/// The pass that multiplies the road under the car by its occlusion.
const GROUND: &str = "drive car ground";
/// What is drawn times what the fragment writes, alpha kept.
const MULTIPLY: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::Src,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

/// The fog's pass label; each probe face's adds the face (`set_fog`).
const FOG: &str = "drive fog";
/// How many faces the probe has, each fogged from its own view.
const PROBE_FACES: u32 = 6;

/// The height fog (`shaders/fog.wxsl`): `density` per metre at `base`
/// metres, thinning by `falloff` per metre above it, lit by the sky.
const FOG_EFFECT: Effect = Effect {
    id: "drive.fog",
    label: "fog",
    description: "Exponential height fog over the opaque frame, lit by the sky's radiance.",
    kind: EffectKind::Screen {
        vertex_entry: "drive_fog_vs",
        fragment_entry: "drive_fog_fs",
    },
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The frame, as linear radiance.",
            history: 0,
        },
        EffectInput {
            name: "depth",
            kind: EffectInputKind::DepthImage,
            description: "The opaque depth.",
            history: 0,
        },
        EffectInput {
            name: "radiance",
            kind: EffectInputKind::Image,
            description: "The sky's radiance cube, with its mips.",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "density",
            default: Value::F32(0.0012),
        },
        EffectParameter {
            name: "falloff",
            default: Value::F32(1.0 / 120.0),
        },
        EffectParameter {
            name: "base",
            default: Value::F32(0.0),
        },
    ],
    shader: EffectShader::Source {
        path: "package::drive::fog",
        wxsl: include_str!("../shaders/fog.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The grade's pass label, which its rows are set on.
pub const GRADE: &str = "drive grade";
/// The grade's saturation: 1 leaves the frame's colours as they are.
const SATURATION: f32 = 1.2;

/// The final colour matrix (`shaders/grade.wxsl`): a row per output
/// channel, its w a lift.
const GRADE_EFFECT: Effect = Effect {
    id: "drive.grade",
    label: "grade",
    description: "A colour matrix over the linear frame, before the tonemap.",
    kind: EffectKind::Screen {
        vertex_entry: "drive_grade_vs",
        fragment_entry: "drive_grade_fs",
    },
    inputs: &[EffectInput {
        name: "color",
        kind: EffectInputKind::Image,
        description: "The frame, as linear radiance.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "red",
            default: Value::Vec4([1.0, 0.0, 0.0, 0.0]),
        },
        EffectParameter {
            name: "green",
            default: Value::Vec4([0.0, 1.0, 0.0, 0.0]),
        },
        EffectParameter {
            name: "blue",
            default: Value::Vec4([0.0, 0.0, 1.0, 0.0]),
        },
    ],
    shader: EffectShader::Source {
        path: "package::drive::grade",
        wxsl: include_str!("../shaders/grade.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The grade's rows for `saturation`: each colour pushed from its
/// luminance (Rec. 709) by that factor, 1 being the identity.
pub fn grade_rows(saturation: f32) -> [[f32; 4]; 3] {
    const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
    std::array::from_fn(|row| {
        let mut out = [0.0; 4];
        for (column, luma) in LUMA.iter().enumerate() {
            let identity = if row == column { 1.0 } else { 0.0 };
            out[column] = luma + (identity - luma) * saturation;
        }
        out
    })
}

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
        FOG_EFFECT,
        GRADE_EFFECT,
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
    let hdr = |name: &str| ResourceDesc::color(name, TextureFormat::Rgba16Float);
    let clear = |resource| Attachment::clear(resource, Color::TRANSPARENT);
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
    // The same fog over each face, from the face's view, before the
    // capture folds it into the cube: the car reflects a fogged world.
    for face in 0..PROBE_FACES {
        let capture = graph
            .passes()
            .iter()
            .position(|pass| pass.label == format!("{PROBE} capture {face}"))
            .ok_or_else(|| format!("the probe has no face {face}"))?;
        let (view, color, depth) = {
            let pass = &graph.passes()[capture];
            (pass.view, pass.reads[0].resource, pass.reads[1].resource)
        };
        let label = probe_fog(face);
        let fogged = graph.resource(
            ResourceDesc::color(label.clone(), TextureFormat::Rgba16Float).with_extent(
                Extent::Fixed {
                    width: CUBE_SIZE,
                    height: CUBE_SIZE,
                },
            ),
        );
        graph.pass(fog_pass(&label, fogged, [color, depth, maps.radiance]).with_view(view));
        graph.pass_mut(capture).ok_or("probe capture")?.reads[0] = Read::current(fogged);
    }
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
    // The car's occlusion on the road, over the opaque frame and lit by
    // the probe, before anything reads that frame.
    graph.pass(
        PassDesc::geometry(
            GROUND,
            DrawSource::Scene(TagExpr::tag(GROUND_TAG)),
            MaterialStage::FORWARD_LIT,
        )
        .with_color(Attachment::load(shaded))
        .with_depth(DepthAttachment::load(depth))
        .with_state(
            PassState::OPAQUE
                .with_depth_test(CompareFunction::LessEqual, false)
                .with_blend(MULTIPLY),
        )
        .with_environment(probe.diffuse, probe.specular),
    );
    let sky_behind =
        ibl::append_background(&mut graph, shaded, depth, maps.radiance, "drive background");
    // The fog over everything opaque, before the glass refracts it.
    let mut background = graph.resource(hdr(FOG));
    graph.pass(fog_pass(
        FOG,
        background,
        [sky_behind, depth, maps.radiance],
    ));
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
    let blurred = graph.passes()[tonemap].reads[0].resource;
    let coc = graph.resource(hdr("drive dof coc"));
    let far = graph.resource(hdr("drive dof far"));
    let focused = graph.resource(hdr("drive dof"));
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
    // The colour matrix, the last change to the radiance.
    let graded = graph.resource(hdr("drive graded"));
    let [red, green, blue] = grade_rows(SATURATION);
    graph.pass(
        PassDesc::screen(GRADE, GRADE_EFFECT.id)
            .with_color(clear(graded))
            .with_reads([Read::current(focused)])
            .with_parameter("red", Value::Vec4(red))
            .with_parameter("green", Value::Vec4(green))
            .with_parameter("blue", Value::Vec4(blue)),
    );
    graph.pass_mut(tonemap).ok_or("tonemap pass")?.reads[0] = Read::current(graded);
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
    // The car's stand-in is drawn by the shadow passes and by no other,
    // the ground grid by its own pass alone.
    let hidden = TagExpr::tag(CAR_SHADOW_TAG).not();
    let grid = TagExpr::tag(GROUND_TAG).not();
    for index in 0..graph.passes().len() {
        let Some(pass) = graph.pass_mut(index) else {
            continue;
        };
        if pass.label == GROUND {
            continue;
        }
        if let PassKind::Geometry {
            source: DrawSource::Scene(selector),
            stage,
        } = &mut pass.kind
        {
            *selector = selector.clone().and(grid.clone());
            if *stage != MaterialStage::SHADOW {
                *selector = selector.clone().and(hidden.clone());
            }
        }
    }
    renderer.set_graph(graph)?;
    Ok(())
}

/// The fog pass of the probe's `face`.
fn probe_fog(face: u32) -> String {
    format!("{FOG} {PROBE} {face}")
}

/// A fog pass into `into`, over `[color, depth, radiance]`.
fn fog_pass(label: &str, into: ResourceId, [color, depth, radiance]: [ResourceId; 3]) -> PassDesc {
    PassDesc::screen(label, FOG_EFFECT.id)
        .with_color(Attachment::clear(into, Color::TRANSPARENT))
        .with_reads([
            Read::current(color),
            Read::current(depth),
            Read::current(radiance),
        ])
}

/// Set the fog's `density` per metre at the ground, on the frame and on
/// each of the probe's faces.
pub fn set_fog(renderer: &mut Renderer, density: f32) -> Result<(), Box<dyn Error>> {
    for label in std::iter::once(FOG.to_string()).chain((0..PROBE_FACES).map(probe_fog)) {
        renderer.set_pass_param(&label, "density", Value::F32(density))?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grade_keeps_greys_and_pushes_colours() {
        let apply = |rows: [[f32; 4]; 3], color: [f32; 3]| {
            rows.map(|row| row[0] * color[0] + row[1] * color[1] + row[2] * color[2] + row[3])
        };
        let grey = apply(grade_rows(1.5), [0.4; 3]);
        assert!(grey.iter().all(|value| (value - 0.4).abs() < 1e-5));
        let red = apply(grade_rows(1.5), [1.0, 0.5, 0.5]);
        assert!(red[0] > 1.0 && red[1] < 0.5 && red[2] < 0.5);
        let same = apply(grade_rows(1.0), [0.3, 0.6, 0.9]);
        assert!(same
            .iter()
            .zip([0.3, 0.6, 0.9])
            .all(|(a, b)| (a - b).abs() < 1e-6));
    }
}
