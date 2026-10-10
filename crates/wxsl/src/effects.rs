//! Screen effects authored as **graphs** — postprocess as a material over
//! the frame
//! ([ADR 0040](../../../docs/adr/0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md)).
//!
//! # Why here and not in `wxsl-render`
//!
//! For the same reason [`crate::stdlib_library`] is here: the renderer
//! deliberately does not depend on the node library (ADR 0002), and a
//! graph-authored effect needs both — a [`NodeRegistry`] to compile against
//! and a [`ShaderLibrary`](wxsl_render::ShaderLibrary) holding the WXSL the
//! nodes call. This crate is where those two meet, so this is where an
//! effect that is a graph can be built. `wxsl-render` keeps shipping the
//! descriptor forms, and a renderer built without the node library still
//! runs every stock pipeline.
//!
//! # What ships
//!
//! [`tonemap`] is the display transform every stock chain already ends in,
//! rebuilt as a graph — the same five operations, wired instead of written.
//! It registers under the id `tonemap`, which is the id the stock documents
//! name, so [`registry`] hands back a shipped registry whose display
//! transform *is* a graph a user could have authored. Nothing downstream
//! knows: the effect descriptor is the seam, and on the far side of it a
//! generated module is a module.
//!
//! [`fxaa`] is the first effect that arrives as a graph rather than being
//! translated into one, and the reason the `filter` category exists.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = wxsl::stdlib::registry();
//! let effects = wxsl::effects::registry(&registry)?;
//! assert!(effects.get("tonemap").expect("shipped").graph().is_some());
//! # Ok(())
//! # }
//! ```

use wxsl_core::abi;
use wxsl_core::error::CodegenError;
use wxsl_core::graph::Graph;
use wxsl_core::node::{GraphDomain, NodeRegistry};
use wxsl_render::effect::{Effect, EffectRegistry};

/// Every shipped effect, with the graph-authored ones in place of — or
/// beside — the descriptors.
///
/// `tonemap` replaces the shipped descriptor under the same id, which is
/// what makes this a claim rather than a demonstration: every stock
/// pipeline goes on naming `wxsl.tonemap` and gets a graph.
pub fn registry(registry: &NodeRegistry) -> Result<EffectRegistry, CodegenError> {
    Ok(EffectRegistry::shipped()
        .with(tonemap(registry)?)
        .with(fxaa(registry)?)
        .with(vignette(registry)?)
        .with(film_grain(registry)?)
        .with(sky(registry)?)
        .with(chromatic_aberration(registry)?))
}

/// Equirectangular preview of the single-scattering sky in linear radiance.
/// The existing image input supplies the extent, not the colour. This does
/// not replace the scene background or illuminate materials (ADR 0060).
pub fn sky(registry: &NodeRegistry) -> Result<Effect, CodegenError> {
    Effect::from_graph(
        "wxsl.sky",
        "Single-scattering sky",
        "Equirectangular Rayleigh/Mie sky preview; follow with a display transform.",
        sky_graph(registry),
        registry,
    )
}

/// Editable sky preview: UV → direction → optical integration → radiance.
/// Sun position, altitude and medium coefficients are sockets on the sky node;
/// integration budgets are its macros, not runtime uniforms.
pub fn sky_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::in_domain("single-scattering sky", GraphDomain::Screen);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let direction = graph.add_node("space.equirect_to_direction");
    let sky = graph.add_node("lighting.sky_single_scattering");
    graph.set_param(
        sky,
        "sun_direction",
        wxsl_core::node::Value::Vec3([0.0, 0.3, 1.0]),
    );
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);
    wire(&mut graph, registry, (uv, "out"), (direction, "uv"));
    wire(&mut graph, registry, (direction, "out"), (sky, "direction"));
    wire(
        &mut graph,
        registry,
        (sky, "out"),
        (out, abi::SOCKET_SCREEN_COLOR),
    );
    graph
}

/// Edge darkening, as a graph: one call to the `filter.vignette` node over
/// the image, at this pixel's uv and texel size.
pub fn vignette(registry: &NodeRegistry) -> Result<Effect, CodegenError> {
    Effect::from_graph(
        "wxsl.vignette",
        "Vignette",
        "Darken the frame toward its edges, in linear radiance.",
        vignette_graph(registry),
        registry,
    )
}

/// Film grain, as a graph: deterministic per-pixel noise, re-seeded per
/// frame, over the image.
pub fn film_grain(registry: &NodeRegistry) -> Result<Effect, CodegenError> {
    Effect::from_graph(
        "wxsl.film_grain",
        "Film grain",
        "Add deterministic per-pixel grain; stops banding, or dates a picture.",
        film_grain_graph(registry),
        registry,
    )
}

/// Chromatic aberration, as a graph: the red and blue channels pulled
/// toward and away from the centre, green left where it is.
pub fn chromatic_aberration(registry: &NodeRegistry) -> Result<Effect, CodegenError> {
    Effect::from_graph(
        "wxsl.chromatic_aberration",
        "Chromatic aberration",
        "Lens-dispersion fringing; strongest at the frame edge.",
        chromatic_aberration_graph(registry),
        registry,
    )
}

/// The display transform, as a graph: load the image, curve it, encode it.
pub fn tonemap(registry: &NodeRegistry) -> Result<Effect, CodegenError> {
    Effect::from_graph(
        "wxsl.tonemap",
        "tonemap",
        "Curve linear radiance for the display, and encode it. Authored as a graph.",
        tonemap_graph(registry),
        registry,
    )
}

/// Anti-aliasing, as a graph: one call to the `filter.fxaa` node over the
/// image, at this pixel's uv and texel size.
pub fn fxaa(registry: &NodeRegistry) -> Result<Effect, CodegenError> {
    Effect::from_graph(
        "wxsl.fxaa",
        "FXAA",
        "Soften luminance edges without touching anything else.",
        fxaa_graph(registry),
        registry,
    )
}

/// The graph [`tonemap`] compiles.
///
/// Five nodes and a terminal, which is what the hand-written
/// `tonemap.wxsl` is in graph form: read the texel under this pixel, take
/// its colour apart from its alpha, run the library's filmic curve and the
/// library's sRGB encode over the colour, and write the two back together.
/// The curve is `package::color::tonemap_filmic` either way — there is one
/// filmic curve in this repo, and this is another caller of it, not another
/// copy.
pub fn tonemap_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::in_domain("tonemap", GraphDomain::Screen);
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let load = graph.add_node("sample.load_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let curve = graph.add_node("color.tonemap_filmic");
    let encode = graph.add_node("color.linear_to_srgb");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);

    wire(&mut graph, registry, (image, "out"), (load, "tex"));
    wire(&mut graph, registry, (uv, "out"), (load, "uv"));
    wire(&mut graph, registry, (load, "out"), (split, "v"));
    for channel in ["x", "y", "z"] {
        wire(&mut graph, registry, (split, channel), (color, channel));
    }
    wire(&mut graph, registry, (color, "out"), (curve, "color"));
    wire(&mut graph, registry, (curve, "out"), (encode, "color"));
    wire(
        &mut graph,
        registry,
        (encode, "out"),
        (out, abi::SOCKET_SCREEN_COLOR),
    );
    wire(
        &mut graph,
        registry,
        (split, "w"),
        (out, abi::SOCKET_SCREEN_ALPHA),
    );
    graph
}

/// The graph [`fxaa`] compiles.
///
/// Three inputs and one call. What makes it short is that `filter.fxaa` is
/// an ordinary library function over a texture, so the screen domain adds
/// exactly what it has to: which image, where in it, and how big a texel
/// is.
pub fn fxaa_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::in_domain("fxaa", GraphDomain::Screen);
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let texel = graph.add_node(abi::context_node_id("texel"));
    let filter = graph.add_node("filter.fxaa");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);

    wire(&mut graph, registry, (image, "out"), (filter, "image"));
    wire(&mut graph, registry, (uv, "out"), (filter, "uv"));
    wire(&mut graph, registry, (texel, "out"), (filter, "texel"));
    wire(
        &mut graph,
        registry,
        (filter, "out"),
        (out, abi::SOCKET_SCREEN_COLOR),
    );
    graph
}

/// The graph [`vignette`] compiles: load the image, darken its edges.
pub fn vignette_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::in_domain("vignette", GraphDomain::Screen);
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let texel = graph.add_node(abi::context_node_id("texel"));
    let load = graph.add_node("sample.load_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let dark = graph.add_node("filter.vignette");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);

    wire(&mut graph, registry, (image, "out"), (load, "tex"));
    wire(&mut graph, registry, (uv, "out"), (load, "uv"));
    wire(&mut graph, registry, (load, "out"), (split, "v"));
    for channel in ["x", "y", "z"] {
        wire(&mut graph, registry, (split, channel), (color, channel));
    }
    wire(&mut graph, registry, (color, "out"), (dark, "color"));
    wire(&mut graph, registry, (uv, "out"), (dark, "uv"));
    wire(&mut graph, registry, (texel, "out"), (dark, "texel"));
    wire(
        &mut graph,
        registry,
        (dark, "out"),
        (out, abi::SOCKET_SCREEN_COLOR),
    );
    wire(
        &mut graph,
        registry,
        (split, "w"),
        (out, abi::SOCKET_SCREEN_ALPHA),
    );
    graph
}

/// The graph [`film_grain`] compiles: load the image, grain it.
pub fn film_grain_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::in_domain("film grain", GraphDomain::Screen);
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let pixel = graph.add_node(abi::context_node_id("pixel"));
    let time = graph.add_node(abi::context_node_id("time"));
    let load = graph.add_node("sample.load_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let grain = graph.add_node("filter.film_grain");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);

    wire(&mut graph, registry, (image, "out"), (load, "tex"));
    wire(&mut graph, registry, (uv, "out"), (load, "uv"));
    wire(&mut graph, registry, (load, "out"), (split, "v"));
    for channel in ["x", "y", "z"] {
        wire(&mut graph, registry, (split, channel), (color, channel));
    }
    wire(&mut graph, registry, (color, "out"), (grain, "color"));
    wire(&mut graph, registry, (pixel, "out"), (grain, "pixel"));
    wire(&mut graph, registry, (time, "out"), (grain, "time"));
    wire(
        &mut graph,
        registry,
        (grain, "out"),
        (out, abi::SOCKET_SCREEN_COLOR),
    );
    wire(
        &mut graph,
        registry,
        (split, "w"),
        (out, abi::SOCKET_SCREEN_ALPHA),
    );
    graph
}

/// The graph [`chromatic_aberration`] compiles: three loads, the outer
/// two pulled along the vector from the centre.
pub fn chromatic_aberration_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::in_domain("chromatic aberration", GraphDomain::Screen);
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let texel = graph.add_node(abi::context_node_id("texel"));
    let pull = graph.add_node("filter.chromatic_aberration");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);

    wire(&mut graph, registry, (image, "out"), (pull, "image"));
    wire(&mut graph, registry, (uv, "out"), (pull, "uv"));
    wire(&mut graph, registry, (texel, "out"), (pull, "texel"));
    wire(
        &mut graph,
        registry,
        (pull, "out"),
        (out, abi::SOCKET_SCREEN_COLOR),
    );
    graph
}

/// Connect two sockets of a graph this module built.
///
/// Panics rather than returning: the graphs above are fixed, so a wire that
/// does not connect is a bug in this file — the same status a malformed
/// node definition has, and reported the same way.
fn wire(
    graph: &mut Graph,
    registry: &NodeRegistry,
    from: (wxsl_core::graph::NodeId, &str),
    to: (wxsl_core::graph::NodeId, &str),
) {
    graph
        .wire(registry, from, to)
        .unwrap_or_else(|error| panic!("a shipped effect graph is wired wrong: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_registrys_display_transform_is_a_graph() {
        let nodes = wxsl_stdlib::registry();
        let effects = registry(&nodes).expect("the shipped graphs generate");
        let tonemap = effects.get("tonemap").expect("shipped under its own id");
        let graph = tonemap.graph().expect("authored as a graph");
        assert_eq!(graph.domain(), GraphDomain::Screen);
        // Replacing, not appending: the stock documents name `tonemap`, and
        // what they get is this. The other five graph effects are additive.
        assert_eq!(effects.len(), EffectRegistry::shipped().len() + 5);
    }

    #[test]
    fn a_screen_graph_in_a_material_is_refused_by_name() {
        // The domain is load-bearing rather than decorative: the same nodes
        // in a surface graph are a validation error naming the node, not a
        // shader that compiles into something surprising.
        let nodes = wxsl_stdlib::registry();
        let mut graph = fxaa_graph(&nodes);
        graph.set_domain(GraphDomain::Surface);
        let errors = graph.validate(&nodes).expect_err("wrong domain");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, wxsl_core::GraphError::NodeOutsideDomain { .. })),
            "{errors}"
        );
    }

    #[test]
    fn the_generated_tonemap_applies_the_librarys_curve() {
        let nodes = wxsl_stdlib::registry();
        let effect = tonemap(&nodes).expect("generates");
        let wxsl_render::effect::EffectShader::Graph { wxsl, .. } = &effect.shader else {
            panic!("authored as a graph");
        };
        for item in [
            "package::color::tonemap_filmic",
            "package::color::linear_to_srgb",
            "package::sample::load_2d",
        ] {
            assert!(
                wxsl.contains(item),
                "the generated module imports {item}:\n{wxsl}"
            );
        }
        // And it is a complete screen module: the ABI's entry points, over
        // the ABI's context.
        assert!(wxsl.contains(abi::SCREEN_VERTEX_ENTRY));
        assert!(wxsl.contains(abi::SCREEN_FRAGMENT_ENTRY));
    }
}
