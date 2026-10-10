//! Shared analytic HDR bake fixture, exported unchanged to Dawn.
use wxsl_core::node::Value;
use wxsl_render::{
    effect::{
        ibl, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
    },
    graph::RenderGraph,
    pass::{Attachment, Extent, PassDesc, Policy, Read, ResourceDesc},
    types::{Color, TextureFormat},
};

/// Authored HDR environment and depth-aware background over either stock path.
pub fn hdr_document(stock: wxsl_render::StockPipeline) -> wxsl_core::graph::Graph {
    use wxsl_core::{
        graph::{Node, SocketRef},
        pipeline as doc,
    };
    let registry = wxsl_render::pipeline_doc::document_registry(
        &wxsl_render::effect::EffectRegistry::shipped(),
    );
    let mut graph = stock.document();
    let tonemap = graph
        .nodes()
        .find(|(_, n)| n.label.as_deref() == Some("tonemap"))
        .unwrap()
        .0;
    let material = graph
        .nodes()
        .filter(|(_, n)| n.def == doc::PASS_GEOMETRY)
        .find(|(_, n)| n.settings.get(doc::SETTING_STAGE).map(String::as_str) != Some("depth_only"))
        .unwrap()
        .0;
    let color = graph
        .disconnect(&registry, &SocketRef::new(tonemap, "image"))
        .unwrap()
        .from;
    let source = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("source HDR")
            .with_setting(doc::SETTING_PRECISION, "float")
            .with_setting(doc::SETTING_IMPORTED, "true"),
    );
    let bake = graph.add(
        Node::new(doc::PASS_ENVIRONMENT)
            .with_label("document IBL")
            .with_setting(doc::SETTING_SIZE, "32")
            .with_setting("diffuse_size", "8")
            .with_setting("mips", "6"),
    );
    let output = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("background radiance")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let background = graph.add(
        Node::new("pass.screen.wxsl.environment_background").with_label("environment background"),
    );
    for (from, to) in [
        ((source, "color"), (bake, "image")),
        ((color.node, color.socket.as_str()), (background, "color")),
        ((material, "depth"), (background, "depth")),
        ((bake, "radiance"), (background, "radiance")),
        ((output, "color"), (background, "into")),
        ((output, "color"), (tonemap, "image")),
    ] {
        graph.wire(&registry, from, to).unwrap();
    }
    graph
}

/// Compile the same document consumed through the FFI and offline host.
pub fn hdr_plan(
    stock: wxsl_render::StockPipeline,
    config: &wxsl_render::PipelineConfig,
) -> RenderGraph {
    let effects = wxsl_render::effect::EffectRegistry::shipped();
    wxsl_render::pipeline_doc::compile(
        &hdr_document(stock),
        &wxsl_render::pipeline_doc::document_registry(&effects),
        &effects,
        config,
    )
    .unwrap()
}
const KIND: EffectKind = EffectKind::Screen {
    vertex_entry: "vertex",
    fragment_entry: "fragment",
};
const INPUT: EffectInput = EffectInput {
    name: "image",
    kind: EffectInputKind::Image,
    description: "Cube radiance",
    history: 0,
};
pub const SOURCE: Effect = Effect {
    id: "test.ibl_source",
    label: "Analytic HDR source",
    description: "Constant or directional HDR equirectangular fixture",
    kind: KIND,
    inputs: &[],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "directional",
        default: Value::U32(0),
    }],
    shader: EffectShader::Source {
        path: "package::test::ibl_source",
        wxsl: include_str!("ibl_source.wxsl"),
    },
};
pub const VIEW: Effect = Effect {
    id: "test.ibl_view",
    label: "IBL bake probe",
    description: "Cardinal directions at diffuse and specular endpoints",
    kind: KIND,
    inputs: &[
        INPUT,
        EffectInput {
            name: "diffuse",
            ..INPUT
        },
        EffectInput {
            name: "specular",
            ..INPUT
        },
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "directional_uv",
        default: Value::U32(0),
    }],
    shader: EffectShader::Source {
        path: "package::test::ibl_view",
        wxsl: include_str!("ibl_view.wxsl"),
    },
};
pub fn effects() -> [Effect; 6] {
    [
        SOURCE,
        VIEW,
        ibl::EQUIRECT_TO_CUBE,
        ibl::DIFFUSE,
        ibl::SPECULAR,
        ibl::RESAMPLE,
    ]
}

/// A real lighting consumer, deliberately declared before its bake writers.
pub fn append_lighting_bake(graph: &mut RenderGraph, directional: bool) {
    let source = graph.resource(
        ResourceDesc::color("lighting HDR", TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 128,
                height: 64,
            })
            .persistent(0),
    );
    graph.pass(
        PassDesc::screen(format!("lighting HDR {directional}"), SOURCE.id)
            .with_color(Attachment::clear(source, Color::BLACK))
            .with_parameter("directional", Value::U32(u32::from(directional)))
            .with_policy(Policy::Once),
    );
    let maps = ibl::append_filtered_bake(graph, source, "lighting IBL", 32, 8, 6);
    graph.declare_environment_maps(maps.diffuse, maps.specular);
}
pub fn plan(format: wxsl_render::wgpu::TextureFormat, directional: bool) -> RenderGraph {
    let mut graph = RenderGraph::new(format);
    let source = graph.resource(
        ResourceDesc::color("analytic HDR", TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 128,
                height: 64,
            })
            .persistent(0),
    );
    graph.pass(
        PassDesc::screen(format!("analytic HDR {directional}"), SOURCE.id)
            .with_color(Attachment::clear(source, Color::BLACK))
            .with_parameter("directional", Value::U32(u32::from(directional)))
            .with_policy(Policy::Once),
    );
    let baked = ibl::append_bake(&mut graph, source, "probe", 32, 8, 3);
    graph.pass(
        PassDesc::screen("view IBL", VIEW.id)
            .with_reads([
                Read::current(baked.radiance),
                Read::current(baked.diffuse),
                Read::current(baked.specular),
            ])
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
    );
    graph
}

/// Small bright emitter: diffuse must integrate energy rather than alias rays.
pub fn hotspot_plan(format: wxsl_render::wgpu::TextureFormat) -> RenderGraph {
    let mut graph = RenderGraph::new(format);
    let source = graph.resource(
        ResourceDesc::color("hotspot HDR", TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 1024,
                height: 512,
            })
            .persistent(0),
    );
    graph.pass(
        PassDesc::screen("hotspot HDR", SOURCE.id)
            .with_color(Attachment::clear(source, Color::BLACK))
            .with_parameter("directional", Value::U32(2))
            .with_policy(Policy::Once),
    );
    let maps = ibl::append_filtered_bake(&mut graph, source, "probe", 128, 16, 3);
    graph.pass(
        PassDesc::screen("view hotspot IBL", VIEW.id)
            .with_reads([
                Read::current(maps.radiance),
                Read::current(maps.diffuse),
                Read::current(maps.specular),
            ])
            .with_parameter("directional_uv", Value::U32(1))
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
    );
    graph
}
