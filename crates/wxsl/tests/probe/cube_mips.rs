//! Shared recording proof: all six faces at four mip levels, then native
//! cube sampling of those levels. This is a backend fixture, not an IBL.
use wxsl_render::{
    effect::{Effect, EffectInput, EffectInputKind, EffectKind, EffectShader},
    graph::RenderGraph,
    pass::{
        Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassState, Policy,
        Read, ResourceDesc,
    },
    types::{Color, TextureFormat},
};

pub const VIEW: Effect = Effect {
    id: "test.cube_mips",
    label: "Cube face/mip probe",
    description: "Native cube sampling of each independently recorded face and mip",
    inputs: &[EffectInput {
        name: "image",
        kind: EffectInputKind::Image,
        description: "Cube",
        history: 0,
    }],
    outputs: &[],
    parameters: &[],
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    shader: EffectShader::Source {
        path: "package::test::cube_mips",
        wxsl: include_str!("cube_mips.wxsl"),
    },
};

pub fn color(face: u32, mip: u32) -> Color {
    Color {
        r: f64::from(face) / 5.0,
        g: f64::from(mip) / 3.0,
        b: f64::from((face + 3 * mip) % 7) / 6.0,
        a: 1.0,
    }
}

pub fn plan(format: wxsl_render::wgpu::TextureFormat) -> RenderGraph {
    let mut graph = RenderGraph::new(format);
    let cube = graph.resource(
        ResourceDesc::color("probe cube", TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 16,
                height: 16,
            })
            .with_dimension(Dimension::Cube, 6)
            .with_mip_levels(4)
            .persistent(0),
    );
    let depth = graph.resource(
        ResourceDesc::color("probe depth mips", TextureFormat::Depth32Float)
            .with_extent(Extent::Fixed {
                width: 16,
                height: 16,
            })
            .with_dimension(Dimension::D2Array, 6)
            .with_mip_levels(4)
            .persistent(0),
    );
    // Deliberately first: dependency ordering, not declaration order, must
    // place this reader after every face/mip writer.
    graph.pass(
        PassDesc::screen("show cube mips", VIEW.id)
            .with_reads([Read::current(cube)])
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
    );
    for mip in 0..4 {
        for face in 0..6 {
            graph.pass(
                PassDesc::geometry(
                    format!("cube face {face} mip {mip}"),
                    DrawSource::Scene(wxsl_core::scene::TagExpr::Never),
                    wxsl_core::abi::MaterialStage::FORWARD_LIT,
                )
                .with_state(
                    PassState::FULLSCREEN.with_depth_format(Some(TextureFormat::Depth32Float)),
                )
                .with_depth(
                    DepthAttachment::clear(depth, 0.75)
                        .with_layer(face)
                        .with_mip(mip),
                )
                .with_color(
                    Attachment::clear(cube, color(face, mip))
                        .with_layer(face)
                        .with_mip(mip),
                )
                .with_policy(Policy::Once),
            );
        }
    }
    graph
}
