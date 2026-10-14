//! Putting the renderer on a demo's pass list.

use super::super::*;

/// Put the renderer on a demo's pass list.
///
/// Stock pipelines go through `set_pipeline`; documents through the
/// public compiler and `set_graph`; hand-built pass lists through
/// `set_graph` directly — the same moves an application makes, and why a
/// gallery demo is not a renderer feature. Features (plan2 P12) and the
/// lighting set validate *against each other* — the feature channel is
/// budgeted against the set that carries it — so the plan is torn down
/// before it is rebuilt: dropping the channels first means the new set
/// compiles alone, and the new demo's channels land on the set that was
/// picked for them.
pub(crate) fn apply(demo: &Demo, renderer: &mut Renderer) -> Result<(), Box<dyn Error>> {
    if !renderer.features().is_empty() && renderer.features() != demo.features {
        renderer.set_features(&[])?;
    }
    // Layer models fill the portable budget alone; the subsurface model
    // joins them because the feature's channel pushes a dispatching set
    // past the portable byte budget. Other demos share the historical
    // wide set. Compare sets before rebuilding the renderer.
    let set = match demo.model {
        Some(name @ ("wxsl.iridescent" | "wxsl.sheen" | "wxsl.subsurface")) => {
            wxsl::core::lighting::LightingSet::single(
                *wxsl::core::lighting::DEFAULT_MODELS
                    .iter()
                    .find(|model| model.name == name)
                    .expect("shipped layer model"),
            )
        }
        Some(_) => wxsl::core::lighting::default_set()?,
        None => wxsl::core::lighting::default_single_set(),
    };
    if renderer.lighting() != &set {
        renderer.set_lighting(set)?;
    }
    if !demo.features.is_empty() && renderer.features() != demo.features {
        renderer.set_features(demo.features)?;
    }
    // The bake table's view rides only with its demo: the next pass list
    // declares no such resource, and a view nothing declares is an error
    // by name — so the demo that owns it takes it back when it leaves.
    if demo.material.is_none() {
        renderer.remove_import("roughness_bake");
    }
    match &demo.pipeline {
        Pipeline::Stock(stock) => {
            renderer.set_pipeline(*stock);
        }
        Pipeline::Document(build) => {
            let document = build();
            let graph = compile_pipeline(
                &document,
                // The registry documents validate against: the shipped
                // vocabulary plus a `pass.compute.<effect>` node per
                // registered compute effect (plan3 N3). A superset of the
                // static registry, so every document that compiled before
                // still does.
                &wxsl::render::document_registry(renderer.effects()),
                renderer.effects(),
                &PipelineConfig::new(renderer.target()),
            )
            .map_err(|error| {
                format!(
                    "the `{}` document does not compile: {error}",
                    document.name()
                )
            })?;
            renderer
                .set_graph(graph)
                .map_err(|error| format!("the `{}` pass list does not run: {error}", demo.name))?;
        }
    }
    // The demo's parameter overrides, through the same call an editor
    // canvas's sliders would drive — the GPU sees them at the next frame,
    // and nothing recompiles (ADR 0042).
    for (pass, name, value) in demo.params {
        renderer
            .set_pass_param(pass, name, Value::F32(*value))
            .map_err(|error| {
                format!(
                    "the `{}` demo could not tune `{name}` on `{pass}`: {error}",
                    demo.name
                )
            })?;
    }
    Ok(())
}
