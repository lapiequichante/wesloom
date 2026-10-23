//! The lamps far away: a lens a few centimetres high covers a pixel at a
//! few hundred metres, then less, and drops in and out of the pixels it
//! no longer fills as the car moves, so the far lamps flicker. Past a
//! few pixels, a sprite on each lens takes its light over: a soft round
//! dot, mipmapped, facing the screen and never under a few pixels across
//! (`drive.lamp_far`, `drive.lamp_halo`), carrying the light the lens
//! would have sent the eye (`drive.lamp_halo_glow`), while the lens'
//! emission gives the same share up (`drive.lamp_lens`). Its pass adds it
//! to the frame (`pipeline`), drawn by no other, before the bloom that
//! spreads it into a halo as it does the lens.

use std::error::Error;

use glam::{Vec2, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node, NodeId, StageConstraint};
use wxsl::core::material::MaterialConfig;
use wxsl::core::node::NodeRegistry;
use wxsl::core::scene::Tags;
use wxsl::core::Value;
use wxsl::render::{
    DrawItem, GpuContext, Material, MaterialBindings, Mesh, MeshData, Renderer, Vertex,
};

/// The tag the sprites' draws carry: drawn by the halo pass alone.
pub const HALO_TAG: &str = "lamp_halo";
/// The parameters a lens and its sprite share: the lens' radiance and the
/// screen's height in pixels.
pub const GLOW: &str = "glow";
pub const SCREEN_HEIGHT: &str = "screen_height";
const HALO: &str = "halo_profile";
const SAMPLER: &str = "halo_sampler";

/// The lens under a lamp head's housing, metres: across the road, high,
/// along it. Its width is what `drive.lamp_far` measures in pixels.
pub const LENS: Vec3 = Vec3::new(0.3, 0.1, 0.28);
/// The sprite's least half size, pixels, and the lens' widths in pixels
/// between which the sprite takes all of its light over.
const PIXELS: f32 = 3.0;
const FULL: f32 = 1.5;
const EMPTY: f32 = 4.0;
/// The least peak of a sprite, against the lens' radiance.
const LEAST: f32 = 1.0;
/// How far towards the eye the sprite moves, clear of the housing.
const LIFT: f32 = 0.5;
/// The profile's texture, texels square.
const SIZE: u32 = 64;

/// `drive.lamp_far` over the position the `stage` sees, its screen height
/// from `height`.
fn far(
    graph: &mut Graph,
    registry: &NodeRegistry,
    stage: StageConstraint,
    height: NodeId,
) -> Result<NodeId, Box<dyn Error>> {
    let node = graph.add(
        Node::new("drive.lamp_far")
            .with_param("size", Value::F32(LENS.x))
            .with_param("pixels", Value::F32(PIXELS))
            .with_param("full", Value::F32(FULL))
            .with_param("empty", Value::F32(EMPTY)),
    );
    graph.set_stage(node, stage);
    let world = graph.add_node(abi::context_node_id("world_position"));
    graph
        .wire(registry, (world, "out"), (node, "world"))
        .map_err(|error| format!("lamp head: {error}"))?;
    graph
        .wire(registry, (height, "out"), (node, "height"))
        .map_err(|error| format!("lamp head: {error}"))?;
    Ok(node)
}

/// A named parameter of `value`.
fn parameter(graph: &mut Graph, name: &str, value: Value) -> NodeId {
    graph.add(
        Node::new("param.value")
            .with_setting("name", name)
            .with_param("value", value),
    )
}

/// The lens' material: `paint` as (colour, roughness, metalness), glowing
/// by the `glow` parameter less what its sprite carries.
pub fn lens_graph(
    registry: &NodeRegistry,
    paint: ([f32; 3], f32, f32),
    glow: [f32; 3],
) -> Result<Graph, Box<dyn Error>> {
    let mut graph = Graph::new("lamp head");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3(paint.0));
    graph.set_param(output, "roughness", Value::F32(paint.1));
    graph.set_param(output, "metallic", Value::F32(paint.2));
    let height = parameter(&mut graph, SCREEN_HEIGHT, Value::F32(1080.0));
    let far = far(&mut graph, registry, StageConstraint::Fragment, height)?;
    let lens = graph.add(Node::new("drive.lamp_lens"));
    graph.set_stage(lens, StageConstraint::Fragment);
    let glow = parameter(&mut graph, GLOW, Value::Vec3(glow));
    graph
        .wire(registry, (glow, "out"), (lens, "glow"))
        .map_err(|error| format!("lamp head: {error}"))?;
    graph
        .wire(registry, (far, "out"), (lens, "far"))
        .map_err(|error| format!("lamp head: {error}"))?;
    graph
        .wire(registry, (lens, "out"), (output, "emissive"))
        .map_err(|error| format!("lamp head: {error}"))?;
    Ok(graph)
}

/// The sprites' material: each corner moved by `drive.lamp_halo`, the
/// profile through `drive.lamp_halo_glow` into the fragment output,
/// unlit (ADR 0089).
fn halo_graph(registry: &NodeRegistry, spread: f32) -> Result<Graph, Box<dyn Error>> {
    let mut graph = Graph::new("drive lamp halo");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(registry, from, to)
            .map_err(|error| format!("drive lamp halo: {error}"))
    };
    let height = parameter(&mut graph, SCREEN_HEIGHT, Value::F32(1080.0));

    let corner = graph.add(Node::new("drive.lamp_halo").with_param("lift", Value::F32(LIFT)));
    graph.set_stage(corner, StageConstraint::Vertex);
    let far_corner = far(&mut graph, registry, StageConstraint::Vertex, height)?;
    wire(&mut graph, (far_corner, "out"), (corner, "far"))?;
    for (field, socket) in [
        ("normal_matrix", "normal_matrix"),
        ("view_direction", "view"),
        ("uv", "uv"),
    ] {
        let input = graph.add_node(abi::context_node_id(field));
        wire(&mut graph, (input, "out"), (corner, socket))?;
    }
    let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
    wire(
        &mut graph,
        (corner, "out"),
        (vertex, abi::SOCKET_POSITION_OFFSET),
    )?;

    let shade = graph.add(
        Node::new("drive.lamp_halo_glow")
            .with_param("side", Value::F32(LENS.x * LENS.y))
            .with_param("bottom", Value::F32(LENS.x * LENS.z))
            .with_param("spread", Value::F32(spread))
            .with_param("least", Value::F32(LEAST)),
    );
    graph.set_stage(shade, StageConstraint::Fragment);
    let far_pixel = far(&mut graph, registry, StageConstraint::Fragment, height)?;
    wire(&mut graph, (far_pixel, "out"), (shade, "far"))?;
    for (field, socket) in [("uv", "uv"), ("view_direction", "view")] {
        let input = graph.add_node(abi::context_node_id(field));
        wire(&mut graph, (input, "out"), (shade, socket))?;
    }
    for (def, socket, slot) in [
        ("texture.texture_2d", "halo", HALO),
        ("texture.sampler", "samp", SAMPLER),
    ] {
        let input = graph.add_resolved(registry, Node::new(def).with_setting("name", slot));
        wire(&mut graph, (input, "out"), (shade, socket))?;
    }
    let glow = parameter(&mut graph, GLOW, Value::Vec3([0.0; 3]));
    wire(&mut graph, (glow, "out"), (shade, "glow"))?;
    let output = graph.add_node(abi::FRAGMENT_OUTPUT_ID);
    wire(
        &mut graph,
        (shade, "out"),
        (output, abi::SOCKET_FRAGMENT_COLOR),
    )?;
    Ok(graph)
}

/// The sprite's profile at `at`, across its square from -1 to 1 each
/// way: a round core, about a pixel wide at the least sprite, so it is
/// never narrower than one, gone at the edge. No glare: the bloom adds
/// the halo around it, as it does around the lens.
fn profile(at: Vec2) -> f32 {
    let r = at.length();
    let t = ((r - 0.7) / 0.3).clamp(0.0, 1.0);
    (-r * r / (2.0 * 0.3 * 0.3)).exp() * (1.0 - t * t * (3.0 - 2.0 * t))
}

/// The profile's levels, finest first, each the average of the one
/// before, and the finest's mean.
fn levels() -> (Vec<Vec<f32>>, f32) {
    let texel = |index: u32| (index as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0;
    let finest: Vec<f32> = (0..SIZE * SIZE)
        .map(|index| profile(Vec2::new(texel(index % SIZE), texel(index / SIZE))))
        .collect();
    let mean = finest.iter().sum::<f32>() / finest.len() as f32;
    let mut levels = vec![finest];
    let mut size = SIZE;
    while size > 1 {
        let half = size / 2;
        let above = levels.last().expect("a level");
        let level = (0..half * half)
            .map(|index| {
                let (x, y) = (index % half * 2, index / half * 2);
                [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .iter()
                    .map(|(dx, dy)| above[((y + dy) * size + x + dx) as usize])
                    .sum::<f32>()
                    * 0.25
            })
            .collect();
        levels.push(level);
        size = half;
    }
    (levels, mean)
}

/// The profile as a filterable single-channel half-float texture with
/// its whole mip chain.
fn upload(gpu: &GpuContext, levels: &[Vec<f32>]) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("drive lamp halo"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (mip, level) in levels.iter().enumerate() {
        let size = SIZE >> mip;
        let bytes: Vec<u8> = level
            .iter()
            .flat_map(|value| crate::car::f16_bits(*value).to_le_bytes())
            .collect();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size * 2),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
    }
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// One sprite per lens in `lenses`: four corners at the lens, uv telling
/// them apart, `drive.lamp_halo` spreading them on screen.
fn sprites(lenses: &[Vec3]) -> MeshData {
    let mut mesh = MeshData::new(Vec::new(), Vec::new());
    for lens in lenses {
        let base = mesh.vertices.len() as u32;
        for uv in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
            mesh.vertices.push(Vertex {
                position: lens.to_array(),
                normal: [0.0, 1.0, 0.0],
                tangent: [1.0, 0.0, 0.0, 1.0],
                uv,
            });
        }
        mesh.indices
            .extend([0, 2, 1, 0, 3, 2].map(|corner| base + corner));
    }
    mesh
}

/// How far past its lenses a piece of the sprites' mesh may reach, for
/// culling: a sprite's half size kilometres away.
const REACH: f32 = 10.0;

pub struct Halos {
    /// (mesh, its bounds).
    meshes: Vec<(Mesh, (Vec3, Vec3))>,
    material: Material,
    bindings: MaterialBindings,
}

impl Halos {
    /// The sprites over `lenses`, dark until [`Halos::set_glow`].
    pub fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        registry: &NodeRegistry,
        lenses: &[Vec3],
    ) -> Result<Self, Box<dyn Error>> {
        let (levels, mean) = levels();
        let graph = halo_graph(registry, 1.0 / mean)?;
        graph
            .validate(registry)
            .map_err(|error| format!("{}: {error}", graph.name()))?;
        let mut config = MaterialConfig::default().with_tags(Tags::from_iter([HALO_TAG]));
        config.cast_shadow = false;
        let material = Material::with_lighting(&graph, registry, &config, renderer.lighting())?;
        let mut bindings = renderer.material_bindings(&gpu.device, &material);
        bindings.set_texture(HALO, &upload(gpu, &levels))?;
        bindings.set_sampler(
            SAMPLER,
            &gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(SAMPLER),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        )?;
        bindings.upload(&gpu.device, &gpu.queue)?;
        let meshes = crate::world::chunks(&sprites(lenses), crate::world::CHUNK)
            .into_iter()
            .map(|(piece, (min, max))| {
                (
                    Mesh::upload(&gpu.device, "drive lamp halos", &piece),
                    (min - Vec3::splat(REACH), max + Vec3::splat(REACH)),
                )
            })
            .collect();
        Ok(Halos {
            meshes,
            material,
            bindings,
        })
    }

    /// Set the lenses' radiance and the screen's height in pixels.
    pub fn set(
        &mut self,
        gpu: &GpuContext,
        glow: [f32; 3],
        height: f32,
    ) -> Result<(), Box<dyn Error>> {
        self.bindings.set(GLOW, Value::Vec3(glow))?;
        self.bindings.set(SCREEN_HEIGHT, Value::F32(height))?;
        self.bindings.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    pub fn draws(&self) -> impl Iterator<Item = DrawItem<'_>> {
        self.meshes.iter().map(|(mesh, (min, max))| {
            DrawItem::new(mesh, &self.material)
                .with_bindings(&self.bindings)
                .with_bounds(*min, *max)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sprite_is_round_soft_and_gone_at_its_edge() {
        assert_eq!(profile(Vec2::ZERO), 1.0);
        assert_eq!(profile(Vec2::new(1.0, 0.0)), 0.0);
        assert_eq!(profile(Vec2::new(0.8, 0.8)), 0.0);
        assert!(profile(Vec2::new(0.3, 0.0)) < profile(Vec2::new(0.1, 0.0)));
        let (levels, mean) = levels();
        assert_eq!(levels.len(), SIZE.ilog2() as usize + 1);
        assert_eq!(levels.last().map(Vec::len), Some(1));
        // Each level keeps the mean: the sprite's light does not change
        // with the mip it reads.
        assert!((levels.last().unwrap()[0] - mean).abs() < 1e-4);
    }

    #[test]
    fn a_sprite_is_two_triangles_on_its_lens() {
        let mesh = sprites(&[Vec3::ONE, Vec3::X]);
        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.indices.len(), 12);
        assert!(mesh.vertices[4..]
            .iter()
            .all(|vertex| vertex.position == [1.0, 0.0, 0.0]));
    }
}
