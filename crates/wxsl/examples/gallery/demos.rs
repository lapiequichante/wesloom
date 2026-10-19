//! The demo catalogue: what each one shows, and on which pipeline.

use super::*;

/// Where a demo's pass list comes from: a stock preset, or a document
/// built here and compiled by the public pipeline compiler. Every demo is
/// a document now — the compute proofs included, whose passes are
/// `pass.compute.<effect>` nodes since plan3 N3.
pub(crate) enum Pipeline {
    Stock(StockPipeline),
    Document(fn() -> Graph),
}

impl Pipeline {
    /// The pipeline's name, for titles and status lines.
    pub(crate) fn name(&self) -> String {
        match self {
            Pipeline::Stock(stock) => stock.name().to_string(),
            Pipeline::Document(build) => build().name().to_string(),
        }
    }
}

/// One entry in the gallery: a name, a pipeline, and how to light it.
pub(crate) struct Demo {
    pub(crate) name: &'static str,
    pub(crate) blurb: &'static str,
    pub(crate) pipeline: Pipeline,
    /// How many copies of the cube: one, or a row.
    pub(crate) instances: u32,
    /// The key light's intensity. The bloom demo needs a highlight that
    /// crosses the threshold — a scene lit normally has nothing to glow.
    pub(crate) key_intensity: f32,
    /// The material features the demo's pipeline enables (plan2 P12).
    pub(crate) features: &'static [&'static str],
    /// Which lighting model shades the shared cube, when the demo's is
    /// not the stock default. Naming a model demands the *set* carry it,
    /// so entering such a demo widens the renderer's lighting set and
    /// leaving restores the stock one — the recompile that costs is the
    /// demo switch itself.
    pub(crate) model: Option<&'static str>,
    /// The macro pins the demo's material resolves with — the subsurface
    /// demo's strength, pinned like any graph macro (plan3 N6).
    pub(crate) macros: &'static [(&'static str, wxsl::core::macros::MacroValue)],
    /// A sky to light the demo by instead of the lights: sky above,
    /// bounce below, with the lamps switched off. What is left is
    /// `ambient_environment` alone, and therefore the environment-BRDF
    /// table it reads (ADR 0039).
    pub(crate) sky: Option<(Vec3, Vec3)>,
    /// The demo's effect-parameter overrides, as (pass label, parameter,
    /// value) — applied through `Renderer::set_pass_param` once the pass
    /// list is set. What an editor canvas's sliders would drive (ADR 0042).
    pub(crate) params: &'static [(&'static str, &'static str, f32)],
    /// A material graph of the demo's own, instead of the shared PBR cube
    /// — one demo, `bake`, needs a material that *declares a bake* (ADR
    /// 0045). `None` is the shared cube, and every other demo's image
    /// comes from it unchanged.
    pub(crate) material: Option<fn() -> Graph>,
    /// How the demo moves, when it moves at all: the transform at `t`,
    /// for the demos whose cube has a *velocity* — the previous-frame
    /// transform the velocity stage reads is this path one frame back.
    /// `None` is the static spin every other demo draws, and a draw with
    /// no previous is zero motion — today's behaviour (plan3 N2).
    pub(crate) motion: Option<Motion>,
    /// How many frames render before the captured one, so a temporal
    /// pipeline's screenshot shows a settled history rather than frame
    /// one's empty ring. The live view needs none of this: it runs
    /// continuously.
    pub(crate) warmup: u32,
}

/// One demo's way of moving: where the cube is at `t`, and the frame step
/// its previous-frame transform answers for in a screenshot. The step is
/// the shutter a motion blur is the average over — a screenshot has no
/// real frame times to measure, so the demo states one.
#[derive(Clone, Copy)]
pub(crate) struct Motion {
    pub(crate) step: f32,
    pub(crate) path: fn(f32) -> Mat4,
}

pub(crate) fn demos() -> Vec<Demo> {
    vec![
        Demo {
            name: "forward",
            blurb: "the stock forward pipeline: depth prepass, then shade",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "deferred",
            blurb: "the stock deferred pipeline: G-buffer, then a lighting pass",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "single-pass",
            blurb: "the minimal forward document: one pass, its own depth, no shadows",
            pipeline: Pipeline::Document(minimal_forward_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "deferred-bloom",
            blurb: "deferred lighting into a colour target, then a separable bloom — \
                    blur along x, then along y — as a document edit",
            pipeline: Pipeline::Document(deferred_bloom_document),
            instances: 1,
            // A highlight bright enough to cross bloom's threshold.
            key_intensity: 160.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-instances",
            blurb: "FXAA, then the separable bloom, six tinted copies from one draw list",
            pipeline: Pipeline::Document(bloom_instances_document),
            instances: 6,
            key_intensity: 160.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-tuned",
            blurb: "the same chain with bloom's parameters tuned live — set_pass_param \
                    moves threshold on the horizontal pass and strength on the vertical \
                    one, with no recompile",
            pipeline: Pipeline::Document(deferred_bloom_document),
            instances: 1,
            key_intensity: 160.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            // The threshold below the lit surface's radiance, so the whole
            // scene glows, and the strength above one, so the glow leads —
            // visibly not the default image, from the same document.
            params: &[
                ("bloom", "threshold", 0.35),
                ("bloom-y", "strength", 1.6),
            ],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-pyramid",
            blurb: "deferred lighting into a colour target, then `pass.bloom` — the \
                    whole pyramid, extract to combine, expanded from one document node \
                    (ADR 0065)",
            pipeline: Pipeline::Document(deferred_pyramid_document),
            instances: 1,
            key_intensity: 160.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-pyramid-tuned",
            blurb: "the pyramid with its parameters tuned live — the expansion's stable \
                    labels are the contract: threshold and knee on `bloom extract`, \
                    strength on `bloom combine`",
            pipeline: Pipeline::Document(deferred_pyramid_document),
            instances: 1,
            key_intensity: 160.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[
                ("bloom extract", "threshold", 0.35),
                ("bloom extract", "knee", 0.3),
                ("bloom combine", "strength", 1.6),
            ],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "dof",
            blurb: "`pass.dof` over a row of copies, focused on the middle one — signed \
                    circles of confusion, an occlusion-faded far blur, a dilated near \
                    composite (ADR 0065)",
            pipeline: Pipeline::Document(dof_document),
            instances: 3,
            key_intensity: 90.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            // The camera sits ~4.4 m from the origin; the row spreads
            // copies at roughly 3.7 and 6.0 m, so f/1.4 on an 85 mm puts
            // the outer copies visibly out of focus and the middle one
            // not at all.
            params: &[
                ("dof circles", "focus", 4.4),
                ("dof circles", "focal", 0.085),
                ("dof circles", "f_number", 1.4),
                ("dof circles", "near", 0.1),
                ("dof circles", "far", 100.0),
                ("dof near blur", "max_coc", 14.0),
            ],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "shadow",
            blurb: "a cube over a ground plane, lit by a sun that casts: the shadow \
                    atlas — one depth texture, a tile per light slot, the key slots \
                    the largest (ADR 0066)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "cascade",
            blurb: "a sun split into four cascades fitted to the camera — near \
                    shadows sharp, far ones coarse, and every box follows the \
                    view (ADR 0071)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "point-shadow",
            blurb: "a point light casting through all six cube faces — six atlas \
                    tiles per light, the fragment picks its face by direction \
                    (ADR 0071)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "area-shadow",
            blurb: "a one-sided rectangular emitter with sixteen shadowed quadrature points — finite-area lighting and penumbrae (ADR 0072)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1, key_intensity: 42.0, features: &[], macros: &[],
            model: None, sky: None,
            params: &[], material: None, motion: None, warmup: 0,
        },
        Demo {
            name: "area-texture",
            blurb: "a rectangle emitting a texture drawn in WXSL, no shadow: its samples \
                    follow each point's specular lobe, so the glossy floor reflects the \
                    panel prefiltered by roughness instead of sixteen dots (ADR 0095)",
            pipeline: Pipeline::Document(deferred_pyramid_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "neon",
            blurb: "neon tube lights — glowing cylinders sampled along their axis around \
                    the specular lobe, one with a WXSL gradient — streaking a glossy \
                    floor, under bloom (ADR 0095)",
            pipeline: Pipeline::Document(deferred_pyramid_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "spot-shadow", blurb: "a focused cone with soft angular falloff and one shadow tile (ADR 0073)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred), instances: 1, key_intensity: 42.0,
            features: &[], macros: &[], model: None, sky: None, params: &[], material: None, motion: None, warmup: 0,
        },
        Demo {
            name: "spot-cookie", blurb: "a textured spotlight: a projected window mask with cone falloff and shadows (ADR 0073)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred), instances: 1, key_intensity: 42.0,
            features: &[], macros: &[], model: None, sky: None, params: &[], material: None, motion: None, warmup: 0,
        },
        Demo {
            name: "sixteen",
            blurb: "sixteen coloured point lights in a ring — the light budget \
                    the scene uniform carries, every row of it reaching the \
                    shader (ADR 0072)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "clustered",
            blurb: "a thousand ranged lamps drifting over the floor — local lights \
                    binned into the camera's light grid, each pixel looping only \
                    over the lamps that reach it (ADR 0076)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "brdf-lut",
            blurb: "a compute effect bakes the split-sum BRDF LUT once (policy: once), \
                    and a per-frame view displays it — the execution-policy proof, \
                    as a document",
            pipeline: Pipeline::Document(brdf_lut_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "buffer-ramp",
            blurb: "a compute effect fills a storage buffer and a screen effect reads \
                    it as storage — buffers are graph resources, and the proof compiles \
                    from a document",
            pipeline: Pipeline::Document(buffer_ramp_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "subsurface",
            blurb: "the subsurface model over the subsurface feature: the G-buffer \
                    channel feeds wrapped diffuse and transmission through the \
                    dispatch under the feature's macro — with the feature off, \
                    the same arm shades plain PBR",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &["subsurface"],
            macros: &[
                ("wxsl_subsurface", wxsl::core::macros::MacroValue::Flag(true)),
                (
                    "wxsl_subsurface_strength",
                    wxsl::core::macros::MacroValue::Float(0.8),
                ),
            ],
            model: Some("wxsl.subsurface"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "cloth",
            blurb: "the same cube shaded by the cloth model: a roughness-wrapped                     diffuse and a Charlie sheen layer, entering the wide                     lighting set for the one demo that needs it",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: Some("wxsl.cloth"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl",
            blurb: "no lamps: HDR image or single-scattering sky, diffuse convolution and GGX roughness mips with split-sum BRDF",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            macros: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl-mirror",
            blurb: "mirror sphere: roughness 0, metallic 1, white F0; lit only by the HDR environment",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            macros: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "glass",
            blurb: "clear glass sphere and blue smoked torus: layered IOR refraction, Fresnel and absorption over HDR; orbit to overlap",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 2,
            key_intensity: 0.0,
            features: &[], macros: &[], model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[], material: None, motion: None, warmup: 0,
        },
        Demo {
            name: "glass-probes",
            blurb: "triangle of opaque glass sphere, blue torus and mirror; two cached probe generations for mutual reflection/refraction",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 3,
            key_intensity: 0.0,
            features: &[], macros: &[], model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[], material: None, motion: None, warmup: 0,
        },
        Demo {
            name: "ibl-grid",
            blurb: "5x5 stationary spheres: roughness 0→1 left to right, metallic 0→1 front to back; matte grey floor, HDR image IBL only",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 25,
            key_intensity: 0.0,
            features: &[],
            macros: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl-grid-sky",
            blurb: "the same 5x5 roughness/metallic grid and matte grey floor, lit only by the Rayleigh/Mie sky",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 25,
            key_intensity: 0.0,
            features: &[],
            macros: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "sky",
            blurb: "equirectangular single-scattering sky: Rayleigh/Mie, optical depth and planet shadow; also available as the IBL source",
            pipeline: Pipeline::Document(sky_document),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bake",
            blurb: "a twelve-octave noise term baked to a 256x256 table once (policy: \
                    once) and sampled back — ADR 0045's toggle: baked and inline \
                    draw the same picture, and only the cost moves",
            pipeline: Pipeline::Document(bake_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: Some(bake_material_graph),
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "fxaa",
            blurb: "the stock forward chain with an anti-aliasing pass appended — and                     both screen effects in it are node graphs, not shader files",
            pipeline: Pipeline::Document(fxaa_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "taa",
            blurb: "the spinning cube under TAA: a velocity pass, a history \
                    ring, and a resolve whose disocclusion answer is a clamp — \
                    the plan3 N2 chain as a document",
            pipeline: Pipeline::Document(taa_document),
            instances: 1,
            key_intensity: 22.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            // The cube spins, so its velocity is real; the history needs
            // frames behind it before the captured one shows what the
            // ring accumulated.
            motion: Some(Motion {
                step: 1.0 / 60.0,
                path: cube_transform,
            }),
            warmup: 24,
        },
        Demo {
            name: "motion-blur",
            blurb: "a cube swept across the frame, smeared along its own \
                    velocity — the first consumer of the velocity buffer that \
                    is not TAA",
            pipeline: Pipeline::Document(motion_blur_document),
            instances: 1,
            key_intensity: 22.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: Some(Motion {
                step: 1.0 / 60.0,
                path: swept_transform,
            }),
            warmup: 0,
        },
        Demo {
            name: "peel",
            blurb: "a torus and a blue sphere through each other, the PBR cube shader \
                    with its alpha uniform at 0.3, composited by dual depth peeling",
            pipeline: Pipeline::Document(peel_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "render-order",
            blurb: "three cards in one blended pass — an order -1 underlay, a sheet, \
                    and an order-1 hologram *behind it in depth* yet drawn after it: \
                    the group order is the draws' data (plan5 D4)",
            pipeline: Pipeline::Document(order_document),
            instances: 1,
            // The cards face away from the key light's angle; the point
            // is the composite, so light the sheet up.
            key_intensity: 90.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "sdf-aa",
            blurb: "an AA'd checker and an SDF disc whose edge is one screen-space \
                    pixel wide — checker_aa and sdf_coverage fed by screen_width, \
                    the fragment-only family (plan5 D6)",
            pipeline: Pipeline::Document(alpha_over_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "iridescence",
            blurb: "a thin-film sphere: graph-authored thickness carried by an HDR \
                    model channel into the spectral specular light loop (ADR 0058)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: Some("wxsl.iridescent"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "sheen",
            blurb: "a PBR sphere under graph-authored sheen: tint and roughness \
                    carried into the Charlie light loop and analytic ambient (ADR 0059)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            macros: &[],
            model: Some("wxsl.sheen"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
    ]
}
