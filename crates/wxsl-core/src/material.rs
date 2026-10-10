//! [`MaterialConfig`]: everything an author sets on a material that is not
//! a node, as one value.
//!
//! A material graph says what a surface *is*. Everything else an author
//! decides about it — the macro values pinned on top of the graph's own,
//! which lighting model shades it, whether it casts and receives shadows,
//! what it is tagged as — used to be several spellings of one fact: fields
//! on the scene document's `MaterialEntry`, fields on `wxsl-render`'s
//! `MaterialOptions`, and two more on [`crate::codegen::CodegenOptions`],
//! with the resolution steps that turn a name into a model id and a flag
//! into a macro happening in two places
//! ([ADR 0038](../../docs/adr/0038-a-materials-configuration-is-one-value.md)).
//!
//! One value now travels the whole way:
//!
//! ```text
//!   MaterialEntry.config ──resolve(set)──> ResolvedMaterialConfig
//!   (the document's        (the one            │
//!    authored knobs)        resolution         ├─> CodegenOptions.material
//!                           point)             └─> Material's own accessors
//! ```
//!
//! The split between the two types is the split between *authored* and
//! *resolved*: a config names a lighting model, a resolved config holds the
//! model's id in the set that enables it; a config carries a
//! `receive_shadow` flag, a resolved config has already pinned
//! [`abi::FEATURE_RECEIVE_SHADOWS`] because that flag *is* the macro. The
//! next per-material knob is a field on [`MaterialConfig`] and whatever
//! [`MaterialConfig::resolve`] has to do with it — not another options
//! struct.

use crate::abi;
use crate::lighting::{ChannelRequest, LightingError, LightingSet, MaterialLighting, FEATURES};
use crate::macros::{MacroSet, MacroValue};
use crate::scene::Tags;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// How a material is compiled, beyond the graph itself.
///
/// The authored half: what a document carries and what an application
/// setting one up by hand fills in. [`MaterialConfig::resolve`] turns it
/// into a [`ResolvedMaterialConfig`], which is what codegen and the
/// renderer read.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MaterialConfig {
    /// Macro values taking precedence over the ones the graph pins.
    ///
    /// The precedence order, weakest first, is: the ABI's defaults, each
    /// node's declared default, what the graph pins, then these.
    #[cfg_attr(feature = "serde", serde(default))]
    pub macros: MacroSet,
    /// Which lighting model shades this material, by the name its registry
    /// entry carries, or `None` for the enabled set's default
    /// ([ADR 0028](../../docs/adr/0028-lighting-models-dispatched-by-a-g-buffer-id.md)).
    ///
    /// A name, never an id: ids belong to the registry, and a document that
    /// stored them would reshade silently when one was renumbered. Resolved
    /// against the set in [`MaterialConfig::resolve`], where a name the set
    /// does not enable is an error rather than a silently different shade.
    #[cfg_attr(
        feature = "serde",
        serde(default, alias = "lighting", skip_serializing_if = "Option::is_none")
    )]
    pub model: Option<String>,
    /// Whether the shadow passes draw this material.
    ///
    /// Off for anything a shadow would only get in the way of: a ground
    /// plane, a skybox, a glowing decal. A *selection*, not code — the
    /// generated modules of a material that casts and one that does not are
    /// identical
    /// ([ADR 0026](../../docs/adr/0026-shadows-a-view-per-light-and-two-flags-on-the-material.md)).
    #[cfg_attr(feature = "serde", serde(default = "yes"))]
    pub cast_shadow: bool,
    /// Whether this material's shading is attenuated by the shadow maps.
    ///
    /// Code, not a selection: it *is* [`abi::FEATURE_RECEIVE_SHADOWS`], so a
    /// material that does not receive shadows does not compile the lookup
    /// and the variant cache keeps the two apart on their macro sets.
    #[cfg_attr(feature = "serde", serde(default = "yes"))]
    pub receive_shadow: bool,
    /// What this material *is*, for a pass's tag expression to select on.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tags: Tags,
    /// Which group of a pass draws this material: draws sort by
    /// (`render_order`, camera depth), ascending, inside each pass that
    /// sorts at all (plan5 D3/D4). Zero — every scene's behaviour before
    /// this field existed. It never reorders *passes*: that stays the
    /// document's job.
    #[cfg_attr(feature = "serde", serde(default))]
    pub render_order: i32,
    /// Whether this material shades in the geometry pass, whichever stage
    /// that pass names (plan5 D2). Under a forward pipeline nothing
    /// changes. Under a deferred one the G-buffer pass runs the material's
    /// own lighting model, the radiance lands where the G-buffer keeps
    /// emissive, and the lighting pass returns it — no lights, no shadows,
    /// no re-shading. This is the escape hatch for shading that needs
    /// screen-space context (derivatives, plan5 D1).
    ///
    /// An explicit, checked decision, never a fallback: resolution
    /// requires `wxsl.preshaded` in the pipeline's set, refuses
    /// `receive_shadow` (a preshaded surface is shaded before any shadow
    /// map exists), and refuses this model named as the material's own.
    #[cfg_attr(feature = "serde", serde(default))]
    pub forward_shaded: bool,
    /// How many peel layers this transparent may consume (plan5 D5). Zero
    /// — every material's behaviour before this field existed — is the
    /// sorted tier: plain alpha blending under a back-to-front sort, and
    /// no peel pass reads the draw. One or more asks for
    /// [ADR 0047](../../docs/adr/0047-dual-depth-peeling-and-the-baseline-native-split-for-blendable-float-targets.md)'s
    /// peeling, one layer per iteration while the budget lasts; a value
    /// beyond the pipeline's `wxsl_peel_layers` cap is clamped by
    /// construction, never an error.
    #[cfg_attr(feature = "serde", serde(default))]
    pub max_layers: i32,
    /// Whether the graph's bake declarations are *consumed* as bakes — the
    /// sampled table — or evaluated inline, the subgraph in the material
    /// module
    /// ([ADR 0045](../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
    ///
    /// This is the toggle the bake's done-when speaks of: flipping it
    /// changes what the material costs (the baked arm's module is smaller
    /// by the whole subgraph) and not what it looks like, because the bake
    /// is the same value at every texel centre. It needs no edit: the
    /// declaration stays in the graph, and both arms compile from it. On
    /// by default — a declaration expresses the intent to bake — and it
    /// rides [`MaterialConfig`] because a material's configuration is one
    /// value ([ADR 0038]).
    #[cfg_attr(feature = "serde", serde(default = "yes"))]
    pub bakes: bool,
    /// The feature channels the surrounding *pipeline* carries, which this
    /// material's G-buffer struct is generated for (plan2 P12, ADR 0037).
    ///
    /// Not authored — handed down from the pipeline's plan, which is why it
    /// is skipped on the wire: a document that stored a pipeline's channels
    /// would be a document that only loads under one pipeline. The loader
    /// fills it from the plan the material is about to be drawn under.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub features: Vec<ChannelRequest>,
}

/// The default of both shadow flags. A function because that is the shape
/// `serde(default = ...)` takes.
fn yes() -> bool {
    true
}

impl Default for MaterialConfig {
    /// No macro overrides, the set's default model, casting and receiving
    /// shadows, untagged, under a pipeline carrying no feature channels.
    fn default() -> Self {
        MaterialConfig {
            macros: MacroSet::new(),
            model: None,
            cast_shadow: true,
            receive_shadow: true,
            tags: Tags::new(),
            render_order: 0,
            forward_shaded: false,
            max_layers: 0,
            bakes: true,
            features: Vec::new(),
        }
    }
}

impl MaterialConfig {
    /// The defaults, with `macros` on top.
    pub fn with_macros(macros: MacroSet) -> Self {
        MaterialConfig {
            macros,
            ..MaterialConfig::default()
        }
    }

    /// Shade this material with the named lighting model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set both shadow flags.
    pub fn with_shadows(mut self, cast: bool, receive: bool) -> Self {
        self.cast_shadow = cast;
        self.receive_shadow = receive;
        self
    }

    /// Replace the tags.
    pub fn with_tags(mut self, tags: Tags) -> Self {
        self.tags = tags;
        self
    }

    /// Mark whether this material shades forward under either pipeline
    /// (plan5 D2).
    pub fn forward_shaded(mut self, forward: bool) -> Self {
        self.forward_shaded = forward;
        self
    }

    /// Set how many peel layers this transparent may consume (plan5 D5).
    pub fn with_max_layers(mut self, layers: i32) -> Self {
        self.max_layers = layers;
        self
    }

    /// Pin this draw's group inside every pass that sorts (plan5 D4):
    /// draws sort by (`render_order`, camera depth), ascending.
    pub fn with_render_order(mut self, order: i32) -> Self {
        self.render_order = order;
        self
    }

    /// Generate against a pipeline carrying `features`' channels.
    pub fn with_features(mut self, features: Vec<ChannelRequest>) -> Self {
        self.features = features;
        self
    }

    /// Resolve this configuration against the lighting set the surrounding
    /// pipeline enables — the one place a name becomes an id and a flag
    /// becomes a macro.
    ///
    /// A model name the set does not enable is an error here rather than a
    /// silently different shade. The feature *demands* a material makes
    /// cannot be checked yet: a graph may pin a feature's macro itself, and
    /// the graph's own pins are only known once codegen has overlaid them —
    /// [`ResolvedMaterialConfig::check_feature_demands`] is that half, run
    /// by the caller that has the generated macro set.
    pub fn resolve(&self, set: &LightingSet) -> Result<ResolvedMaterialConfig, LightingError> {
        let lighting = MaterialLighting::resolve(set, self.model.as_deref())?
            .with_features(self.features.clone());
        // The forward-shading demands (plan5 D2) are checked here, at the
        // one resolution point, so a scene document that cannot be
        // honoured is refused before anything is built.
        if self.forward_shaded {
            if lighting.model().name == crate::lighting::PRESHADED_MODEL {
                return Err(LightingError::ForwardShadedModel);
            }
            if set.preshaded().is_none() {
                return Err(LightingError::PreshadedNotEnabled);
            }
            if self.receive_shadow {
                return Err(LightingError::ForwardShadedWithShadows);
            }
        }
        let mut macros = self.macros.clone();
        // The flag wins over anything the graph or the caller put under the
        // same name, because it is the field's whole meaning.
        macros.set(
            abi::FEATURE_RECEIVE_SHADOWS,
            MacroValue::Flag(self.receive_shadow),
        );
        Ok(ResolvedMaterialConfig {
            macros,
            lighting,
            cast_shadow: self.cast_shadow,
            tags: self.tags.clone(),
            render_order: self.render_order,
            forward_shaded: self.forward_shaded,
            max_layers: self.max_layers,
            bakes: self.bakes,
        })
    }
}

/// A [`MaterialConfig`] resolved against a pipeline's lighting set: what
/// codegen compiles under and what the renderer checks against.
///
/// Everything here is a *fact* rather than a request — an id rather than a
/// name, a macro set rather than a flag — which is what lets the renderer
/// compare two of them for a mismatch by value.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMaterialConfig {
    /// The macro values to sit above the graph's own: the config's
    /// overrides, plus the receive-shadows flag it pins.
    pub macros: MacroSet,
    /// Which lighting model shades this material, which set it was resolved
    /// against, and which feature channels its G-buffer struct carries.
    pub lighting: MaterialLighting,
    /// Whether the shadow passes draw this material.
    pub cast_shadow: bool,
    /// Which group of a pass draws this material, ascending (plan5 D4).
    pub render_order: i32,
    /// How many peel layers this transparent may consume (plan5 D5): zero
    /// is the sorted tier no peel pass reads, one or more is the draw's
    /// request under the pipeline's `wxsl_peel_layers` cap.
    pub max_layers: i32,
    /// Whether this material shades in the geometry pass under either
    /// pipeline (plan5 D2): its own model ran in the G-buffer pass, whose
    /// radiance the lighting pass returns untouched.
    pub forward_shaded: bool,
    /// What this material *is*, for a pass's tag expression to select on.
    pub tags: Tags,
    /// Whether the graph's bake declarations are consumed as bakes (the
    /// sampled table) or evaluated inline (the subgraph in the module).
    pub bakes: bool,
}

impl Default for ResolvedMaterialConfig {
    /// [`MaterialConfig::default`] resolved against the library's default
    /// model in a set of one — the shape every material had before models
    /// existed.
    fn default() -> Self {
        MaterialConfig::default()
            .resolve(&LightingSet::default())
            .expect("the default config names no model, so nothing can fail to resolve")
    }
}

impl ResolvedMaterialConfig {
    /// The second half of the feature handshake: a material whose
    /// *effective* macros turn a feature on while the plan it was resolved
    /// against carries no channel for it.
    ///
    /// Takes the macro set codegen produced rather than
    /// [`ResolvedMaterialConfig::macros`], because a graph can pin a
    /// feature's macro itself and that pin is only visible after the
    /// graph's own values have been overlaid. The demand going unanswered
    /// would otherwise be a material silently shaded without the feature.
    pub fn check_feature_demands(&self, effective: &MacroSet) -> Result<(), LightingError> {
        for feature in FEATURES {
            let demands = matches!(
                effective.get(feature.macro_name),
                Some(MacroValue::Flag(true))
            );
            if demands
                && !self
                    .lighting
                    .features()
                    .iter()
                    .any(|request| request.source.name() == feature.name)
            {
                return Err(LightingError::FeatureNotCarried {
                    macro_name: feature.macro_name,
                    feature: feature.name,
                });
            }
        }
        Ok(())
    }

    /// Whether this material's shading is attenuated by the shadow maps,
    /// read back from the macro it *is*.
    pub fn receive_shadow(&self) -> bool {
        matches!(
            self.macros.get(abi::FEATURE_RECEIVE_SHADOWS),
            Some(MacroValue::Flag(true))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::feature_requests;

    #[test]
    fn a_flag_becomes_a_macro_exactly_once() {
        let mut macros = MacroSet::new();
        // Even a caller pinning the macro by hand loses to the field: the
        // field is what the flag means.
        macros.set(abi::FEATURE_RECEIVE_SHADOWS, MacroValue::Flag(true));
        let resolved = MaterialConfig {
            macros,
            receive_shadow: false,
            ..MaterialConfig::default()
        }
        .resolve(&LightingSet::default())
        .expect("the default set resolves");
        assert_eq!(
            resolved.macros.get(abi::FEATURE_RECEIVE_SHADOWS),
            Some(MacroValue::Flag(false))
        );
        assert!(!resolved.receive_shadow());
    }

    #[test]
    fn a_model_the_set_does_not_enable_is_named() {
        let error = MaterialConfig::default()
            .with_model("no-such-model")
            .resolve(&LightingSet::default())
            .expect_err("the default set enables one model");
        assert!(matches!(error, LightingError::UnknownModel { .. }));
    }

    #[test]
    fn a_feature_demand_the_plan_does_not_carry_is_named() {
        let feature = FEATURES.first().expect("at least one feature ships");
        let mut effective = MacroSet::new();
        effective.set(feature.macro_name, MacroValue::Flag(true));

        // Resolved against a plan with no channels: the demand goes
        // unanswered, and is named rather than silently dropped.
        let bare = MaterialConfig::default()
            .resolve(&LightingSet::default())
            .expect("resolves");
        let error = bare
            .check_feature_demands(&effective)
            .expect_err("the plan carries no channel");
        let message = error.to_string();
        assert!(
            message.contains(feature.macro_name) && message.contains(feature.name),
            "the error names the macro and the feature: {message}"
        );

        // Resolved against the plan that carries it: the same demand is
        // answered.
        let carried = MaterialConfig::default()
            .with_features(feature_requests(&[feature.name]).expect("the feature ships"))
            .resolve(&LightingSet::default())
            .expect("resolves");
        assert!(carried.check_feature_demands(&effective).is_ok());
    }

    /// The forward-shading demands (plan5 D2), each refused by name at the
    /// one resolution point: the preshaded route must be in the set, the
    /// shadows must be given up, and the route itself is not a shading
    /// function.
    #[test]
    fn the_forward_shading_demands_are_refused_by_name() {
        let set = crate::lighting::default_set().expect("the shipped models");
        assert!(
            set.preshaded().is_some(),
            "the shipped set carries the route"
        );

        // Happy path: the flag travels, and the model is the material's own.
        let resolved = MaterialConfig::default()
            .with_model("pbr")
            .forward_shaded(true)
            .with_shadows(true, false)
            .resolve(&set)
            .expect("every demand is met");
        assert!(resolved.forward_shaded);
        assert_eq!(resolved.lighting.model().name, "wxsl.pbr");

        // The default set enables the route, so the set-less refusals need
        // a set without it.
        let without = crate::lighting::LightingSet::single(
            crate::lighting::DEFAULT_MODELS
                .iter()
                .find(|m| m.name == "wxsl.pbr")
                .copied()
                .expect("pbr ships"),
        );
        let error = MaterialConfig::default()
            .forward_shaded(true)
            .with_shadows(true, false)
            .resolve(&without)
            .expect_err("the route is not in the set");
        assert_eq!(error, crate::lighting::LightingError::PreshadedNotEnabled);

        // Shadows: the flag on its own resolves; with the flag it cannot.
        let error = MaterialConfig::default()
            .forward_shaded(true)
            .resolve(&set)
            .expect_err("receive_shadow defaults to true");
        assert_eq!(
            error,
            crate::lighting::LightingError::ForwardShadedWithShadows
        );

        // The route named as the model: nothing left to shade with.
        let error = MaterialConfig::default()
            .with_model(crate::lighting::PRESHADED_MODEL)
            .forward_shaded(true)
            .with_shadows(true, false)
            .resolve(&set)
            .expect_err("the route is not a shading function");
        assert_eq!(error, crate::lighting::LightingError::ForwardShadedModel);

        // Naming the route *without* the flag stays legal — it shades
        // inconsistently (the ADR says so) but nothing breaks.
        assert!(MaterialConfig::default()
            .with_model(crate::lighting::PRESHADED_MODEL)
            .resolve(&set)
            .is_ok());
    }
}
