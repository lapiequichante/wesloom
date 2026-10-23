//! Glare: what a camera's lens adds to the bright parts of the frame
//! beyond the bloom's soft glow.
//!
//! A [`Star`] is diffraction on the aperture: thin rays through each
//! highlight, the same pattern for every light, a brighter one showing
//! more of its faint rays and showing them longer; or, for an anamorphic
//! lens, one long horizontal streak. A [`Flare`] is light reflected
//! between the lens' surfaces: ghosts of each highlight on the line
//! through the image centre and, for one light whose direction is known
//! (the sun), a flare drawn from its place on screen, hidden as the scene
//! hides it. [`append`] adds either or both onto the frame, after the
//! bloom and before the display transform.
//!
//! Both are drawn from the frame's own highlights, [`EXTRACT`]ed at half
//! resolution with a plain average: the bloom's pyramid weighs a single
//! bright texel down so that it neither flickers nor glows square, which
//! would take from the glare the small, intense lights it is for.
//! [`CELLS`] gathers them per cell of a grid, and [`STARBURST`] draws a
//! sprite of rays from each cell with light enough. [`STREAK`] blurs
//! along one direction (an anamorphic streak, a side of a hexagon),
//! [`GHOSTS`] mirrors the highlights about the centre, [`FLARE`] draws a
//! known light's flare, and [`ADD`] adds a glare onto the frame.

use super::{
    Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use crate::graph::FrameGraph;
use crate::pass::{
    Attachment, Extent, PassDesc, PassState, Read, ResourceDesc, ResourceId, ResourceShape,
    ADDITIVE,
};
use crate::types::{Color, TextureFormat};
use wxsl_core::node::Value;

/// One image, its description.
const fn image(name: &'static str, description: &'static str) -> EffectInput {
    EffectInput {
        name,
        kind: EffectInputKind::Image,
        description,
        history: 0,
    }
}

/// A parameter and its default.
const fn parameter(name: &'static str, default: Value) -> EffectParameter {
    EffectParameter { name, default }
}

/// The highlights: the excess of what is brighter than `threshold`, eased
/// in over a knee, averaged 2×2 into a target half the image's size.
pub const EXTRACT: Effect = Effect {
    id: "wxsl.glare_extract",
    label: "glare extract",
    description: "Threshold an image and average it 2x2: the highlights a glare is drawn from.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[image("image", "The frame, as linear radiance.")],
    outputs: &[],
    parameters: &[parameter("threshold", Value::F32(8.0))],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_extract",
        wxsl: include_str!("../../shaders/glare_extract.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The highlights, one per light: each texel keeps the peak radiance of
/// a cell, 8×8 texels of the extract seen as a 540-line image, when no
/// cell around outshines it, and packs where that light's core is,
/// counting it only as far as it is a point, much brighter than the cells
/// around it. The grid fills the target's top left; the target is a
/// quarter of the extract, room for the grid of any frame of 540 lines or
/// more.
pub const CELLS: Effect = Effect {
    id: "wxsl.glare_cells",
    label: "glare cells",
    description: "Find an image's highlights, one per light, with their peak and centre.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[image("image", "The glare's extract.")],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_cells",
        wxsl: include_str!("../../shaders/glare_cells.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// How many sprites [`STARBURST`] draws at most: one per cell of the grid,
/// 120 × 68 at 16:9 whatever the size, of a frame up to eight times wider
/// than high.
pub const STARBURST_SPRITES: u32 = 65536;

/// A starburst's sprites, one per cell of [`CELLS`]' grid with light
/// enough, a light's peak radiance counting over `area` pixels of a
/// 1080-line frame: `rays` rays `base` pixels wide at the light and `width` at
/// their tip, `jitter` of a spacing
/// off even, their strengths and lengths `spread` apart, from `angle`;
/// fading over `length` pixels of a 1080-line frame, as one over the
/// distance past `core`; reaching `reach` pixels per square root of the
/// cell's light, between `smallest` pixels and `largest` of the height;
/// none under `least`; adding `strength` of the light, the extract's
/// texels `scale` target pixels wide.
pub const STARBURST: Effect = Effect {
    id: "wxsl.glare_starburst",
    label: "starburst",
    description: "Draw a starburst of thin rays through each highlight, as sprites.",
    kind: EffectKind::Sprites {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
        vertices: 6,
        instances: STARBURST_SPRITES,
    },
    inputs: &[
        image(
            "cells",
            "The highlights per cell, from the glare cells pass.",
        ),
        image(
            "extract",
            "The glare's extract the cells were gathered from.",
        ),
    ],
    outputs: &[],
    parameters: &[
        parameter("rays", Value::F32(32.0)),
        parameter("jitter", Value::F32(0.8)),
        parameter("spread", Value::F32(1.0)),
        parameter("angle", Value::F32(0.0)),
        parameter("length", Value::F32(120.0)),
        parameter("width", Value::F32(2.0)),
        parameter("base", Value::F32(6.0)),
        parameter("core", Value::F32(8.0)),
        parameter("strength", Value::F32(0.3)),
        parameter("reach", Value::F32(6.0)),
        parameter("smallest", Value::F32(16.0)),
        parameter("largest", Value::F32(0.15)),
        parameter("least", Value::F32(20.0)),
        parameter("area", Value::F32(100.0)),
        parameter("scale", Value::F32(2.0)),
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_starburst",
        wxsl: include_str!("../../shaders/glare_starburst.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// One pass of a blur along `direction`: `taps` taps, `step` of the
/// height apart, centred on the pixel or (`sided` 1) starting at it, each
/// `exp(-k / falloff)` for the `k`-th out, or all alike for a `falloff`
/// of 0.
pub const STREAK: Effect = Effect {
    id: "wxsl.glare_streak",
    label: "glare streak",
    description:
        "Blur an image along one direction: one pass of a streak, or one side of a hexagon.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[image("image", "The image to blur, as linear radiance.")],
    outputs: &[],
    parameters: &[
        parameter("direction", Value::Vec2([1.0, 0.0])),
        parameter("step", Value::F32(STEP)),
        parameter("taps", Value::F32(7.0)),
        parameter("falloff", Value::F32(0.0)),
        parameter("sided", Value::F32(0.0)),
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_streak",
        wxsl: include_str!("../../shaders/glare_streak.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// A lens flare's ghosts and halo ring of whatever is bright: up to eight
/// ghosts, their scales about the centre in `scales_a` and `scales_b`,
/// stretched by `squeeze`, dispersed by `chroma`, tinted by `tint`, none
/// brighter than `brightest`; and a ring `ring` of the height wide,
/// weighing `ring_strength`; all of only what is brighter than
/// `threshold`.
pub const GHOSTS: Effect = Effect {
    id: "wxsl.glare_ghosts",
    label: "lens flare ghosts",
    description: "Mirror the bright part of an image about its centre into a lens flare's ghosts and halo ring.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[image("image", "The highlights: the glare's extract.")],
    outputs: &[],
    parameters: &[
        parameter("scales_a", Value::Vec4([-0.5, -1.2, 0.4, -2.0])),
        parameter("scales_b", Value::Vec4([0.0; 4])),
        parameter("squeeze", Value::Vec2([1.0, 1.0])),
        parameter("chroma", Value::F32(0.02)),
        parameter("tint", Value::F32(0.7)),
        parameter("brightest", Value::F32(4.0)),
        parameter("ring", Value::F32(0.0)),
        parameter("ring_strength", Value::F32(0.5)),
        parameter("threshold", Value::F32(0.0)),
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_ghosts",
        wxsl: include_str!("../../shaders/glare_ghosts.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// A known light's flare: `light` the direction towards it and its
/// strength (0 for none); ghosts at `scales_a/b` along the axis through
/// the centre, `sizes_a/b` of the height, round or (`hexagon` 1)
/// hexagonal, stretched by `squeeze`; a ring `ring` of the height around
/// it, `ring_strength` strong; a horizontal streak `streak` strong; all
/// tinted by `tint`, hidden as the depth over `occluder` of the height
/// around the light stops being sky.
pub const FLARE: Effect = Effect {
    id: "wxsl.glare_flare",
    label: "lens flare",
    description:
        "Draw a known light's lens flare from its place on screen, hidden as the scene hides it.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[EffectInput {
        name: "depth",
        kind: EffectInputKind::DepthImage,
        description: "The opaque depth, for how much of the light is hidden.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[
        parameter("light", Value::Vec4([0.0; 4])),
        parameter("scales_a", Value::Vec4([-0.4, -0.9, -1.5, 0.5])),
        parameter("scales_b", Value::Vec4([0.0; 4])),
        parameter("sizes_a", Value::Vec4([0.05, 0.03, 0.09, 0.02])),
        parameter("sizes_b", Value::Vec4([0.0; 4])),
        parameter("hexagon", Value::F32(0.0)),
        parameter("squeeze", Value::F32(1.0)),
        parameter("ring", Value::F32(0.0)),
        parameter("ring_strength", Value::F32(0.0)),
        parameter("streak", Value::F32(0.0)),
        parameter("tint", Value::Vec4([1.0; 4])),
        parameter("occluder", Value::F32(0.02)),
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_flare",
        wxsl: include_str!("../../shaders/glare_flare.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// A glare added onto the frame: `glow`, at any size, times `tint` and
/// `strength`, onto `color`.
pub const ADD: Effect = Effect {
    id: "wxsl.glare_add",
    label: "glare add",
    description: "Add a glare, at any resolution, tinted, onto the frame.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[
        image("color", "The frame, as linear radiance."),
        image("glow", "The glare to add, as linear radiance."),
    ],
    outputs: &[],
    parameters: &[
        parameter("strength", Value::F32(1.0)),
        parameter("tint", Value::Vec4([1.0; 4])),
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::glare_add",
        wxsl: include_str!("../../shaders/glare_add.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// A streak's first step, as a share of the height: a texel of a 1080p
/// frame's half-resolution extract. Each next pass steps four times
/// further.
const STEP: f32 = 1.0 / 540.0;

/// How much brighter than a star's threshold a highlight must be to make
/// ghosts: a ghost is a reflection off coated glass, a percent or so of
/// the light, which only the brightest highlights make visible.
const FLARE_CONTRAST: f32 = 5.0;

/// A star through the highlights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Star {
    /// Four spikes on the diagonals: a cross screen filter.
    Cross,
    /// Many thin rays of every strength and length: a small aperture's
    /// diffraction, the brighter the light the more of them.
    Starburst,
    /// Six spikes, each across one edge of a six-bladed iris, with fainter
    /// rays between.
    Hexagonal,
    /// One long horizontal streak, blue: an anamorphic lens.
    Anamorphic,
}

impl Star {
    /// Every star, in the order a slider steps through them.
    pub const ALL: [Star; 4] = [
        Star::Cross,
        Star::Starburst,
        Star::Hexagonal,
        Star::Anamorphic,
    ];

    /// Its name, lower case.
    pub fn name(self) -> &'static str {
        match self {
            Star::Cross => "cross",
            Star::Starburst => "starburst",
            Star::Hexagonal => "hexagonal",
            Star::Anamorphic => "anamorphic",
        }
    }

    /// The [`STARBURST`] parameter `name` it sets, such as how far its
    /// rays reach (`length`) or how strongly they add (`strength`); `None`
    /// for one it leaves at its default, and for a streak, which has no
    /// rays.
    pub fn ray_parameter(self, name: &str) -> Option<f32> {
        let StarShape::Rays(parameters) = self.shape() else {
            return None;
        };
        parameters
            .into_iter()
            .find_map(|(known, value)| match value {
                Value::F32(value) if known == name => Some(value),
                _ => None,
            })
    }

    /// Its [`STARBURST`] parameters, or its streak.
    fn shape(self) -> StarShape {
        let rays = |rays: f32,
                    jitter: f32,
                    spread: f32,
                    degrees: f32,
                    length: f32,
                    base: f32,
                    strength: f32| {
            StarShape::Rays(vec![
                ("rays", Value::F32(rays)),
                ("jitter", Value::F32(jitter)),
                ("spread", Value::F32(spread)),
                ("angle", Value::F32(degrees.to_radians())),
                ("length", Value::F32(length)),
                ("base", Value::F32(base)),
                ("strength", Value::F32(strength)),
            ])
        };
        match self {
            Star::Cross => rays(4.0, 0.0, 0.0, 0.0, 60.0, 4.0, 0.05),
            // A hundred and more rays: on a dim light only the few strong
            // ones show, on the sun all of them.
            Star::Starburst => rays(128.0, 0.9, 1.0, 0.0, 45.0, 4.0, 0.125),
            // Six strong spikes; the spread is small, the jitter none.
            Star::Hexagonal => rays(6.0, 0.0, 0.25, 0.0, 80.0, 4.0, 0.06),
            Star::Anamorphic => StarShape::Streak {
                length: 0.25,
                passes: 4,
                tint: [0.35, 0.55, 1.0],
                strength: 0.6,
            },
        }
    }
}

enum StarShape {
    /// The starburst's parameters.
    Rays(Vec<(&'static str, Value)>),
    /// A horizontal Kawase streak: the share of the height it fades to
    /// 1/e over, its passes (four reach half the height), its tint and
    /// how strongly it adds.
    Streak {
        length: f32,
        passes: u32,
        tint: [f32; 3],
        strength: f32,
    },
}

/// A lens flare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flare {
    /// Soft round ghosts in many colours and a halo ring.
    Ghosts,
    /// Sharp-edged hexagonal ghosts: the highlights through a six-bladed
    /// iris.
    Aperture,
    /// Oval ghosts spread wide and a long blue streak: an anamorphic lens.
    Anamorphic,
}

impl Flare {
    /// Every flare, in the order a slider steps through them.
    pub const ALL: [Flare; 3] = [Flare::Ghosts, Flare::Aperture, Flare::Anamorphic];

    /// Its name, lower case.
    pub fn name(self) -> &'static str {
        match self {
            Flare::Ghosts => "ghosts",
            Flare::Aperture => "aperture",
            Flare::Anamorphic => "anamorphic",
        }
    }

    fn shape(self) -> FlareShape {
        match self {
            Flare::Ghosts => FlareShape {
                blur: 3,
                scales: [-0.35, -0.7, -1.25, -2.0, 0.55, 0.3, -0.18, 1.6],
                squeeze: [1.0, 1.0],
                chroma: 0.008,
                tint: 0.5,
                ring: 0.35,
                hexagon: None,
                streak: false,
                color: [1.0, 1.0, 1.0],
                strength: 0.15,
                light: vec![
                    ("scales_a", Value::Vec4([-0.3, -0.65, -1.1, -1.6])),
                    ("scales_b", Value::Vec4([0.4, 0.2, -0.45, -2.2])),
                    ("sizes_a", Value::Vec4([0.04, 0.07, 0.025, 0.12])),
                    ("sizes_b", Value::Vec4([0.02, 0.015, 0.05, 0.18])),
                    ("ring", Value::F32(0.22)),
                    ("ring_strength", Value::F32(0.08)),
                ],
            },
            Flare::Aperture => FlareShape {
                blur: 2,
                scales: [-0.5, -1.0, -1.8, 0.4, -0.25, -0.7, 0.0, 0.0],
                squeeze: [1.0, 1.0],
                chroma: 0.005,
                tint: 0.6,
                ring: 0.0,
                hexagon: Some(0.03),
                streak: false,
                color: [1.0, 1.0, 1.0],
                strength: 0.25,
                light: vec![
                    ("scales_a", Value::Vec4([-0.25, -0.55, -0.9, -1.35])),
                    ("scales_b", Value::Vec4([0.35, -1.9, 0.6, -0.15])),
                    ("sizes_a", Value::Vec4([0.03, 0.05, 0.08, 0.04])),
                    ("sizes_b", Value::Vec4([0.025, 0.14, 0.02, 0.015])),
                    ("hexagon", Value::F32(1.0)),
                ],
            },
            Flare::Anamorphic => FlareShape {
                blur: 3,
                scales: [-0.5, -1.1, 0.4, -1.8, -0.3, 0.0, 0.0, 0.0],
                squeeze: [0.5, 1.0],
                chroma: 0.006,
                tint: 0.3,
                ring: 0.0,
                hexagon: None,
                streak: true,
                color: [0.55, 0.75, 1.0],
                strength: 0.2,
                light: vec![
                    ("scales_a", Value::Vec4([-0.4, -0.8, -1.3, 0.45])),
                    ("scales_b", Value::Vec4([-1.9, 0.0, 0.0, 0.0])),
                    ("sizes_a", Value::Vec4([0.05, 0.03, 0.07, 0.02])),
                    ("sizes_b", Value::Vec4([0.1, 0.0, 0.0, 0.0])),
                    ("squeeze", Value::F32(0.35)),
                    ("streak", Value::F32(0.6)),
                    ("tint", Value::Vec4([0.6, 0.8, 1.0, 1.0])),
                ],
            },
        }
    }
}

struct FlareShape {
    /// How many times the extract is taken down (halved and blurred)
    /// before the ghosts mirror it: a ghost is the light out of focus, so
    /// a soft blot, never the sharp copy of a lamp.
    blur: u32,
    scales: [f32; 8],
    squeeze: [f32; 2],
    chroma: f32,
    tint: f32,
    ring: f32,
    /// The ghosts blurred through a hexagon this share of the height
    /// across, or not at all.
    hexagon: Option<f32>,
    /// An anamorphic streak through each highlight, with the ghosts.
    streak: bool,
    color: [f32; 3],
    strength: f32,
    /// The known light's flare: [`FLARE`]'s parameters.
    light: Vec<(&'static str, Value)>,
}

/// A target `label` in `format` the size of `like` times `scale`.
fn target_scaled(
    graph: &mut FrameGraph,
    like: ResourceId,
    scale: f32,
    format: TextureFormat,
    label: String,
) -> ResourceId {
    let extent = graph
        .resource_desc(like)
        .and_then(|desc| match desc.shape {
            ResourceShape::Texture { extent, .. } => Some(extent),
            ResourceShape::Buffer { .. } => None,
        })
        .unwrap_or_default();
    let extent = match extent {
        Extent::Viewport { scale: at } => Extent::Viewport { scale: at * scale },
        Extent::Fixed { width, height } => Extent::Fixed {
            width: ((width as f32 * scale).ceil() as u32).max(1),
            height: ((height as f32 * scale).ceil() as u32).max(1),
        },
    };
    graph.resource(ResourceDesc::color(label, format).with_extent(extent))
}

/// A half-float target `label`, the size of `like`.
fn target_like(graph: &mut FrameGraph, like: ResourceId, label: String) -> ResourceId {
    target_scaled(graph, like, 1.0, TextureFormat::Rgba16Float, label)
}

/// One [`STREAK`] pass of `image` into `into`, added onto it when `add`.
#[allow(clippy::too_many_arguments)]
fn streak(
    graph: &mut FrameGraph,
    label: String,
    image: ResourceId,
    into: ResourceId,
    degrees: f32,
    step: f32,
    taps: f32,
    falloff: f32,
    sided: bool,
    add: bool,
) {
    let radians = degrees.to_radians();
    let (attachment, state) = if add {
        (
            Attachment::load(into),
            PassState::FULLSCREEN.with_blend(ADDITIVE),
        )
    } else {
        (
            Attachment::clear(into, Color::TRANSPARENT),
            PassState::FULLSCREEN,
        )
    };
    graph.pass(
        PassDesc::screen(label, STREAK.id)
            .with_color(attachment)
            .with_state(state)
            .with_reads([Read::current(image)])
            .with_parameter("direction", Value::Vec2([radians.cos(), radians.sin()]))
            .with_parameter("step", Value::F32(step))
            .with_parameter("taps", Value::F32(taps))
            .with_parameter("falloff", Value::F32(falloff))
            .with_parameter("sided", Value::F32(if sided { 1.0 } else { 0.0 })),
    );
}

/// The highlights of `color` brighter than `threshold`, at half its size,
/// into a new target named `name`, which is returned.
pub fn append_extract(
    graph: &mut FrameGraph,
    color: ResourceId,
    threshold: f32,
    name: &str,
) -> ResourceId {
    let extract = target_scaled(graph, color, 0.5, TextureFormat::Rgba16Float, name.into());
    graph.pass(
        PassDesc::screen(name, EXTRACT.id)
            .with_color(Attachment::clear(extract, Color::TRANSPARENT))
            .with_reads([Read::current(color)])
            .with_parameter("threshold", Value::F32(threshold)),
    );
    extract
}

/// A horizontal streak, `length` of the height long, through the
/// highlights of `extract`, in `passes` Kawase passes, into a new target
/// the size of `extract` named after `name`, which is returned.
pub fn append_streak(
    graph: &mut FrameGraph,
    extract: ResourceId,
    length: f32,
    passes: u32,
    name: &str,
) -> ResourceId {
    let mut source = extract;
    for pass in 0..passes {
        let step = STEP * 4f32.powi(pass as i32);
        let into = target_like(graph, extract, format!("{name} {pass}"));
        streak(
            graph,
            format!("{name} {pass}"),
            source,
            into,
            0.0,
            step,
            7.0,
            length / step,
            false,
            false,
        );
        source = into;
    }
    source
}

/// `star` through the highlights of `extract` (from [`append_extract`]
/// on `color`), added onto `color` under the pass labelled `name`: a
/// starburst's sprites, drawn into a target of their own (drawn onto
/// `color` itself, they would write what the extract read), or a streak.
/// Returns the frame.
pub fn append_star(
    graph: &mut FrameGraph,
    color: ResourceId,
    extract: ResourceId,
    star: Star,
    name: &str,
) -> ResourceId {
    match star.shape() {
        StarShape::Rays(parameters) => {
            let cells = target_scaled(
                graph,
                extract,
                1.0 / 4.0,
                TextureFormat::Rgba32Float,
                format!("{name} cells"),
            );
            graph.pass(
                PassDesc::screen(format!("{name} cells"), CELLS.id)
                    .with_color(Attachment::clear(cells, Color::TRANSPARENT))
                    .with_reads([Read::current(extract)]),
            );
            let rays = target_like(graph, color, format!("{name} rays"));
            let pass = PassDesc::screen(format!("{name} rays"), STARBURST.id)
                .with_color(Attachment::clear(rays, Color::TRANSPARENT))
                .with_state(PassState::FULLSCREEN.with_blend(ADDITIVE))
                .with_reads([Read::current(cells), Read::current(extract)]);
            graph.pass(
                parameters
                    .into_iter()
                    .fold(pass, |pass, (name, value)| pass.with_parameter(name, value)),
            );
            append_add(graph, color, rays, [1.0; 3], 1.0, name.to_string())
        }
        StarShape::Streak {
            length,
            passes,
            tint,
            strength,
        } => {
            let streak = append_streak(graph, extract, length, passes, &format!("{name} streak"));
            append_add(graph, color, streak, tint, strength, name.to_string())
        }
    }
}

/// `image` blurred through a hexagon `size` of the height across its
/// corners, into a new target named `name`, which is returned: three
/// one-sided boxes, 120° apart, compose pairwise into the three rhombi
/// that tile a hexagon (White and Barré-Brisebois, SIGGRAPH 2011), their
/// sum three times the image's light.
pub fn append_hexagon(
    graph: &mut FrameGraph,
    image: ResourceId,
    size: f32,
    name: &str,
) -> ResourceId {
    const TAPS: f32 = 8.0;
    let step = size * 0.5 / TAPS;
    let sides = [90.0, 210.0, 330.0];
    let first: Vec<ResourceId> = sides
        .iter()
        .enumerate()
        .map(|(index, &degrees)| {
            let into = target_like(graph, image, format!("{name} side {index}"));
            streak(
                graph,
                format!("{name} side {index}"),
                image,
                into,
                degrees,
                step,
                TAPS,
                0.0,
                true,
                false,
            );
            into
        })
        .collect();
    let result = target_like(graph, image, name.to_string());
    for index in 0..3 {
        // The rhombus between side `index` and the next.
        streak(
            graph,
            format!("{name} rhombus {index}"),
            first[index],
            result,
            sides[(index + 1) % 3],
            step,
            TAPS,
            0.0,
            true,
            index > 0,
        );
    }
    result
}

/// The pass label [`append`]'s star draws or adds under.
pub fn star_label(name: &str) -> String {
    format!("{name} star")
}

/// The pass label [`append`]'s flare adds its ghosts under, for their
/// `strength`.
pub fn flare_label(name: &str) -> String {
    format!("{name} flare add")
}

/// The pass label [`append`] draws the known light's flare under: its
/// `light` parameter, the direction towards the light and its strength,
/// is the host's to set every frame.
pub fn light_label(name: &str) -> String {
    format!("{name} flare light")
}

/// `flare`'s ghosts (and ring, hexagon, streak) of the highlights of
/// `extract` brighter than `threshold` times [`FLARE_CONTRAST`], added
/// onto `color` under [`flare_label`]; then the known light's flare
/// drawn onto the result under [`light_label`], hidden by `depth`.
/// Returns the frame.
pub fn append_flare(
    graph: &mut FrameGraph,
    color: ResourceId,
    depth: ResourceId,
    extract: ResourceId,
    flare: Flare,
    threshold: f32,
    name: &str,
) -> ResourceId {
    let shape = flare.shape();
    let mut source = extract;
    for level in 0..shape.blur {
        let down = target_scaled(
            graph,
            source,
            0.5,
            TextureFormat::Rgba16Float,
            format!("{name} soft {level}"),
        );
        graph.pass(
            PassDesc::screen(format!("{name} soft {level}"), super::BLOOM_DOWN.id)
                .with_color(Attachment::clear(down, Color::TRANSPARENT))
                .with_reads([Read::current(source)]),
        );
        source = down;
    }
    // At a quarter of the frame whatever the blur, so the soft source's
    // texels are filtered once more before they are added.
    let ghosts = target_scaled(
        graph,
        extract,
        0.5,
        TextureFormat::Rgba16Float,
        format!("{name} ghosts"),
    );
    let [a, b] = [&shape.scales[..4], &shape.scales[4..]]
        .map(|half| Value::Vec4([half[0], half[1], half[2], half[3]]));
    graph.pass(
        PassDesc::screen(format!("{name} ghosts"), GHOSTS.id)
            .with_color(Attachment::clear(ghosts, Color::TRANSPARENT))
            .with_reads([Read::current(source)])
            .with_parameter("scales_a", a)
            .with_parameter("scales_b", b)
            .with_parameter("squeeze", Value::Vec2(shape.squeeze))
            .with_parameter("chroma", Value::F32(shape.chroma))
            .with_parameter("tint", Value::F32(shape.tint))
            .with_parameter("ring", Value::F32(shape.ring))
            .with_parameter("threshold", Value::F32(threshold * FLARE_CONTRAST)),
    );
    let mut strength = shape.strength;
    let mut glare = ghosts;
    if let Some(size) = shape.hexagon {
        glare = append_hexagon(graph, ghosts, size, &format!("{name} hexagon"));
        strength /= 3.0;
    }
    let mut color = append_add(
        graph,
        color,
        glare,
        shape.color,
        strength,
        flare_label(name),
    );
    if shape.streak {
        if let StarShape::Streak {
            length,
            passes,
            tint,
            strength,
        } = Star::Anamorphic.shape()
        {
            let streak = append_streak(graph, extract, length, passes, &format!("{name} streak"));
            color = append_add(
                graph,
                color,
                streak,
                tint,
                strength,
                format!("{name} streak add"),
            );
        }
    }
    let pass = PassDesc::screen(light_label(name), FLARE.id)
        .with_color(Attachment::load(color))
        .with_state(PassState::FULLSCREEN.with_blend(ADDITIVE))
        .with_reads([Read::current(depth)]);
    graph.pass(
        shape
            .light
            .into_iter()
            .fold(pass, |pass, (name, value)| pass.with_parameter(name, value)),
    );
    color
}

/// `glow` times `tint` and `strength` added onto `color`, into a new
/// target the size of `color` labelled `label`, which is returned.
pub fn append_add(
    graph: &mut FrameGraph,
    color: ResourceId,
    glow: ResourceId,
    tint: [f32; 3],
    strength: f32,
    label: String,
) -> ResourceId {
    let into = target_like(graph, color, label.clone());
    graph.pass(
        PassDesc::screen(label, ADD.id)
            .with_color(Attachment::clear(into, Color::TRANSPARENT))
            .with_reads([Read::current(color), Read::current(glow)])
            .with_parameter("strength", Value::F32(strength))
            .with_parameter("tint", Value::Vec4([tint[0], tint[1], tint[2], 1.0])),
    );
    into
}

/// The passes and parameters to set for [`append`]'s glare, named `name`,
/// to start at `threshold` rather than the threshold it was built with:
/// (pass label, parameter, value). A pass the glare does not have (no
/// flare, say) is the host's to skip.
pub fn threshold_parameters(name: &str, threshold: f32) -> [(String, &'static str, Value); 2] {
    [
        (
            format!("{name} extract"),
            "threshold",
            Value::F32(threshold),
        ),
        (
            format!("{name} flare ghosts"),
            "threshold",
            Value::F32(threshold * FLARE_CONTRAST),
        ),
    ]
}

/// What [`append`] adds: a star, a flare, and the luminance past which a
/// highlight glares (a flare's ghosts asking [`FLARE_CONTRAST`] times
/// more).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    /// The star through the highlights, or none.
    pub star: Option<Star>,
    /// The lens flare, or none.
    pub flare: Option<Flare>,
    /// The linear radiance past which a highlight glares.
    pub threshold: f32,
}

/// `lens`' star and flare, either or both, from the highlights of `color`
/// (the frame, its opaque depth `depth`), added onto it, into targets
/// named after `name`; the frame is returned, `color` itself when
/// neither is asked for.
pub fn append(
    graph: &mut FrameGraph,
    color: ResourceId,
    depth: ResourceId,
    lens: Lens,
    name: &str,
) -> ResourceId {
    if lens.star.is_none() && lens.flare.is_none() {
        return color;
    }
    let extract = append_extract(graph, color, lens.threshold, &format!("{name} extract"));
    let mut color = color;
    if let Some(star) = lens.star {
        color = append_star(graph, color, extract, star, &star_label(name));
    }
    if let Some(flare) = lens.flare {
        color = append_flare(
            graph,
            color,
            depth,
            extract,
            flare,
            lens.threshold,
            &format!("{name} flare"),
        );
    }
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_a_distinct_name_and_a_shape() {
        let stars: Vec<_> = Star::ALL.iter().map(|star| star.name()).collect();
        let flares: Vec<_> = Flare::ALL.iter().map(|flare| flare.name()).collect();
        for names in [&stars, &flares] {
            let mut unique = names.to_vec();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), names.len());
        }
        // Every parameter a model sets is one its effect declares.
        for star in Star::ALL {
            if let StarShape::Rays(parameters) = star.shape() {
                for (name, _) in parameters {
                    assert!(
                        STARBURST.parameters.iter().any(|known| known.name == name),
                        "{name}"
                    );
                }
            }
        }
        for flare in Flare::ALL {
            let shape = flare.shape();
            assert!(shape.scales.iter().any(|scale| *scale != 0.0));
            for (name, _) in shape.light {
                assert!(
                    FLARE.parameters.iter().any(|known| known.name == name),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn a_star_and_a_flare_add_onto_the_frame() {
        let mut graph = FrameGraph::new(TextureFormat::Rgba16Float);
        let hdr = TextureFormat::Rgba16Float;
        let color = graph.resource(ResourceDesc::color("color", hdr));
        let depth = graph.resource(ResourceDesc::color("depth", hdr));
        let none = Lens {
            star: None,
            flare: None,
            threshold: 8.0,
        };
        assert_eq!(append(&mut graph, color, depth, none, "glare"), color);
        assert!(graph.passes().is_empty());
        let lens = Lens {
            star: Some(Star::Starburst),
            flare: Some(Flare::Aperture),
            ..none
        };
        let out = append(&mut graph, color, depth, lens, "glare");
        assert_ne!(out, color);
        let labels: Vec<&str> = graph
            .passes()
            .iter()
            .map(|pass| pass.label.as_str())
            .collect();
        // The extract; the cells, the sprites and their add; two blurs,
        // the ghosts, six hexagon passes and their add; the light's flare.
        assert_eq!(labels.len(), 1 + 3 + 2 + 1 + 6 + 1 + 1, "{labels:?}");
        let extent = |id| match graph.resource_desc(id).map(|desc| &desc.shape) {
            Some(ResourceShape::Texture { extent, .. }) => *extent,
            _ => panic!("a texture"),
        };
        let extract = graph
            .resource_by_label("glare extract")
            .expect("the extract");
        assert_eq!(extent(extract), Extent::Viewport { scale: 0.5 });
        let cells = graph
            .resource_by_label("glare star cells")
            .expect("the cells");
        assert_eq!(extent(cells), Extent::Viewport { scale: 0.125 });
        let rays = graph
            .passes()
            .iter()
            .find(|pass| pass.label == format!("{} rays", star_label("glare")))
            .expect("the starburst");
        assert_eq!(rays.state.blend, Some(ADDITIVE));
        let star = graph
            .passes()
            .iter()
            .find(|pass| pass.label == star_label("glare"))
            .expect("the starburst's add");
        assert_eq!(star.reads[0].resource, color);
        assert_eq!(star.reads[1].resource, rays.color[0].resource);
        let light = graph
            .passes()
            .iter()
            .find(|pass| pass.label == light_label("glare flare"))
            .expect("the light's flare");
        assert_eq!(light.color[0].resource, out);
        assert_eq!(light.reads[0].resource, depth);
        // The threshold can be retuned on passes the glare has.
        for (label, parameter, _) in threshold_parameters("glare", 4.0) {
            let pass = graph
                .passes()
                .iter()
                .find(|pass| pass.label == label)
                .expect(&label);
            assert!(pass.parameters.contains_key(parameter), "{label}");
        }
    }
}
