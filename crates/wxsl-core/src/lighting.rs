//! The lighting-model registry, and the two shaders generated from it.
//!
//! A *lighting model* is one WXSL function of one fixed signature plus a
//! small integer id. A material declares which one shades it; a deferred
//! pipeline enables a *set* of them, packs each fragment's choice into a
//! G-buffer channel, and its lighting pass dispatches on that id. The
//! forward path needs no dispatch at all — it shades where the surface is
//! evaluated, so each material simply calls its own model.
//!
//! The registry entry is the seam. Nothing downstream — the generated
//! dispatch, the G-buffer layout, the lighting pass — can tell a
//! hand-written model from one generated out of a graph, so adding
//! graph-authored models later is a new *producer* of entries, not a
//! redesign of any of this.
//!
//! # The model contract
//!
//! Every model function has the signature, in WXSL spelling:
//!
//! ```text
//! fn <function>(surface: Surface, ctx: SurfaceContext, light: LightSample, extra: vec4f) -> vec3f
//! ```
//!
//! `surface` is what the material produced, `ctx` the per-fragment inputs,
//! `light` the one light being contributed, and `extra` the texel of the
//! model's requested G-buffer target — or zero for a model that requested
//! none. One `vec4f` is enough because the G-buffer's own convention is
//! packing two quantities per target; a model that outgrows it is a schema
//! change recorded here, not a per-model special case.
//!
//! A model that requests a target also names a *pack* function in its
//! module, `fn <pack>(surface: Surface) -> vec4f`, which the material's
//! G-buffer stage calls to fill it. The lighting side reads the texel back
//! and hands it to the model as `extra`.
//!
//! # What is generated
//!
//! Three things, all data-driven and all in this module so there is one
//! place that knows the shape:
//!
//! * the per-light dispatch plus the whole [`abi::SHADE_SURFACE_FN`] — the
//!   light loop, ambient, exposure and tonemap — which replaced the
//!   hand-written `shading.wxsl`, because the call inside the loop is the
//!   one thing that varies with the enabled set;
//! * the [`abi::GBUFFER_STRUCT`] struct and [`abi::PACK_GBUFFER_FN`] — its
//!   fields are the set's layout, so they cannot be fixed shipped text;
//! * the whole deferred lighting pass, including
//!   [`abi::UNPACK_GBUFFER_FN`] and a `switch` over the id.
//!
//! A model nobody enabled is not even imported, so it costs nothing — and
//! dead-code elimination (ADR 0012) removes what conditional translation
//! leaves behind.

use core::fmt;
use std::fmt::Write as _;

use crate::abi::{self, GBufferPrecision, GBufferTarget};
use crate::wxsl::stable_hash;

/// The field name of the G-buffer target carrying the dispatch id.
pub const MODEL_ID_FIELD: &str = "lighting";

/// The G-buffer target the dispatch id rides in, when a set dispatches.
///
/// One normalized scalar channel, with the id scaled by
/// [`MODEL_ID_SCALE`] on each side: a normalized target divides by 255 on
/// store and multiplies back on load, and `id/255` survives that round
/// trip exactly. One byte of the attachment budget, because a whole
/// `vec4` target would cost eight of it for three empty channels — and
/// the base targets have nearly spent the budget already.
pub const MODEL_ID_TARGET: GBufferTarget = GBufferTarget {
    field: MODEL_ID_FIELD,
    doc: "lighting model id / 255 (see LightingSet)",
    precision: GBufferPrecision::NormalizedScalar,
};

/// The scale the dispatch id travels at, in [`MODEL_ID_TARGET`].
pub const MODEL_ID_SCALE: f32 = 255.0;

/// The G-buffer channels a model may ask for, and how they are filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelExtra {
    /// The requested render target, packed after the base targets and any
    /// earlier model's request.
    pub target: GBufferTarget,
    /// Name of the function in the model's module that fills the target
    /// from the surface: `fn <pack>(surface: Surface) -> vec4f`.
    pub pack: &'static str,
}

/// One lighting model: a registry entry, and the whole contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LightingModel {
    /// The id written into the G-buffer and switched over. Unique within a
    /// set, and stable across sessions — a saved scene names models, so
    /// renumbering one silently reshades everything that used it.
    pub id: u32,
    /// Short name, as a scene document and a command line spell it.
    pub name: &'static str,
    /// WXSL module path holding `function` (and `extra`'s pack).
    pub module: &'static str,
    /// The model function, per the contract in this module's docs.
    pub function: &'static str,
    /// The G-buffer target this model asks for, if any.
    pub extra: Option<ModelExtra>,
    /// What the model is, for editors and diagnostics.
    pub doc: &'static str,
}

impl LightingModel {
    /// The registry entry as a cache-key and label string.
    fn signature(&self) -> String {
        format!("{}:{}", self.name, self.id)
    }
}

/// Something is wrong with a set of lighting models.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LightingError {
    /// Two entries claim the same id, so one would silently win the
    /// dispatch.
    DuplicateId {
        /// The contested id.
        id: u32,
    },
    /// Two entries share a name, so a scene naming one is ambiguous.
    DuplicateName {
        /// The contested name.
        name: String,
    },
    /// The set is empty, which no pipeline can draw with.
    Empty,
    /// A material names a model the set does not enable.
    UnknownModel {
        /// The name that was asked for.
        name: String,
    },
}

impl fmt::Display for LightingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LightingError::DuplicateId { id } => {
                write!(f, "two lighting models claim id {id}")
            }
            LightingError::DuplicateName { name } => {
                write!(f, "two lighting models are named `{name}`")
            }
            LightingError::Empty => write!(f, "a lighting model set cannot be empty"),
            LightingError::UnknownModel { name } => {
                write!(f, "no lighting model named `{name}` is enabled")
            }
        }
    }
}

impl std::error::Error for LightingError {}

/// The lighting models a pipeline enables.
///
/// A *set*, never a global: two renderers in one process may run different
/// sets, and the set decides the G-buffer's shape, so it is part of building
/// a pipeline rather than of the ABI. Kept sorted by id, which is the order
/// the G-buffer's requested targets are packed in and the order the
/// generated switch lists its arms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightingSet {
    models: Vec<LightingModel>,
}

impl Default for LightingSet {
    /// The default: the library's default model in a set of one.
    fn default() -> Self {
        default_single_set()
    }
}

impl LightingSet {
    /// Validate and build a set from `models`.
    ///
    /// Sorted by id here rather than trusted: the G-buffer layout and the
    /// generated dispatch both read in this order, and a set built twice
    /// from the same entries in a different order should be the same set.
    pub fn new(models: impl IntoIterator<Item = LightingModel>) -> Result<Self, LightingError> {
        let mut models: Vec<_> = models.into_iter().collect();
        if models.is_empty() {
            return Err(LightingError::Empty);
        }
        models.sort_by_key(|model| model.id);
        for (index, model) in models.iter().enumerate() {
            if models
                .iter()
                .skip(index + 1)
                .any(|other| other.id == model.id)
            {
                return Err(LightingError::DuplicateId { id: model.id });
            }
        }
        let mut names: Vec<&str> = models.iter().map(|model| model.name).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() != models.len() {
            let duplicated = models
                .iter()
                .find(|model| {
                    models
                        .iter()
                        .filter(|other| other.name == model.name)
                        .count()
                        > 1
                })
                .map(|model| model.name)
                .unwrap_or_default();
            return Err(LightingError::DuplicateName {
                name: duplicated.to_string(),
            });
        }
        Ok(LightingSet { models })
    }

    /// A set of exactly one model: no dispatch, no id channel.
    ///
    /// The shape every deferred pipeline had before models existed, and
    /// still the default — it keeps the G-buffer at the base three targets
    /// until someone asks for more.
    pub fn single(model: LightingModel) -> Self {
        LightingSet {
            models: vec![model],
        }
    }

    /// The enabled models, in id order.
    pub fn models(&self) -> &[LightingModel] {
        &self.models
    }

    /// How many models are enabled.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether no model is enabled, which [`LightingSet::new`] prevents.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// The model with `id`, if the set enables one.
    pub fn get(&self, id: u32) -> Option<&LightingModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// The model named `name`, as a scene document spells it.
    pub fn by_name(&self, name: &str) -> Option<&LightingModel> {
        self.models.iter().find(|model| model.name == name)
    }

    /// The model a material gets when it does not name one: the lowest id.
    ///
    /// An order, not a flag — a `default` bit would be a second thing to
    /// keep consistent with the ids, and the ids already order the set.
    pub fn default_model(&self) -> &LightingModel {
        // `new` rejects the empty set; `single` cannot build one.
        &self.models[0]
    }

    /// Whether fragments must carry an id for the lighting pass to
    /// dispatch on: more than one model is enabled.
    pub fn dispatches(&self) -> bool {
        self.models.len() > 1
    }

    /// The models that ask for a G-buffer target, in id order.
    pub fn extras(&self) -> Vec<ModelExtra> {
        self.models.iter().filter_map(|model| model.extra).collect()
    }

    /// The G-buffer layout this set needs, in `@location` order: the base
    /// targets, then the id channel if the set dispatches, then each
    /// requesting model's target in id order.
    ///
    /// This is the single answer to "what does the deferred path attach,
    /// bind and read" — `wxsl-render` declares its resources from it, the
    /// generated struct is written from it, and the lighting pass binds in
    /// the same order, so the three cannot drift apart short of a change
    /// here. Its cost against the attachment budget is
    /// `wxsl-render`'s `gbuffer_bytes_per_sample`, which is what tells a
    /// set that asked for more than fits.
    pub fn gbuffer_layout(&self) -> Vec<GBufferTarget> {
        let mut layout: Vec<GBufferTarget> = abi::GBUFFER_BASE_TARGETS.to_vec();
        if self.dispatches() {
            layout.push(MODEL_ID_TARGET);
        }
        layout.extend(self.extras().iter().map(|extra| extra.target));
        layout
    }

    /// A stable identity for caches and labels.
    pub fn signature(&self) -> String {
        let models = self
            .models
            .iter()
            .map(LightingModel::signature)
            .collect::<Vec<_>>()
            .join(",");
        format!("models={models}")
    }

    /// A hash of [`LightingSet::signature`], for cache keys.
    pub fn stable_id(&self) -> u64 {
        stable_hash(self.signature().as_bytes())
    }
}

/// The shipped models, by id.
///
/// The paths are ABI — the same kind of core-to-library contract as
/// [`abi::SURFACE_MODULE`] — and `wxsl-stdlib` ships a module at each of
/// them, which its tests check by compiling. An application substitutes its
/// own set freely; these are what the default pipelines mean by "a
/// lighting model".
pub const DEFAULT_MODELS: &[LightingModel] = &[
    LightingModel {
        id: 0,
        name: "lambert",
        module: "package::lighting::models::lambert",
        function: "lighting_lambert",
        extra: None,
        doc: "Diffuse-only Lambertian: radiance * albedo * max(dot(n, l), 0).",
    },
    LightingModel {
        id: 1,
        name: "phong",
        module: "package::lighting::models::phong",
        function: "lighting_phong",
        extra: None,
        doc: "Lambertian diffuse plus a Phong specular lobe from the roughness.",
    },
    LightingModel {
        id: 2,
        name: "pbr",
        module: "package::lighting::models::pbr",
        function: "lighting_pbr",
        extra: None,
        doc: "The Cook-Torrance GGX model the library has always shaded with.",
    },
    LightingModel {
        id: 3,
        name: "clearcoat",
        module: "package::lighting::models::clearcoat",
        function: "lighting_clearcoat",
        extra: Some(ModelExtra {
            target: GBufferTarget {
                field: "clearcoat",
                doc: "x = coat strength, y = coat roughness",
                precision: GBufferPrecision::HighDynamicRangePair,
            },
            pack: "pack_clearcoat",
        }),
        doc: "PBR plus a second clear-coat lobe, asking for a G-buffer target.",
    },
];

/// The id of the model [`LightingSet::default_set`] shades with, and the
/// model a material that names none gets: the same Cook-Torrance model the
/// ABI shaded with before models existed.
pub const DEFAULT_MODEL_ID: u32 = 2;

/// A set of exactly [`DEFAULT_MODEL_ID`]: the default.
pub fn default_single_set() -> LightingSet {
    let model = DEFAULT_MODELS[DEFAULT_MODEL_ID as usize];
    debug_assert_eq!(model.id, DEFAULT_MODEL_ID);
    LightingSet::single(model)
}

/// Every shipped model, enabled: the set a demo or a "show me everything"
/// pipeline asks for.
pub fn default_set() -> Result<LightingSet, LightingError> {
    LightingSet::new(DEFAULT_MODELS.iter().copied())
}

/// What one material is shaded with: its model, and the set around it.
///
/// Both together, because the two answer different questions and both are
/// needed at generation time — the model picks the function the forward
/// stage calls, the set picks whether the G-buffer carries an id and which
/// targets its struct has. An *unresolved* choice (a model by name, against
/// a set) resolves through [`MaterialLighting::resolve`], which is where a
/// name the set does not enable is reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialLighting {
    /// The set the surrounding pipeline enables.
    pub set: LightingSet,
    /// The id of this material's model, which must be in `set`.
    pub model: u32,
}

impl MaterialLighting {
    /// Resolve an *authored* choice — a model name, optional — against a
    /// pipeline's set. `None` means "the default model", which is the
    /// library's default when the set enables it and otherwise the set's
    /// lowest id — so widening a set never quietly reshades a material
    /// that named nothing.
    pub fn resolve(set: &LightingSet, name: Option<&str>) -> Result<Self, LightingError> {
        let model = match name {
            Some(name) => {
                set.by_name(name)
                    .ok_or_else(|| LightingError::UnknownModel {
                        name: name.to_string(),
                    })?
                    .id
            }
            None => match set.get(DEFAULT_MODEL_ID) {
                Some(default) => default.id,
                None => set.default_model().id,
            },
        };
        Ok(MaterialLighting {
            set: set.clone(),
            model,
        })
    }

    /// The material's model entry.
    pub fn model(&self) -> &LightingModel {
        self.set
            .get(self.model)
            .expect("resolved against the set, so the id is in it")
    }

    /// The enabled set.
    pub fn set(&self) -> &LightingSet {
        &self.set
    }
}

impl Default for MaterialLighting {
    /// The library's default model in a set of one — the shape every
    /// pipeline had before models existed, which writes no id channel and
    /// adds no G-buffer target.
    fn default() -> Self {
        MaterialLighting {
            set: default_single_set(),
            model: DEFAULT_MODEL_ID,
        }
    }
}

// ---------------------------------------------------------------------------
// Generated shaders
// ---------------------------------------------------------------------------

/// One generated piece of WXSL: the source, and what it imports.
///
/// The imports are separate because a generated *material* module has one
/// import list shared with the graph's own, and merging text that carries
/// `import` lines into it would mean parsing them back out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedLighting {
    /// The WXSL source, without any `import` lines.
    pub source: String,
    /// `(module, item)` pairs the source names, for the host module to
    /// import.
    pub imports: Vec<(&'static str, &'static str)>,
}

/// How the generated shading function picks its model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// Call one model directly: the forward stage of a material, compiled
    /// per material, which knows its model at generation time. No id, no
    /// switch, and no other model in the module.
    Direct(LightingModel),
    /// Switch over an id the caller read from the G-buffer: the deferred
    /// lighting pass, which shades every enabled model's pixels in one
    /// draw.
    Switch(LightingSet),
}

/// Generate the per-light dispatch and the whole [`abi::SHADE_SURFACE_FN`].
///
/// The light loop is shared text — sample, shadow, accumulate — and is the
/// same for every set; the call inside it is the one thing that varies, so
/// it goes through [`abi::LIGHTING_DISPATCH_FN`], whose body is the only
/// part the dispatch shape changes.
///
/// The macro knobs the loop honours (`wxsl_tonemap`,
/// `wxsl_debug_normals`, `wxsl_receive_shadows`) are declared here with the
/// ABI's defaults, exactly as `shading.wxsl` used to declare them: a WXSL
/// module declares the knobs it uses, and the host binds values over the
/// top.
pub fn shade_surface(dispatch: &Dispatch) -> GeneratedLighting {
    shade_surface_with(dispatch, true)
}

/// The `${NAME}`-holed templates the generators fill, embedded at compile
/// time and kept under `templates/lighting/` for the ADR 0020 reason: the
/// light loop, the ambient and the pass's plumbing are shader text a
/// shader author may want to read, diff and edit as shader, and none of it
/// can be a shipped module because it names things no fixed module can.
/// The generator owns the logic — which models, which targets, which ids —
/// and one table of hole → text per template
/// ([`crate::template`], plan2 P1).
const MACROS_TEMPLATE: &str = include_str!("../templates/lighting/macros.wxsl");
const DISPATCH_DIRECT_TEMPLATE: &str = include_str!("../templates/lighting/dispatch_direct.wxsl");
const DISPATCH_SWITCH_TEMPLATE: &str = include_str!("../templates/lighting/dispatch_switch.wxsl");
const SHADE_SURFACE_TEMPLATE: &str = include_str!("../templates/lighting/shade_surface.wxsl");
const LIGHTING_PASS_TEMPLATE: &str = include_str!("../templates/lighting/lighting_pass.wxsl");

/// [`shade_surface`], with the macro declarations omitted.
///
/// A generated *material* module declares every macro in effect itself, so
/// shading text pasted into one must not declare them again — twice in the
/// same module is a redefinition. The lighting pass, compiled as a root of
/// its own, has nobody else to declare them and needs them.
pub fn shade_surface_with(dispatch: &Dispatch, declare_macros: bool) -> GeneratedLighting {
    let mut imports = vec![
        (abi::SURFACE_MODULE, abi::SURFACE_STRUCT),
        (abi::SURFACE_MODULE, abi::CONTEXT_STRUCT),
        ("package::wxsl::bindings", "sample_light"),
        ("package::wxsl::bindings", "scene"),
        ("package::wxsl::bindings", "WXSL_MAX_LIGHTS"),
        ("package::wxsl::bindings", "LightSample"),
        (abi::SHADOW_MODULE, abi::SHADOW_FACTOR_FN),
        (
            "package::lighting::ambient_environment",
            "ambient_environment",
        ),
        ("package::color::linear_to_srgb", "linear_to_srgb"),
        ("package::color::tonemap_filmic", "tonemap_filmic"),
    ];

    let mut out = String::with_capacity(3072);
    out.push_str(
        "// ---------------------------------------------------------------------------\n",
    );
    out.push_str("// Lighting: generated by `wxsl_core::lighting` from the enabled models.\n");
    out.push_str("// Do not edit: change the registry or the model modules instead.\n");
    out.push_str(
        "// ---------------------------------------------------------------------------\n\n",
    );
    if declare_macros {
        out.push_str(&crate::template::fill(MACROS_TEMPLATE, &[]));
    }

    let (shading_params, dispatch_args, extra_decl) = match dispatch {
        Dispatch::Direct(model) => {
            imports.push((model.module, model.function));
            let extra = match model.extra {
                // Computed once, before the loop: the pack reads the same
                // surface every light would hand it.
                Some(extra) => {
                    imports.push((model.module, extra.pack));
                    format!("{}(surface)", extra.pack)
                }
                None => "vec4f(0.0)".to_string(),
            };
            out.push_str(&crate::template::fill(
                DISPATCH_DIRECT_TEMPLATE,
                &[
                    ("DISPATCH_FN", abi::LIGHTING_DISPATCH_FN),
                    ("SURFACE", abi::SURFACE_STRUCT),
                    ("CONTEXT", abi::CONTEXT_STRUCT),
                    ("FUNCTION", model.function),
                ],
            ));
            (
                String::new(),
                ", model_extra".to_string(),
                format!("    let model_extra = {extra};\n"),
            )
        }
        Dispatch::Switch(set) => {
            for model in set.models() {
                imports.push((model.module, model.function));
            }
            // The models' view of the G-buffer: one field per requested
            // target, and nothing else — no id, no base targets. Empty
            // when no model asked for one, which WGSL permits.
            let mut fields = String::new();
            for extra in set.extras() {
                let _ = writeln!(
                    fields,
                    "    {}: {},",
                    extra.target.field,
                    extra.target.precision.field_type(),
                );
            }
            // A narrower target is widened back to the vec4f the model
            // contract hands over.
            let mut arms = String::new();
            for model in set.models() {
                let extra = match model.extra {
                    Some(extra) => match extra.target.precision.channels() {
                        1 => format!("vec4f(extras.{}, 0.0, 0.0, 0.0)", extra.target.field),
                        2 => format!("vec4f(extras.{}, 0.0, 0.0)", extra.target.field),
                        _ => format!("extras.{}", extra.target.field),
                    },
                    None => "vec4f(0.0)".to_string(),
                };
                let _ = writeln!(
                    arms,
                    "        case {id}u: {{ return {function}(surface, ctx, light, {extra}); }}",
                    id = model.id,
                    function = model.function,
                );
            }
            out.push_str(&crate::template::fill(
                DISPATCH_SWITCH_TEMPLATE,
                &[
                    ("MODEL_EXTRAS_FIELDS", &fields),
                    ("DISPATCH_FN", abi::LIGHTING_DISPATCH_FN),
                    ("SURFACE", abi::SURFACE_STRUCT),
                    ("CONTEXT", abi::CONTEXT_STRUCT),
                    ("SWITCH_ARMS", &arms),
                ],
            ));
            (
                // The shading function receives the extras as they came
                // off the G-buffer; there is nothing to compute.
                ", extras: ModelExtras, model_id: u32".to_string(),
                ", extras, model_id".to_string(),
                String::new(),
            )
        }
    };

    out.push_str(&crate::template::fill(
        SHADE_SURFACE_TEMPLATE,
        &[
            ("SHADE_FN", abi::SHADE_SURFACE_FN),
            ("SURFACE", abi::SURFACE_STRUCT),
            ("CONTEXT", abi::CONTEXT_STRUCT),
            ("SHADING_PARAMS", &shading_params),
            ("EXTRA_DECL", &extra_decl),
            ("SHADOW_FN", abi::SHADOW_FACTOR_FN),
            ("DISPATCH_FN", abi::LIGHTING_DISPATCH_FN),
            ("DISPATCH_ARGS", &dispatch_args),
        ],
    ));

    GeneratedLighting {
        source: out,
        imports: dedup(imports),
    }
}

/// Generate the [`abi::GBUFFER_STRUCT`] struct for a layout.
///
/// One field per target, at its `@location`, which is the index in the
/// layout — the same list `wxsl-render` declares its resources from.
pub fn gbuffer_struct(layout: &[GBufferTarget]) -> String {
    let mut out = String::with_capacity(256);
    let _ = writeln!(
        out,
        "struct {struct_name} {{",
        struct_name = abi::GBUFFER_STRUCT
    );
    for (location, target) in layout.iter().enumerate() {
        let _ = writeln!(
            out,
            "    // {doc}\n    @location({location}) {field}: {ty},",
            doc = target.doc,
            field = target.field,
            ty = target.precision.field_type(),
        );
    }
    out.push_str("}\n");
    out
}

/// Generate [`abi::PACK_GBUFFER_FN`] for a material shading with `model`
/// under `set`.
///
/// The base targets are packed exactly as they always were; the id channel
/// is written only when the set dispatches; and the material fills its own
/// model's target through the model's pack function, leaving every *other*
/// model's request zeroed. Zeroed rather than skipped: the field exists,
/// and an undefined channel reads back whatever the clear left.
pub fn pack_gbuffer(model: &LightingModel, set: &LightingSet) -> GeneratedLighting {
    let layout = set.gbuffer_layout();
    let mut imports = Vec::new();
    let mut source = gbuffer_struct(&layout);
    let _ = write!(
        source,
        "\nfn {pack}(surface: {surface}{id_param}) -> {struct_name} {{\n    \
             var out: {struct_name};\n",
        pack = abi::PACK_GBUFFER_FN,
        surface = abi::SURFACE_STRUCT,
        id_param = if set.dispatches() {
            ", model_id: u32"
        } else {
            ""
        },
        struct_name = abi::GBUFFER_STRUCT,
    );
    for target in &layout {
        let expr = if abi::GBUFFER_BASE_TARGETS
            .iter()
            .any(|base| base.field == target.field)
        {
            // The base packing, exactly as `deferred.wxsl` wrote it.
            match target.field {
                "base_color" => "vec4f(surface.base_color, saturate(surface.metallic))",
                "normal" => "vec4f(normalize(surface.normal), saturate(surface.roughness))",
                "emissive" => "vec4f(surface.emissive, saturate(surface.occlusion))",
                other => unreachable!("base target `{other}` has no pack expression"),
            }
            .to_string()
        } else if target.field == MODEL_ID_FIELD {
            format!("f32(model_id) / {MODEL_ID_SCALE}")
        } else if model.extra.map(|extra| extra.target.field) == Some(target.field) {
            // The pack contract hands a vec4f over; a narrower target
            // simply takes the leading components of it.
            let extra = model.extra.expect("matched above");
            imports.push((model.module, extra.pack));
            match target.precision.channels() {
                1 => format!("{}(surface).x", extra.pack),
                2 => format!("{}(surface).xy", extra.pack),
                _ => format!("{}(surface)", extra.pack),
            }
        } else {
            match target.precision.channels() {
                1 => "0.0".to_string(),
                2 => "vec2f(0.0)".to_string(),
                _ => "vec4f(0.0)".to_string(),
            }
        };
        let _ = writeln!(source, "    out.{field} = {expr};", field = target.field);
    }
    source.push_str("    return out;\n}\n");
    GeneratedLighting {
        source,
        imports: dedup(imports),
    }
}

/// Generate the whole deferred lighting pass for `set`, as one root module.
///
/// The fullscreen triangle and the depth-based background test are the
/// plumbing they always were; the unpack, the struct it returns and the
/// shading function are the set's, because they name its targets and
/// models. Mounted under [`abi::LIGHTING_PASS_MODULE`], which is why that
/// path still exists for labels and diagnostics.
pub fn lighting_pass_source(set: &LightingSet) -> String {
    let layout = set.gbuffer_layout();
    // One model needs no dispatch at all: the pass shades with it
    // directly, and the module contains only that model — which is the
    // bar a single-model pipeline is held to. Only a real set switches.
    let dispatch = match set.len() {
        1 => Dispatch::Direct(*set.models().first().expect("non-empty")),
        _ => Dispatch::Switch(set.clone()),
    };
    let switch_shape = matches!(dispatch, Dispatch::Switch(_));
    let shading = shade_surface(&dispatch);

    let mut out = String::with_capacity(4096);
    out.push_str(
        "// The deferred lighting pass, generated by `wxsl_core::lighting`
",
    );
    out.push_str("// from the enabled models (");
    let names = set
        .models()
        .iter()
        .map(|model| model.name)
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "{names}).");
    out.push_str(
        "// Do not edit: change the set or a model module instead.

",
    );

    out.push_str(
        "import package::wxsl::bindings::{camera, scene};
",
    );
    out.push_str(
        "import package::space::tangent_basis::tangent_basis;
",
    );
    for (module, item) in &shading.imports {
        let _ = writeln!(out, "import {module}::{item};");
    }

    // One binding per target, in layout order, then depth — the same order
    // `wxsl-render` declared the pass's reads in.
    let mut bindings = String::new();
    for (binding, target) in layout.iter().enumerate() {
        let _ = writeln!(
            bindings,
            "@group(3) @binding({binding}) var gbuffer_{field}: texture_2d<f32>;",
            binding = binding,
            field = target.field,
        );
    }

    // What the texels come back as: the surface, plus whatever the models
    // asked for. One struct rather than out-params, because the dispatch's
    // arms read different subsets of it.
    let mut unpacked = String::from(
        "struct UnpackedGBuffer {
    surface: Surface,
",
    );
    if set.dispatches() {
        unpacked.push_str(
            "    model_id: u32,
",
        );
    }
    for extra in set.extras() {
        let _ = writeln!(
            unpacked,
            "    {}: {},",
            extra.target.field,
            extra.target.precision.field_type(),
        );
    }
    unpacked.push_str(
        "}

",
    );

    let params = layout
        .iter()
        .map(|target| format!("{}: {}", target.field, target.precision.field_type()))
        .collect::<Vec<_>>()
        .join(", ");

    // The id line exists only when the set dispatches. Half-float
    // integers are exact well past 255, so the round trip through a
    // normalized target costs nothing (see `MODEL_ID_TARGET`).
    let model_id_line = if set.dispatches() {
        let mut line = String::new();
        let _ = writeln!(
            line,
            "    out.model_id = u32(round(lighting * {}));",
            MODEL_ID_SCALE
        );
        line
    } else {
        String::new()
    };

    // One passthrough per requested target.
    let mut extras_unpack = String::new();
    for extra in set.extras() {
        let _ = writeln!(
            extras_unpack,
            "    out.{field} = {field};",
            field = extra.target.field,
        );
    }

    // `textureLoad` always answers with a vec4; the unpack's parameter
    // takes the channels the target actually has.
    let loads = layout
        .iter()
        .map(|target| {
            let load = format!("textureLoad(gbuffer_{}, coord, 0)", target.field);
            match target.precision.channels() {
                1 => format!("{load}.x"),
                2 => format!("{load}.xy"),
                _ => load,
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let shade_args = if switch_shape {
        format!(
            "unpacked.surface, ctx, ModelExtras({}), unpacked.model_id",
            set.extras()
                .iter()
                .map(|extra| format!("unpacked.{}", extra.target.field))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        "unpacked.surface, ctx".to_string()
    };

    let depth_binding = layout.len().to_string();
    out.push_str(&crate::template::fill(
        LIGHTING_PASS_TEMPLATE,
        &[
            ("GBUFFER_BINDINGS", bindings.as_str()),
            ("DEPTH_BINDING", depth_binding.as_str()),
            ("VERTEX_ENTRY", abi::LIGHTING_PASS_VERTEX_ENTRY),
            ("UNPACKED_STRUCT", unpacked.as_str()),
            ("UNPACK_FN", abi::UNPACK_GBUFFER_FN),
            ("UNPACK_PARAMS", params.as_str()),
            ("MODEL_ID_LINE", model_id_line.as_str()),
            ("EXTRAS_UNPACK", extras_unpack.as_str()),
            ("SHADING", shading.source.as_str()),
            ("FRAGMENT_ENTRY", abi::LIGHTING_PASS_FRAGMENT_ENTRY),
            ("FIRST_TARGET", layout[0].field),
            ("UNPACK_CALL", loads.as_str()),
            ("SHADE_FN", abi::SHADE_SURFACE_FN),
            ("SHADE_ARGS", shade_args.as_str()),
        ],
    ));
    out
}

/// Drop repeated `(module, item)` pairs, keeping first order.
fn dedup(imports: Vec<(&'static str, &'static str)>) -> Vec<(&'static str, &'static str)> {
    let mut seen = Vec::new();
    for pair in imports {
        if !seen.contains(&pair) {
            seen.push(pair);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: u32, name: &str) -> LightingModel {
        LightingModel {
            id,
            name: Box::leak(name.to_string().into_boxed_str()),
            module: "package::test",
            function: "test_model",
            extra: None,
            doc: "",
        }
    }

    #[test]
    fn a_set_is_sorted_by_id_and_rejects_collisions() {
        let set = LightingSet::new([model(2, "b"), model(0, "a")]).unwrap();
        assert_eq!(
            set.models().iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(set.default_model().name, "a", "lowest id is the default");

        assert_eq!(
            LightingSet::new([model(1, "a"), model(1, "b")]).unwrap_err(),
            LightingError::DuplicateId { id: 1 }
        );
        assert_eq!(
            LightingSet::new([model(0, "same"), model(1, "same")]).unwrap_err(),
            LightingError::DuplicateName {
                name: "same".to_string()
            }
        );
        assert_eq!(LightingSet::new([]).unwrap_err(), LightingError::Empty);
    }

    #[test]
    fn a_single_model_needs_no_id_channel() {
        let set = LightingSet::single(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize]);
        assert!(!set.dispatches());
        assert_eq!(set.gbuffer_layout(), abi::GBUFFER_BASE_TARGETS.to_vec());
    }

    #[test]
    fn the_layout_is_base_plus_id_plus_extras_in_id_order() {
        let set = default_set().unwrap();
        assert!(set.dispatches());
        let layout = set.gbuffer_layout();
        let fields: Vec<&str> = layout.iter().map(|t| t.field).collect();
        assert_eq!(
            fields,
            vec!["base_color", "normal", "emissive", "lighting", "clearcoat"],
            "base targets, then the id channel, then the requests"
        );
        assert_eq!(layout[3], MODEL_ID_TARGET);
    }

    #[test]
    fn a_direct_dispatch_names_one_model_and_no_other() {
        let pbr = DEFAULT_MODELS[DEFAULT_MODEL_ID as usize];
        let generated = shade_surface(&Dispatch::Direct(pbr));
        assert!(generated.source.contains("fn lighting_dispatch"));
        assert!(generated
            .source
            .contains("return lighting_pbr(surface, ctx, light, extra);"));
        assert!(!generated.source.contains("lighting_lambert"));
        assert!(!generated.source.contains("switch"));
        assert!(generated
            .imports
            .iter()
            .any(|(module, item)| *module == pbr.module && *item == pbr.function));
        // One import per pair, even when two pieces asked for the same.
        let mut unique = generated.imports.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), generated.imports.len());
    }

    #[test]
    fn a_switch_dispatch_has_one_arm_per_enabled_model() {
        let set = default_set().unwrap();
        let generated = shade_surface(&Dispatch::Switch(set.clone()));
        for model in set.models() {
            assert!(
                generated.source.contains(&format!(
                    "case {}u: {{ return {}(",
                    model.id, model.function
                )),
                "no arm for {}",
                model.name
            );
        }
        // Every enabled model is imported: a model nobody enabled costs
        // nothing, which is the whole point of generating the dispatch.
        assert!(generated.source.contains("lighting_lambert"));
        assert!(generated.source.contains("default: { return vec3f(0.0); }"));
        // The arm for the model with an extra reads it, widened back to
        // the vec4f the contract hands over; the others are handed zero.
        assert!(generated.source.contains(
            "case 3u: { return lighting_clearcoat(surface, ctx, light, vec4f(extras.clearcoat, 0.0, 0.0)); }"
        ));
        assert!(generated
            .source
            .contains("case 2u: { return lighting_pbr(surface, ctx, light, vec4f(0.0)); }"));
        // The switch flavour's shading function takes the extras and the id.
        assert!(generated.source.contains(
            "fn shade_surface(surface: Surface, ctx: SurfaceContext, extras: ModelExtras, model_id: u32)"
        ));
    }

    #[test]
    fn the_pack_fills_the_id_channel_only_when_dispatching() {
        let single = LightingSet::single(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize]);
        let plain = pack_gbuffer(&DEFAULT_MODELS[DEFAULT_MODEL_ID as usize], &single);
        assert!(plain
            .source
            .contains("fn pack_gbuffer(surface: Surface) -> GBuffer"));
        assert!(!plain.source.contains("model_id"));

        let set = default_set().unwrap();
        let dispatched = pack_gbuffer(&DEFAULT_MODELS[DEFAULT_MODEL_ID as usize], &set);
        assert!(dispatched
            .source
            .contains("fn pack_gbuffer(surface: Surface, model_id: u32) -> GBuffer"));
        assert!(dispatched
            .source
            .contains("out.lighting = f32(model_id) / 255"));

        // The clearcoat model fills its own target through its pack, and a
        // different model's target stays zeroed.
        let coat = DEFAULT_MODELS
            .iter()
            .find(|m| m.name == "clearcoat")
            .unwrap();
        let coated = pack_gbuffer(coat, &set);
        // The pair-precision target takes the leading components of the
        // pack contract's vec4f.
        assert!(coated
            .source
            .contains("out.clearcoat = pack_clearcoat(surface).xy;"));
        assert!(coated.source.contains("out.lighting = f32(model_id)"));
        let plain_under_set = pack_gbuffer(&DEFAULT_MODELS[DEFAULT_MODEL_ID as usize], &set);
        assert!(plain_under_set
            .source
            .contains("out.clearcoat = vec2f(0.0);"));
    }

    #[test]
    fn the_lighting_pass_binds_one_target_per_layout_entry_plus_depth() {
        let set = default_set().unwrap();
        let source = lighting_pass_source(&set);
        let layout = set.gbuffer_layout();
        for (binding, target) in layout.iter().enumerate() {
            assert!(
                source.contains(&format!(
                    "@group(3) @binding({binding}) var gbuffer_{}:",
                    target.field
                )),
                "no binding for {}",
                target.field
            );
        }
        assert!(source.contains(&format!(
            "@group(3) @binding({}) var gbuffer_depth:",
            layout.len()
        )));
        // The dispatch and the unpack are both present, and the fragment
        // entry hands the extras and the id to the shading function.
        assert!(source.contains("switch model_id"));
        assert!(source.contains("out.model_id = u32(round(lighting * 255))"));
        assert!(source.contains(
            "return shade_surface(unpacked.surface, ctx, ModelExtras(unpacked.clearcoat), unpacked.model_id)"
        ));
    }

    #[test]
    fn a_single_model_pass_has_no_dispatch_at_all() {
        let set = LightingSet::single(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize]);
        let source = lighting_pass_source(&set);
        assert!(!source.contains("switch"));
        assert!(!source.contains("model_id"));
        // One model shades directly: no dispatch, no extras argument, and
        // nothing in the module but that model.
        assert!(source.contains("return shade_surface(unpacked.surface, ctx)"));
        assert!(source.contains("return lighting_pbr(surface, ctx, light, extra);"));
    }

    #[test]
    fn a_set_signature_distinguishes_sets() {
        let single = LightingSet::single(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize]);
        let all = default_set().unwrap();
        assert_ne!(single.signature(), all.signature());
        assert_ne!(single.stable_id(), all.stable_id());
        // Order of construction does not change the identity.
        let reordered = LightingSet::new(DEFAULT_MODELS.iter().rev().copied()).unwrap();
        assert_eq!(all.signature(), reordered.signature());
    }
}
