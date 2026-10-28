//! The frame: the stock forward document with a velocity pass, motion
//! blur and a bloom pyramid composed onto it as nodes, then, on the
//! compiled pass list, the
//! sky baked into the environment maps and drawn behind everything, a
//! height fog lit by that sky, the car's glass, depth of field focused on the car, the lens'
//! glare (a star through the highlights, [`HALOS`]), a colour matrix,
//! chromatic aberration when it is on, the tonemap (one of [`TONEMAPS`]) and FXAA.
//!
//! ```text
//! shadows → prepass → forward ─┬─ background (sky) ─ (ssr) ─ fog ─ sun ─ glass ─ motion blur ─ bloom ─ dof ─ (glare) ─ grade ─ (aberration) ─ tonemap ─ fxaa
//!                               │   (the demo's own, `shaders/motion_blur.wxsl`, or
//!                               │    `shaders/motion_good.wxsl` after the motion tiles)
//!                     velocity ┘
//! sky (once per hour) → IBL bake (once) ┘
//! ```
//!
//! Under the car, before the background, a pass multiplies the road by
//! the car's occlusion: the ground grid's PRT bake lit by the car's probe
//! (`car_ground`), drawn by no other pass. The same grid writes the
//! occlusion factor directly for the motion blur to leave in place. Road
//! normals and world shadows stay in the colour that is blurred.
//! Another then adds the neons'
//! baked light (`neon`), and a third the far lamps' halos (`halo`); the
//! probe captures neither.
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

use glam::Vec3;

use wxsl::core::abi::{self, MaterialStage};
use wxsl::core::graph::{Graph, Node, NodeId, SocketRef};
use wxsl::core::node::NodeRegistry;
use wxsl::core::pipeline as doc;
use wxsl::core::scene::TagExpr;
use wxsl::core::Value;
use wxsl::render::effect::{
    self, glare, ibl, refraction, ssr, sun, Downlevel, Effect, EffectInput, EffectInputKind,
    EffectKind, EffectParameter, EffectShader,
};
use wxsl::render::pass::{
    Attachment, DepthAttachment, DepthBias, DrawSource, Extent, Load, PassDesc, PassKind,
    PassState, Policy, Read, ResourceDesc, ResourceId,
};
use wxsl::render::types::{
    BlendComponent, BlendFactor, BlendOperation, BlendState, Color, CompareFunction, TextureFormat,
};
use wxsl::render::{Camera, Renderer, StockPipeline};

use crate::car_ground::GROUND_TAG;
use crate::halo::HALO_TAG;
use crate::lights::{CAR_SHADOW_TAG, CAR_TAG};
use crate::neon::NEON_TAG;
use crate::sky::{SKY, SKY_HEIGHT, SKY_PASS, SKY_WIDTH};

/// The pass the lens' numbers go to.
const DOF_CIRCLES: &str = "drive dof circles";
/// The widest blur disc, in pixels.
const MAX_COC: f32 = 20.0;
/// The shadow passes' slope-scaled depth offset, in depth slopes.
const SHADOW_SLOPE_BIAS: f32 = 1.0;
/// The motion blur's pass label, which `strength` is set on.
pub const MOTION_BLUR: &str = "motion blur";
/// The reconstruction's neighbourhood pass, which takes the same
/// `strength`.
pub const MOTION_NEIGHBORS: &str = "motion neighbours";
/// The sky's environment cubes: the edge of the radiance and specular
/// cubes, the diffuse cube's, and the specular mips. The car's probe has
/// the same mips, as a pass's own cubes must (ADR 0102), and its edge
/// scaled by the quality: seven mips leave room for a 64² probe.
const CUBE_SIZE: u32 = 128;
const DIFFUSE_SIZE: u32 = 16;
const SPECULAR_MIPS: u32 = 7;
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

/// What the lens adds to the highlights: the bloom's glow or not, and a
/// star through its highlights (`effect::glare`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glare {
    pub bloom: bool,
    pub star: Option<glare::Star>,
}

/// The `halo` slider's steps: (name, the bloom's glow, a star).
pub const HALOS: [(&str, bool, Option<glare::Star>); 6] = [
    ("none", false, None),
    ("bloom", true, None),
    ("cross", true, Some(glare::Star::Cross)),
    ("starburst", true, Some(glare::Star::Starburst)),
    ("hexagonal", true, Some(glare::Star::Hexagonal)),
    ("anamorphic", true, Some(glare::Star::Anamorphic)),
];

/// The bloom document node's label, and the glare's passes'.
const BLOOM_LABEL: &str = "drive bloom";
const GLARE: &str = "drive glare";
/// What glares: only what is brighter than this, the lamps, the
/// headlights, the sun and its glints, not a lit wall.
pub const GLARE_THRESHOLD: f32 = 6.0;
/// The sun's disc pass, and the disc: twice the Sun's own size, so it
/// reads as a disc rather than a point, and far dimmer than the Sun's
/// irradiance over it would make it, so it glares without whiting out
/// the frame.
const SUN: &str = "drive sun";
const SUN_RADIUS: f32 = sun::SUN_RADIUS * 2.0;
const SUN_GAIN: f32 = 150.0;

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
        EffectInput {
            name: "shadow",
            kind: EffectInputKind::Image,
            description: "The car's PRT ground occlusion, left unblurred.",
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

/// The motion blur's qualities: the drive's own gather along each
/// pixel's motion, or `wxsl.motion_blur`'s reconstruction filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blur {
    /// Along each pixel's own motion, refusing taps that moved otherwise:
    /// a fast post stays sharp over a still background.
    Fast,
    /// Along the longest motion around each pixel, by depth: a fast post
    /// smears over whatever it crosses.
    Good,
}

/// The `motion blur quality` slider's steps.
pub const BLURS: [(&str, Option<Blur>); 3] = [
    ("off", None),
    ("fast", Some(Blur::Fast)),
    ("good", Some(Blur::Good)),
];

/// `wxsl.motion_blur`'s reconstruction filter, with the car's contact
/// occlusion left in place.
const MOTION_GOOD_EFFECT: Effect = Effect {
    id: "drive.motion_good",
    label: "motion blur",
    description: "Smear the frame along the longest motion around it, by depth.",
    kind: EffectKind::Screen {
        vertex_entry: "drive_motion_good_vs",
        fragment_entry: "drive_motion_good_fs",
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
        EffectInput {
            name: "depth",
            kind: EffectInputKind::DepthImage,
            description: "The opaque depth that says what is in front.",
            history: 0,
        },
        EffectInput {
            name: "neighbors",
            kind: EffectInputKind::Image,
            description: "The motion neighbours' longest motions around each tile.",
            history: 0,
        },
        EffectInput {
            name: "shadow",
            kind: EffectInputKind::Image,
            description: "The car's PRT ground occlusion, left unblurred.",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "strength",
        default: Value::F32(1.0),
    }],
    shader: EffectShader::Source {
        path: "package::drive::motion_good",
        wxsl: include_str!("../shaders/motion_good.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The car's ground occlusion, written directly by its PRT grid.
/// World shadows and normal-map lighting must not enter this factor.
const CAR_SHADOW: &str = "car shadow";
const CAR_SHADOW_PASS: &str = "drive car shadow";

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

/// The pass that adds the neons' light to the frame.
const NEON: &str = "drive neon";
/// What is drawn plus what the fragment writes, alpha kept.
const ADD: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

/// The pass that adds the far lamps' halos to the frame.
const LAMP_HALOS: &str = "drive lamp halos";

/// The draws tagged `tag`, the neons' or the halos', added over `color`,
/// depth-tested against `depth`, both faces: a halo's sprite faces the
/// screen whichever way its corners wind, and the decal is seen from
/// above.
fn add_pass(label: String, tag: &str, color: ResourceId, depth: ResourceId) -> PassDesc {
    PassDesc::geometry(
        label,
        DrawSource::Scene(TagExpr::tag(tag)),
        MaterialStage::FORWARD_LIT,
    )
    .with_color(Attachment::load(color))
    .with_depth(DepthAttachment::load(depth))
    .with_state(
        PassState::OPAQUE
            .with_depth_test(CompareFunction::LessEqual, false)
            .with_blend(ADD)
            .with_cull_mode(None),
    )
}

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
/// The grade's saturation until the host sets its rows: 1 leaves the
/// frame's colours as they are.
pub const SATURATION: f32 = 1.0;

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

/// The reflections' pass label.
pub const SSR: &str = "drive ssr";
/// The glass reflections' pass label.
const SSR_GLASS: &str = "drive glass ssr";

/// The aberration's pass label, which its strength is set on.
pub const ABERRATION: &str = "drive aberration";

/// Chromatic aberration (`shaders/aberration.wxsl`): the stdlib's node,
/// its strength a parameter.
const ABERRATION_EFFECT: Effect = Effect {
    id: "drive.aberration",
    label: "aberration",
    description: "Lens-dispersion fringing over the linear frame, strongest at the edge.",
    kind: EffectKind::Screen {
        vertex_entry: "drive_aberration_vs",
        fragment_entry: "drive_aberration_fs",
    },
    inputs: &[EffectInput {
        name: "color",
        kind: EffectInputKind::Image,
        description: "The frame, as linear radiance.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "strength",
        default: Value::F32(0.0),
    }],
    shader: EffectShader::Source {
        path: "package::drive::aberration",
        wxsl: include_str!("../shaders/aberration.wxsl"),
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

/// The display transforms the `tonemap` slider steps through: its name,
/// the stdlib curve, and the effect that applies it. Filmic is the stock
/// `wxsl.tonemap`; each other is its graph with that curve in its place.
pub const TONEMAPS: [(&str, &str, &str); 7] = [
    ("filmic", "color.tonemap_filmic", "wxsl.tonemap"),
    ("ACES", "color.tonemap_aces", "drive.tonemap_aces"),
    (
        "Reinhard",
        "color.tonemap_reinhard",
        "drive.tonemap_reinhard",
    ),
    (
        "Reinhard-Jodie",
        "color.tonemap_reinhard_jodie",
        "drive.tonemap_reinhard_jodie",
    ),
    ("WXSL", "color.tonemap_wxsl", "drive.tonemap_wxsl"),
    ("AgX", "color.tonemap_agx", "drive.tonemap_agx"),
    ("Hable", "color.tonemap_hable", "drive.tonemap_hable"),
];

/// The stock tonemap's graph with `curve` for its filmic curve: the same
/// load, encode and alpha, and the same `color, exposure` sockets.
fn tonemap_effect(
    registry: &NodeRegistry,
    (label, curve, id): (&'static str, &str, &'static str),
) -> Result<Effect, Box<dyn Error>> {
    let mut graph = wxsl::effects::tonemap_graph(registry);
    let filmic = graph
        .nodes()
        .find(|(_, node)| node.def == TONEMAPS[0].1)
        .map(|(node, _)| node)
        .ok_or("the stock tonemap has no filmic curve")?;
    graph.node_mut(filmic).ok_or("filmic curve")?.def = curve.to_string();
    Ok(Effect::from_graph(
        id,
        label,
        "The stock tonemap with another stdlib curve.",
        graph,
        registry,
    )?)
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
        MOTION_GOOD_EFFECT,
        FOG_EFFECT,
        GRADE_EFFECT,
        ABERRATION_EFFECT,
        ssr::SSR,
        ssr::GLASS,
        glare::EXTRACT,
        glare::CELLS,
        glare::STARBURST,
        glare::STREAK,
        glare::GHOSTS,
        glare::FLARE,
        glare::ADD,
        sun::SUN,
    ] {
        renderer.add_effect(effect);
    }
    let registry = wxsl::stdlib::registry();
    renderer.add_effect(wxsl::effects::fxaa(&registry)?);
    for tonemap in &TONEMAPS[1..] {
        renderer.add_effect(tonemap_effect(&registry, *tonemap)?);
    }
    Ok(())
}

/// The stock forward (or deferred) document with a velocity pass over the
/// prepass' (or the G-buffer's) depth, then a motion blur and a bloom
/// between the shaded colour and the tonemap; without `motion_blur`, only
/// the bloom: no velocity pass, no blur.
fn motion_blur_document(
    renderer: &Renderer,
    deferred: bool,
    motion_blur: Option<Blur>,
) -> Result<Graph, Box<dyn Error>> {
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
    let glowing = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("glowing")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bloom = BLOOM.iter().fold(
        Node::new(doc::PASS_BLOOM).with_label(BLOOM_LABEL),
        |node, (name, value)| node.with_setting(*name, *value),
    );
    let bloom = document.add(bloom);
    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    let mut wires = vec![
        ((glowing, "color"), (bloom, "into")),
        ((glowing, "color"), (tonemap, "image")),
    ];
    let Some(quality) = motion_blur else {
        wires.push(((color, "color"), (bloom, "image")));
        return wire(document, &registry, &wires);
    };
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
    // Written by the passes `install` adds.
    let shadow = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label(CAR_SHADOW)
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let blur = document.add(
        Node::new(format!(
            "{}{}",
            doc::PASS_SCREEN_PREFIX,
            blur_effect(quality).id
        ))
        .with_label(MOTION_BLUR),
    );
    wires.extend([
        ((scene, "draws"), (motion, "draws")),
        ((prepass, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((color, "color"), (blur, "color")),
        ((velocity, "color"), (blur, "velocity")),
        ((shadow, "color"), (blur, "shadow")),
        ((blurred, "color"), (blur, "into")),
        ((blurred, "color"), (bloom, "image")),
    ]);
    if quality == Blur::Good {
        // The reconstruction's tiles: the longest motion per tile, then
        // around it, which the blur follows.
        let tile = |label: &str| {
            Node::new(doc::RESOURCE_COLOR)
                .with_label(label)
                .with_setting(doc::SETTING_PRECISION, "pair")
                .with_setting(doc::SETTING_SCALE, effect::MOTION_TILES_SCALE)
        };
        let tiles = document.add(tile("motion tiles target"));
        let neighbors = document.add(tile("motion neighbours target"));
        let tiling = document.add(
            Node::new(doc::PASS_SCREEN)
                .with_setting(doc::SETTING_EFFECT, effect::MOTION_TILES.id)
                .with_label("motion tiles"),
        );
        let spreading = document.add(
            Node::new(doc::PASS_SCREEN)
                .with_setting(doc::SETTING_EFFECT, effect::MOTION_NEIGHBORS.id)
                .with_label(MOTION_NEIGHBORS),
        );
        wires.extend([
            ((velocity, "color"), (tiling, "image")),
            ((tiles, "color"), (tiling, "into")),
            ((tiles, "color"), (spreading, "image")),
            ((neighbors, "color"), (spreading, "into")),
            ((prepass, "depth"), (blur, "depth")),
            ((neighbors, "color"), (blur, "neighbors")),
        ]);
    }
    wire(document, &registry, &wires)
}

/// The blur's effect at `quality`.
fn blur_effect(quality: Blur) -> &'static Effect {
    match quality {
        Blur::Fast => &MOTION_BLUR_EFFECT,
        Blur::Good => &MOTION_GOOD_EFFECT,
    }
}

/// One wire of a document: from a node's socket to another's.
type Wire<'a> = ((NodeId, &'a str), (NodeId, &'a str));

/// `document` with each of `wires` wired.
fn wire(
    mut document: Graph,
    registry: &NodeRegistry,
    wires: &[Wire<'_>],
) -> Result<Graph, Box<dyn Error>> {
    for &(from, to) in wires {
        document
            .wire(registry, from, to)
            .map_err(|error| format!("motion blur wiring: {error}"))?;
    }
    Ok(document)
}

/// Build the frame, forward or `deferred`, with or without the
/// `motion_blur` (its velocity pass, the car's shadow and the blur), `fxaa`,
/// the chromatic `aberration` pass and the screen-space reflections (`ssr`,
/// at that quality),
/// the lens' `glare`,
/// displayed through [`TONEMAPS`]`[tonemap]`, and give it to `renderer`. With `refractive`, the car's glass leaves the opaque
/// passes for the refraction tier (ADR 0067). `quality` scales the car's
/// probe and the shadow atlas from their defaults (0.5, 1 or 2); at 0 the
/// car has no probe and reflects the sky's maps, and the caller turns the
/// shadows off.
#[allow(clippy::too_many_arguments)]
pub fn install(
    renderer: &mut Renderer,
    refractive: bool,
    deferred: bool,
    motion_blur: Option<Blur>,
    fxaa: bool,
    aberration: bool,
    ssr: Option<ssr::Quality>,
    glare: Glare,
    tonemap: usize,
    quality: f32,
) -> Result<(), Box<dyn Error>> {
    let document = motion_blur_document(renderer, deferred, motion_blur)?;
    // The renderer's own: its lighting set shapes the G-buffer.
    let mut config = renderer.pipeline_config().clone();
    // The sun's far cascades cover metres a texel: the slope term keeps
    // the road off its own texels there, which 5 cm of normal offset
    // cannot.
    config.shadow_depth_bias = DepthBias::slope(SHADOW_SLOPE_BIAS);
    if quality > 0.0 {
        config.shadow_atlas_size = (abi::SHADOW_ATLAS_SIZE as f32 * quality) as u32;
    }
    let mut graph = wxsl::render::compile_pipeline(
        &document,
        &wxsl::render::document_registry(renderer.effects()),
        renderer.effects(),
        &config,
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
    // The G-buffer the reflections read, before the probe's passes join.
    let gbuffer = match ssr {
        Some(quality) => {
            let layout = config.plan().map_err(|error| error.to_string())?;
            let targets = ssr::gbuffer(&mut graph, layout.layout().to_vec(), "drive")?;
            Some((targets, quality))
        }
        None => None,
    };

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
    let blur = match motion_blur {
        Some(quality) => Some(effect_pass(&graph, blur_effect(quality).id)?),
        None => None,
    };
    let bloom_pass = |pass: &PassDesc| pass.label.starts_with(BLOOM_LABEL);
    let curve = tonemap;
    let tonemap = effect_pass(&graph, TONEMAPS[0].2)?;
    graph.pass_mut(tonemap).ok_or("tonemap pass")?.kind = PassKind::Screen {
        effect: TONEMAPS[curve].2.to_string(),
    };
    // What the blur reads, or without it the bloom.
    let shaded = match blur {
        Some(blur) => graph.passes()[blur].reads[0].resource,
        None => graph
            .passes()
            .iter()
            .find(|pass| bloom_pass(pass))
            .map(|pass| pass.reads[0].resource)
            .ok_or("the frame has no bloom")?,
    };
    let depth = graph
        .passes()
        .iter()
        .filter_map(|pass| pass.depth.as_ref().map(|attachment| attachment.resource))
        .next_back()
        .ok_or("the frame has no depth")?;

    // The car's probe, after the lookups above: its faces have depth too.
    // Without one, the car reflects the sky's maps: diffuse, specular and
    // the radiance its glass refracts.
    let probe_size = (CUBE_SIZE as f32 * quality) as u32;
    let probe = if probe_size == 0 {
        ProbeMaps {
            diffuse: maps.diffuse,
            specular: maps.specular,
            radiance: maps.radiance,
        }
    } else {
        let probe = ibl::append_probe(
            &mut graph,
            TagExpr::parse(&format!("!{CAR_TAG}"))?,
            maps.radiance,
            PROBE,
            probe_size,
            DIFFUSE_SIZE,
            SPECULAR_MIPS,
            PROBE_SAMPLES,
            // Refreshed every few frames by `refresh_probe`.
            Policy::OnDemand,
        );
        fog_probe(&mut graph, probe_size, maps.radiance)?;
        ProbeMaps {
            diffuse: probe.diffuse,
            specular: probe.specular,
            radiance: probe.radiance,
        }
    };
    // The car's occlusion on the road, then the neons' light, over the
    // world and lit by the probe, before the car: declared first, they
    // write the frame first, and the car drawn after hides them where it
    // stands in front of the road.
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
    // Preserve only the car's contact occlusion. A ratio of shaded and
    // unshadowed road would also restore world shadows and normal detail
    // after the blur. The PRT grid already outputs the isolated factor.
    if motion_blur.is_some() {
        let car_shadow = graph
            .resource_by_label(CAR_SHADOW)
            .ok_or("the frame has no car shadow target")?;
        graph.pass(
            PassDesc::geometry(
                CAR_SHADOW_PASS,
                DrawSource::Scene(TagExpr::tag(GROUND_TAG)),
                MaterialStage::FORWARD_LIT,
            )
            .with_color(Attachment::clear(
                car_shadow,
                Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            ))
            .with_depth(DepthAttachment::load(depth))
            .with_state(PassState::OPAQUE.with_depth_test(CompareFunction::LessEqual, false))
            .with_environment(probe.diffuse, probe.specular),
        );
    }
    graph.pass(add_pass(NEON.into(), NEON_TAG, shaded, depth));
    graph.pass(add_pass(LAMP_HALOS.into(), HALO_TAG, shaded, depth));
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
    let sky_behind =
        ibl::append_background(&mut graph, shaded, depth, maps.radiance, "drive background");
    // The reflections of the opaque frame, before the fog fogs them.
    let opaque = match gbuffer {
        Some((targets, quality)) => {
            ssr::append(&mut graph, sky_behind, depth, targets, quality, SSR)
        }
        None => sky_behind,
    };
    // The fog over everything opaque, before the glass refracts it.
    let mut background = graph.resource(hdr(FOG));
    graph.pass(fog_pass(FOG, background, [opaque, depth, maps.radiance]));
    // The sun's disc on the fogged sky, before the glass refracts it.
    sun::append(&mut graph, background, depth, SUN_RADIUS, SUN);
    if refractive {
        let optics = refraction::append_optics(
            &mut graph,
            background,
            depth,
            probe.radiance,
            "drive glass",
            2,
            refraction::Quality::Layered,
        );
        // The glass's own reflections, of the fogged opaque frame.
        background = match gbuffer {
            Some((_, quality)) => ssr::append_glass(
                &mut graph,
                optics,
                background,
                depth,
                probe.radiance,
                quality,
                SSR_GLASS,
            ),
            None => optics.color,
        };
    }
    match blur {
        Some(blur) => {
            graph.pass_mut(blur).ok_or("motion blur pass")?.reads[0] = Read::current(background)
        }
        // The bloom's passes read the frame where the blur would have.
        None => {
            for index in 0..graph.passes().len() {
                let Some(pass) = graph.pass_mut(index).filter(|pass| bloom_pass(pass)) else {
                    continue;
                };
                for read in &mut pass.reads {
                    if read.resource == shaded {
                        *read = Read::current(background);
                    }
                }
            }
        }
    }

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
    // The lens' glare after the depth of field: it is the lens', not the
    // scene's, so nothing blurs it.
    if !glare.bloom {
        let combine = format!("{BLOOM_LABEL} combine");
        let index = graph
            .passes()
            .iter()
            .position(|pass| pass.label == combine)
            .ok_or("the frame has no bloom")?;
        if let Some(pass) = graph.pass_mut(index) {
            pass.parameters.insert("strength".into(), Value::F32(0.0));
        }
    }
    let focused = glare::append(
        &mut graph,
        focused,
        depth,
        glare::Lens {
            star: glare.star,
            flare: None,
            threshold: GLARE_THRESHOLD,
        },
        GLARE,
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
    // The fringe, on the radiance the grade left.
    let graded = if aberration {
        let fringed = graph.resource(hdr("drive aberrated"));
        graph.pass(
            PassDesc::screen(ABERRATION, ABERRATION_EFFECT.id)
                .with_color(clear(fringed))
                .with_reads([Read::current(graded)]),
        );
        fringed
    } else {
        graded
    };
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
    // the ground grid, the neons and the halos by
    // their own passes alone.
    let hidden = TagExpr::tag(CAR_SHADOW_TAG).not();
    let grid = TagExpr::tag(GROUND_TAG)
        .or(TagExpr::tag(NEON_TAG))
        .or(TagExpr::tag(HALO_TAG))
        .not();
    for index in 0..graph.passes().len() {
        let Some(pass) = graph.pass_mut(index) else {
            continue;
        };
        if [GROUND, CAR_SHADOW_PASS, NEON, LAMP_HALOS].contains(&pass.label.as_str()) {
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

/// The cubes the car is lit by: its probe's, or the sky's.
struct ProbeMaps {
    diffuse: ResourceId,
    specular: ResourceId,
    radiance: ResourceId,
}

/// The same fog over each of the probe's faces, `size` texels square,
/// from the face's view, before the capture folds it into the cube: the
/// car reflects a fogged world.
fn fog_probe(
    graph: &mut wxsl::render::RenderGraph,
    size: u32,
    sky: ResourceId,
) -> Result<(), Box<dyn Error>> {
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
        // Kept between refreshes, as the probe's faces are.
        let fogged = graph.resource(
            ResourceDesc::color(label.clone(), TextureFormat::Rgba16Float)
                .with_extent(Extent::Fixed {
                    width: size,
                    height: size,
                })
                .persistent(0),
        );
        graph.pass(
            fog_pass(&label, fogged, [color, depth, sky])
                .with_view(view)
                .with_policy(Policy::OnDemand),
        );
        graph.pass_mut(capture).ok_or("probe capture")?.reads[0] = Read::current(fogged);
    }
    Ok(())
}

/// Run the car's probe on the next frame: its capture, fog and filtering
/// are on demand, so that it can be refreshed every few frames.
pub fn refresh_probe(renderer: &mut Renderer) {
    let fog = format!("{FOG} {PROBE}");
    let labels: Vec<String> = renderer
        .render_graph()
        .passes()
        .iter()
        .filter(|pass| pass.label.starts_with(PROBE) || pass.label.starts_with(&fog))
        .map(|pass| pass.label.clone())
        .collect();
    for label in &labels {
        renderer.mark_pass(label);
    }
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

/// Put the sun's disc at `direction` (towards it) with `color` (its
/// light, 0 at night).
pub fn set_sun(
    renderer: &mut Renderer,
    direction: Vec3,
    color: Vec3,
) -> Result<(), Box<dyn Error>> {
    let towards = Value::Vec4([direction.x, direction.y, direction.z, 0.0]);
    let radiance = color * SUN_GAIN;
    renderer.set_pass_param(SUN, "direction", towards)?;
    renderer.set_pass_param(
        SUN,
        "radiance",
        Value::Vec4([radiance.x, radiance.y, radiance.z, 0.0]),
    )?;
    Ok(())
}

/// Make `star`'s rays reach `length` times as far as it sets them to, and
/// add `opacity` times as strongly; nothing for no star or one without
/// rays.
pub fn set_rays(
    renderer: &mut Renderer,
    star: Option<glare::Star>,
    length: f32,
    opacity: f32,
) -> Result<(), Box<dyn Error>> {
    let Some(star) = star else {
        return Ok(());
    };
    let rays = format!("{} rays", glare::star_label(GLARE));
    for (name, scale) in [("length", length), ("strength", opacity)] {
        if let Some(value) = star.ray_parameter(name) {
            renderer.set_pass_param(&rays, name, Value::F32(value * scale))?;
        }
    }
    Ok(())
}

/// Set the fog's `density` per metre at the ground, on the frame and on
/// each of the probe's faces.
pub fn set_fog(renderer: &mut Renderer, density: f32) -> Result<(), Box<dyn Error>> {
    for label in std::iter::once(FOG.to_string()).chain((0..PROBE_FACES).map(probe_fog)) {
        // The probe's faces only when the frame has the probe.
        let present = renderer
            .render_graph()
            .passes()
            .iter()
            .any(|pass| pass.label == label);
        if present {
            renderer.set_pass_param(&label, "density", Value::F32(density))?;
        }
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
#[path = "motion_blur_tests.rs"]
mod motion_blur_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// What the `halo` slider does: each step re-installs the frame, and
    /// the renderer must then hold the new step's values, not the ones
    /// the passes it replaced had.
    #[test]
    fn each_halo_step_installs_its_own_values() {
        use glare::Star;
        let Ok(gpu) = pollster::block_on(wxsl::render::gpu::GpuContext::headless()) else {
            return;
        };
        let (_, library) = crate::world::libraries().unwrap();
        let target = wxsl::render::TargetConfig::new(64, 64, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut renderer = Renderer::new(&gpu.device, library, target).unwrap();
        add_effects(&mut renderer).unwrap();
        let rays = format!("{} rays", glare::star_label(GLARE));
        let combine = format!("{BLOOM_LABEL} combine");
        let mut step = |bloom: bool, star: Option<Star>| {
            let glare = Glare { bloom, star };
            install(
                &mut renderer,
                false,
                false,
                Some(Blur::Good),
                true,
                false,
                None,
                glare,
                0,
                1.0,
            )
            .unwrap();
            let contact = renderer
                .render_graph()
                .passes()
                .iter()
                .find(|pass| pass.label == CAR_SHADOW_PASS)
                .unwrap();
            let PassKind::Geometry {
                source: DrawSource::Scene(tags),
                ..
            } = &contact.kind
            else {
                panic!("contact occlusion must come from the PRT grid, not the shaded road");
            };
            assert_eq!(*tags, TagExpr::tag(GROUND_TAG));
            (
                renderer.pass_param(&rays, "rays"),
                renderer.pass_param(&combine, "strength"),
            )
        };
        let bloom = Some(Value::F32(0.07));
        assert_eq!(
            step(false, Some(Star::Hexagonal)),
            (Some(Value::F32(6.0)), Some(Value::F32(0.0)))
        );
        assert_eq!(
            step(false, Some(Star::Starburst)),
            (Some(Value::F32(128.0)), Some(Value::F32(0.0)))
        );
        assert_eq!(step(true, None), (None, bloom));
        assert_eq!(step(false, None), (None, Some(Value::F32(0.0))));
        assert_eq!(
            step(false, Some(Star::Cross)),
            (Some(Value::F32(4.0)), Some(Value::F32(0.0)))
        );
    }

    #[test]
    fn every_other_tonemap_is_the_stock_graph_with_its_own_curve() {
        let registry = wxsl::stdlib::registry();
        for tonemap in &TONEMAPS[1..] {
            let effect = tonemap_effect(&registry, *tonemap).unwrap();
            let EffectShader::Graph { wxsl, .. } = &effect.shader else {
                panic!("{} is not a graph effect", tonemap.0);
            };
            let curve = tonemap.1.trim_start_matches("color.");
            assert!(wxsl.contains(curve), "{}", tonemap.0);
            assert!(!wxsl.contains("tonemap_filmic"), "{}", tonemap.0);
        }
    }

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
