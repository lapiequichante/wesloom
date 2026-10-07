//! The capability contract: what a render setup provides, what a scene
//! needs, and the device-free check between them
//! ([ADR 0044](../../docs/adr/0044-identity-versions-and-the-capability-check.md)).
//!
//! The facts of a setup were always computed — the channel plan, the
//! lighting set, the effects the pipeline names, the handshake a material
//! is resolved under — but scattered across the renderer, the compiler and
//! the first frame. This module publishes them as one value:
//! [`RenderSetup::capabilities`] is what a pipeline provides, and
//! [`RenderSetup::check`] returns every way a scene does not fit it, by
//! name, before anything is built. The same [`check_scene`] runs inside
//! `wxsl::scene::SceneResources::load`, which is the load-time half of the
//! same contract.
//!
//! Pure data and pure functions: no device, no shader compiler. It is the
//! seamless twin of `pipeline_doc::compile` — that one checks a pipeline
//! document alone, this one checks a pipeline against a scene.

#[cfg(test)]
use crate::types::TextureFormat;

use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::codegen::{self, CodegenOptions};
use wxsl_core::graph::Graph;
use wxsl_core::identity;
use wxsl_core::lighting::{ChannelRequest, GBufferPlan, LightingError, LightingSet};
use wxsl_core::node::NodeRegistry;
use wxsl_core::pipeline::{self as doc};
use wxsl_core::scene::{MaterialEntry, Scene};

use crate::effect::EffectRegistry;
use crate::pipeline::PipelineConfig;

/// What a render setup provides and requires, published as one value.
///
/// The read-only answer to "what does this pipeline need and what does it
/// give a scene": an editor's plan inspector, a tool deciding what to
/// register, an application validating before it builds.
#[derive(Clone, Debug)]
pub struct Capabilities {
    /// The material stages this pipeline's pass list drives — the `stage`
    /// settings of its geometry passes, plus `shadow` when it casts — in
    /// [`MaterialStage`] spelling order.
    pub stages: Vec<String>,
    /// The G-buffer channel plan the pipeline's pass lists are built
    /// under: which targets exist, who asked for them, what they cost.
    pub plan: GBufferPlan,
    /// The lighting set the pipeline shades with.
    pub lighting: LightingSet,
    /// The effects this pipeline's passes name, resolved — a document may
    /// spell a shipped id bare, this is what it means.
    pub effects: Vec<String>,
}

/// One way a scene does not fit a render setup, named well enough to act
/// on. [`RenderSetup::check`] returns *every* one it finds rather than
/// stopping at the first, because a load that fails one material per
/// attempt is a debug session, not a diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incompatibility {
    /// The pipeline's own channel plan does not build — a byte budget or a
    /// field collision — so nothing in the scene can be checked against it.
    Plan {
        /// What the plan refused.
        reason: String,
    },
    /// The pipeline names an effect no registered effect provides.
    Effect {
        /// The pass that names it.
        pass: String,
        /// The id as the document spelled it.
        effect: String,
    },
    /// A material names a lighting model the setup's set does not enable.
    Model {
        /// The material that names it.
        material: String,
        /// The name as the material spelled it.
        model: String,
    },
    /// A material pins a feature macro whose channel the plan does not
    /// carry — the feature handshake
    /// ([ADR 0037](../../docs/adr/0037-semantic-channels.md)), reported
    /// before anything is built rather than at first resolve.
    Feature {
        /// The material that pins it.
        material: String,
        /// The macro the material pinned.
        macro_name: &'static str,
        /// The feature that macro asks for.
        feature: &'static str,
    },
    /// Any other way this material fails to compile under this setup —
    /// still reported here, at check time, rather than at draw time.
    Material {
        /// The material that fails.
        material: String,
        /// What compiling it said.
        reason: String,
    },
}

impl core::fmt::Display for Incompatibility {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Incompatibility::Plan { reason } => write!(f, "{reason}"),
            Incompatibility::Effect { pass, effect } => write!(
                f,
                "pass `{pass}` names effect `{effect}`, which no registered \
                 effect provides"
            ),
            Incompatibility::Model { material, model } => write!(
                f,
                "material `{material}` names lighting model `{model}`, which \
                 the pipeline's lighting set does not enable"
            ),
            Incompatibility::Feature {
                material,
                macro_name,
                feature,
            } => write!(
                f,
                "material `{material}` pins `{macro_name}`, which asks for the \
                 `{feature}` channel, but the pipeline's plan does not carry it — \
                 enable it with `Renderer::set_features` and resolve the scene \
                 against the same plan"
            ),
            Incompatibility::Material { material, reason } => {
                write!(f, "material `{material}` does not compile: {reason}")
            }
        }
    }
}

impl std::error::Error for Incompatibility {}

/// What an application intends to run: a pipeline document, the effects
/// its passes may name, and the config the pass lists are built under —
/// the same three facts a `wxsl_render::Renderer` is configured with, held
/// where they can be checked before a device builds any of it.
#[derive(Clone, Debug)]
pub struct RenderSetup {
    pipeline: Graph,
    effects: EffectRegistry,
    config: PipelineConfig,
}

impl RenderSetup {
    /// A setup for `pipeline`, compiled-under `config`, with `effects` as
    /// the vocabulary its passes name.
    pub fn new(pipeline: Graph, effects: EffectRegistry, config: PipelineConfig) -> Self {
        RenderSetup {
            pipeline,
            effects,
            config,
        }
    }

    /// A setup for a stock preset. The document is the shipped preset's.
    pub fn for_stock(
        stock: crate::pipeline::StockPipeline,
        effects: EffectRegistry,
        config: PipelineConfig,
    ) -> Self {
        RenderSetup::new(stock.document(), effects, config)
    }

    /// What this setup provides, as one value. Fails only when the
    /// pipeline's own channel plan does not build — which [`Self::check`]
    /// reports as an [`Incompatibility::Plan`] instead.
    pub fn capabilities(&self) -> Result<Capabilities, LightingError> {
        Ok(Capabilities {
            stages: provided_stages(&self.pipeline),
            plan: self.config.plan()?,
            lighting: self.config.lighting.clone(),
            effects: required_effects(&self.pipeline)
                .into_iter()
                .map(|spelled| {
                    self.effects
                        .get(&spelled)
                        .map(|effect| effect.id.to_string())
                        .unwrap_or(spelled)
                })
                .collect(),
        })
    }

    /// Every way `scene` does not fit this setup, by name — empty when it
    /// loads clean. Materials are compiled device-free against the same
    /// resolution `SceneResources::load` runs, so what is refused here is
    /// exactly what would have been refused there, only all at once and
    /// before any buffer exists.
    ///
    /// `registry` is the node vocabulary the scene's materials compile
    /// against — the same one the load would take.
    pub fn check(&self, scene: &Scene, registry: &NodeRegistry) -> Vec<Incompatibility> {
        let mut found = self.check_pipeline();
        found.extend(check_scene(
            scene,
            registry,
            &self.config.lighting,
            &self.config.features,
        ));
        found
    }

    /// The setup's own problems that no scene is needed to find: an
    /// effect no registered effect provides. The plan's problems are
    /// [`check_scene`]'s to report — the scene check is where materials
    /// stop for them — so a plan error appears once, not twice.
    fn check_pipeline(&self) -> Vec<Incompatibility> {
        effect_references(&self.pipeline)
            .into_iter()
            .filter(|(_, spelled)| self.effects.get(spelled).is_none())
            .map(|(pass, effect)| Incompatibility::Effect { pass, effect })
            .collect()
    }
}

/// The material stages a pipeline document's pass list drives: the
/// `stage` settings of its geometry passes, plus the shadow stage when it
/// casts, in [`MATERIAL_STAGES`] order.
fn provided_stages(pipeline: &Graph) -> Vec<String> {
    let vocabulary = doc::registry();
    let mut stages: Vec<String> = Vec::new();
    for node in pipeline.nodes() {
        let (id, node) = node;
        let stage = match node.def.as_str() {
            doc::PASS_GEOMETRY => pipeline.setting(&vocabulary, id, doc::SETTING_STAGE),
            doc::PASS_SHADOW => Some(abi::MATERIAL_STAGES[MaterialStage::SHADOW.index()].name),
            _ => None,
        };
        if let Some(stage) = stage {
            if !stages.iter().any(|known| known == stage) {
                stages.push(stage.to_string());
            }
        }
    }
    stages.sort_by_key(|stage| {
        abi::MATERIAL_STAGES
            .iter()
            .position(|known| known.name == stage.as_str())
            .unwrap_or(usize::MAX)
    });
    stages
}

/// The `(pass label, spelled effect id)` pairs a pipeline document's
/// screen passes carry. An unpinned `effect` setting reads as the
/// definition's default — [`Graph::setting`] resolves it — so what a pass
/// names is exactly what the compiler will run.
fn effect_references(pipeline: &Graph) -> Vec<(String, String)> {
    let vocabulary = doc::registry();
    let mut found = Vec::new();
    for node in pipeline.nodes() {
        let (id, node) = node;
        if node.def != doc::PASS_SCREEN {
            continue;
        }
        let spelled = pipeline
            .setting(&vocabulary, id, doc::SETTING_EFFECT)
            .unwrap_or_default()
            .to_string();
        found.push((
            node.label.clone().unwrap_or_else(|| "screen".into()),
            spelled,
        ));
    }
    found
}

/// The effects a pipeline document requires: the ids its screen passes
/// name and its compute passes are derived from, resolved the way the
/// compiler resolves them.
fn required_effects(pipeline: &Graph) -> Vec<String> {
    let mut found: Vec<String> = effect_references(pipeline)
        .into_iter()
        .map(|(_, spelled)| identity::resolve(&spelled).into_owned())
        .collect();
    for node in pipeline.nodes() {
        let (_, node) = node;
        if let Some(effect) = node.def.strip_prefix(doc::PASS_COMPUTE_PREFIX) {
            found.push(effect.to_string());
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Every way `scene` does not fit a plan of `lighting` and `features`,
/// by name — the load-time check, shared by [`RenderSetup::check`] and
/// `wxsl::scene::SceneResources::load`.
///
/// Each material is compiled device-free under the same resolution the
/// load runs, so a scene refused here is exactly the scene the load would
/// have refused — the point is that this says so before any mesh is
/// uploaded, and says *everything* rather than one material per attempt.
pub fn check_scene(
    scene: &Scene,
    registry: &NodeRegistry,
    lighting: &LightingSet,
    features: &[ChannelRequest],
) -> Vec<Incompatibility> {
    let mut found = Vec::new();
    // The plan itself first: a plan that does not build is about the
    // pipeline, and no per-material check can mean anything under it —
    // the generated G-buffer struct *is* the plan, so report the plan and
    // stop.
    if let Err(error) = lighting.plan(features) {
        found.push(Incompatibility::Plan {
            reason: error.to_string(),
        });
        return found;
    }
    for entry in &scene.materials {
        found.extend(check_material(entry, registry, lighting, features));
    }
    found
}

/// Check one material against a plan, by compiling it the way a load
/// would — resolve, then generate one stage, then the feature handshake.
fn check_material(
    entry: &MaterialEntry,
    registry: &NodeRegistry,
    lighting: &LightingSet,
    features: &[ChannelRequest],
) -> Vec<Incompatibility> {
    let material = entry.name.clone();
    let config = entry.config.clone().with_features(features.to_vec());
    let resolved = match config.resolve(lighting) {
        Ok(resolved) => resolved,
        Err(LightingError::UnknownModel { name }) => {
            return vec![Incompatibility::Model {
                material,
                model: name,
            }];
        }
        Err(error) => {
            return vec![Incompatibility::Material {
                material,
                reason: error.to_string(),
            }];
        }
    };
    // The demand half of the handshake is only visible once codegen has
    // overlaid the macros — a graph can pin a feature's macro as exactly
    // as a config can — so one stage is generated and read the way
    // `Material::with_lighting` reads `stages[0]`.
    let generated = codegen::generate(
        &entry.graph,
        registry,
        &CodegenOptions {
            stage: MaterialStage::ALL[0],
            material: resolved.clone(),
            ..CodegenOptions::default()
        },
    );
    let generated = match generated {
        Ok(generated) => generated,
        Err(error) => {
            return vec![Incompatibility::Material {
                material,
                reason: error.to_string(),
            }];
        }
    };
    match resolved.check_feature_demands(&generated.macros) {
        Ok(()) => Vec::new(),
        Err(LightingError::FeatureNotCarried {
            macro_name,
            feature,
        }) => vec![Incompatibility::Feature {
            material,
            macro_name,
            feature,
        }],
        Err(error) => vec![Incompatibility::Material {
            material,
            reason: error.to_string(),
        }],
    }
}

#[cfg(test)]
mod tests {
    //! Device-free: the whole contract is data and pure functions, so the
    //! tests need neither a device nor a shader compiler.

    use wxsl_core::abi;
    use wxsl_core::graph::Graph;
    use wxsl_core::lighting::{default_set, feature_requests, ChannelSource};
    use wxsl_core::macros::{MacroSet, MacroValue};
    use wxsl_core::node::NodeRegistry;
    use wxsl_core::scene::{MaterialEntry, MeshEntry, Scene};

    use super::*;
    use crate::effect::EffectRegistry;
    use crate::pipeline::{PipelineConfig, StockPipeline};

    /// The ABI's own nodes: enough vocabulary for a material graph that
    /// drives nothing but the surface defaults.
    fn abi_registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register_all(abi::context_node_defs());
        registry.register(abi::surface_output_def());
        registry
    }

    /// A material that drives nothing: every surface output falls back to
    /// the ABI's default function, so it compiles against any plan.
    fn plain_material(name: &str) -> MaterialEntry {
        let mut graph = Graph::new(name);
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        MaterialEntry::new(name, graph)
    }

    /// A material with `macro` pinned on: demanding whatever channel that
    /// macro asks for.
    fn pinning_material(name: &str, macro_name: &str) -> MaterialEntry {
        let mut entry = plain_material(name);
        let mut macros = MacroSet::new();
        macros.set(macro_name, MacroValue::Flag(true));
        entry.config.macros = macros;
        entry
    }

    /// A scene with one cube and `materials`.
    fn scene_with(materials: Vec<MaterialEntry>) -> Scene {
        let mut scene = Scene::new("check");
        scene.add_mesh(MeshEntry::new(
            "cube",
            wxsl_core::scene::MeshSource::Cube { size: 1.0 },
        ));
        for material in materials {
            scene.add_material(material);
        }
        scene
    }

    fn deferred_setup(features: &[&str]) -> RenderSetup {
        let features = feature_requests(features).expect("shipped features");
        let config = PipelineConfig {
            features,
            ..PipelineConfig::new(crate::pipeline::TargetConfig::new(
                512,
                512,
                TextureFormat::Bgra8Unorm,
            ))
        };
        RenderSetup::for_stock(StockPipeline::Deferred, EffectRegistry::shipped(), config)
    }

    #[test]
    fn the_deferred_setup_publishes_its_capabilities() {
        let capabilities = deferred_setup(&[]).capabilities().expect("the plan builds");
        assert_eq!(capabilities.stages, vec!["gbuffer", "shadow"]);
        assert_eq!(capabilities.lighting.len(), 1);
        assert!(
            capabilities
                .effects
                .contains(&"wxsl.deferred_lighting".to_string())
                && capabilities.effects.contains(&"wxsl.tonemap".to_string()),
            "the shipped chain's effects, resolved: {:?}",
            capabilities.effects
        );
        // The plan: the base three targets, at the default set's 24 bytes
        // per sample.
        assert_eq!(
            capabilities.plan.layout().len(),
            abi::GBUFFER_BASE_TARGETS.len()
        );
        assert_eq!(
            crate::pipeline::gbuffer_layout_bytes_per_sample(capabilities.plan.layout()),
            24
        );
    }

    #[test]
    fn a_scene_that_fits_the_plan_reports_nothing() {
        let scene = scene_with(vec![plain_material("paint")]);
        assert!(deferred_setup(&[])
            .check(&scene, &abi_registry())
            .is_empty());
    }

    #[test]
    fn a_feature_pin_says_so_by_name_before_anything_is_built() {
        let scene = scene_with(vec![pinning_material("skin", "wxsl_subsurface")]);
        let found = deferred_setup(&[]).check(&scene, &abi_registry());
        assert_eq!(
            found,
            vec![Incompatibility::Feature {
                material: "skin".to_string(),
                macro_name: "wxsl_subsurface",
                feature: "subsurface",
            }],
            "the done-when: by name, device-free"
        );
        // And the same setup, carrying the channel, takes the scene.
        let found = deferred_setup(&["subsurface"]).check(&scene, &abi_registry());
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn every_mismatch_is_reported_at_once() {
        let scene = scene_with(vec![
            pinning_material("skin", "wxsl_subsurface"),
            {
                let mut entry = plain_material("gilded");
                entry.config.model = Some("wxsl.toon".to_string());
                entry
            },
            plain_material("fine"),
        ]);
        let found = deferred_setup(&[]).check(&scene, &abi_registry());
        assert_eq!(found.len(), 2, "both, not one per attempt: {found:?}");
        assert!(found.contains(&Incompatibility::Feature {
            material: "skin".to_string(),
            macro_name: "wxsl_subsurface",
            feature: "subsurface",
        }));
        assert!(found.contains(&Incompatibility::Model {
            material: "gilded".to_string(),
            model: "wxsl.toon".to_string(),
        }));
    }

    #[test]
    fn a_bare_model_spelling_resolves_against_the_shipped_package() {
        let mut entry = plain_material("shaded");
        entry.config.model = Some("pbr".to_string());
        let scene = scene_with(vec![entry]);
        assert!(deferred_setup(&[])
            .check(&scene, &abi_registry())
            .is_empty());
    }

    #[test]
    fn an_effect_no_registered_effect_provides_is_named() {
        let mut pipeline = StockPipeline::Deferred.document();
        let screen = pipeline
            .nodes()
            .find(|(_, node)| node.def == doc::PASS_SCREEN)
            .map(|(id, _)| id)
            .expect("the deferred preset has a screen pass");
        pipeline.set_setting(screen, doc::SETTING_EFFECT, "demo.grain");
        let setup = RenderSetup::new(
            pipeline,
            EffectRegistry::shipped(),
            PipelineConfig::new(crate::pipeline::TargetConfig::new(
                512,
                512,
                TextureFormat::Bgra8Unorm,
            )),
        );
        let found = setup.check_pipeline();
        assert_eq!(
            found,
            vec![Incompatibility::Effect {
                pass: "deferred lighting".to_string(),
                effect: "demo.grain".to_string(),
            }]
        );
    }

    #[test]
    fn a_plan_that_does_not_build_is_reported_before_the_scene() {
        // The full shipped set claims the `lighting` field for its dispatch
        // id; a feature claiming the same field collides with it. The
        // collision is about the pipeline, so it is one `Plan`
        // incompatibility, and no material is asked anything after it.
        let mut config = PipelineConfig::new(crate::pipeline::TargetConfig::new(
            512,
            512,
            TextureFormat::Bgra8Unorm,
        ));
        config.lighting = default_set().expect("the shipped set");
        config.features = vec![ChannelRequest {
            source: ChannelSource::Feature { name: "subsurface" },
            target: abi::GBufferTarget {
                field: "lighting",
                doc: "a collision by construction",
                precision: abi::GBufferPrecision::NormalizedScalar,
            },
        }];
        let setup = RenderSetup::new(
            StockPipeline::Deferred.document(),
            EffectRegistry::shipped(),
            config,
        );
        let scene = scene_with(vec![
            plain_material("paint"),
            pinning_material("skin", "wxsl_subsurface"),
        ]);
        let found = setup.check(&scene, &abi_registry());
        assert_eq!(found.len(), 1, "the plan once, and nothing after it");
        assert!(
            matches!(&found[0], Incompatibility::Plan { reason } if reason.contains("`lighting`")),
            "the collision is named: {:?}",
            found[0]
        );
    }
}
