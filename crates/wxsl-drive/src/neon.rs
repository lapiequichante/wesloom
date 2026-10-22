//! The underglow: neon strips under the car, following its outline, whose
//! light on the road is baked once, in the car's space, into a texture.
//! A flat decal under the car adds it to the frame (`pipeline`); the car's
//! probe does not capture it. No light is shaded for it: the strips cost
//! one texture read where the decal covers.
//!
//! Over it, a ribbon along the strips is their glossy reflection on the
//! road: its vertex stage (`drive.neon_streak`) stretches it towards the
//! viewer when the view is low and along a strip's normal, and keeps it
//! narrow seen from above or along the strip.
//!
//! The light leaves the strips down and out, its cone ending short of the
//! horizon, so it would light nothing of the car above it anyway. The road
//! is taken as flat and of one albedo; the bake ignores the wheels.

use std::error::Error;

use glam::{Mat4, Vec2, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node, NodeId, StageConstraint};
use wxsl::core::material::MaterialConfig;
use wxsl::core::node::NodeRegistry;
use wxsl::core::scene::Tags;
use wxsl::core::Value;
use wxsl::render::{
    DrawItem, GpuContext, Material, MaterialBindings, Mesh, MeshData, Renderer, Vertex,
};

/// The tag the decal's draws carry: drawn by the neon passes alone.
pub const NEON_TAG: &str = "neon";
const COLOR: &str = "color";
const GLOW: &str = "neon_glow";
const SAMPLER: &str = "neon_sampler";

/// One step of the `neons` slider: off, or the strips' colour.
pub struct Neon {
    pub name: &'static str,
    pub color: Option<Vec3>,
}

/// The `neons` slider's steps: the yellow is the hornet paint's orange.
pub const NEONS: [Neon; 5] = [
    Neon {
        name: "off",
        color: None,
    },
    Neon {
        name: "yellow",
        color: Some(Vec3::new(1.0, 0.3, 0.0)),
    },
    Neon {
        name: "red",
        color: Some(Vec3::new(1.0, 0.02, 0.11)),
    },
    Neon {
        name: "blue",
        color: Some(Vec3::new(0.05, 0.25, 1.0)),
    },
    Neon {
        name: "white",
        color: Some(Vec3::new(0.9, 0.95, 1.0)),
    },
];

/// How far inside the body's outline the strips run, well under it so
/// little of their light shows past the body, and how far apart the bake
/// samples them.
const INSET: f32 = 0.25;
const SPACING: f32 = 0.04;
/// How many passes of neighbour averaging round the loop off before it is
/// inset, and after: no bend is then tighter than the inset, whose offset
/// would otherwise fold over itself.
const ROUNDING: usize = 80;
/// A strip's intensity per metre.
const INTENSITY: f32 = 1.6;
/// How far the light leans out from straight down, and its cone: the two
/// add up to less than 90°, under the horizon.
const TILT: f32 = 25.0 * std::f32::consts::PI / 180.0;
const CONE: f32 = 62.0 * std::f32::consts::PI / 180.0;
const _: () = assert!(TILT + CONE < std::f32::consts::FRAC_PI_2);
/// Where the light has faded to nothing, windowed as a ranged light is
/// (ADR 0076): the bake reaches this far past the strips, and its edges
/// are black.
const REACH: f32 = 3.0;
/// The bake's texel, metres, and the road's albedo it is the diffuse
/// response of.
const TEXEL: f32 = 0.04;
const ROAD_ALBEDO: f32 = 0.15;
/// The power the bake's light, over its brightest, is raised to: past 1,
/// it keeps its peak by the strips and falls off faster away from them, a
/// harder glow than the light's own smooth falloff.
const CONTRAST: f32 = 1.8;
/// The radiance past which the neons' core turns white (`drive.neon`):
/// just under the bake's peak at night, so a brighter strip whitens.
const WHITE_KNEE: f32 = 0.5;
/// How far over the road the decal floats, against z-fighting, and how
/// far towards the eye its vertices, the streak's and the car's ground
/// occlusion's move (`drive.toward_eye`): a road a little off flat does
/// not cut them, while a rail in front still hides them. The car is drawn
/// after them (`pipeline`), so it hides them whatever the bias.
const LIFT: f32 = 0.02;
pub const DEPTH_BIAS: f32 = 0.3;
/// The streak's rest width, how many times that it reaches seen grazing
/// along a strip's normal, and its brightness at the strip against the
/// bake's brightest.
const STREAK_WIDTH: f32 = 0.12;
const STREAK_STRETCH: f32 = 8.0;
/// How far the streak turns from a strip's normal towards the eye as it
/// stretches: half way.
const STREAK_TOWARD: f32 = 0.5;
const STREAK_SHARE: f32 = 0.5;
/// How much of the streak's width its brightness takes to rise off the
/// strip.
const STREAK_RISE: f32 = 0.12;
/// How far inside a wheel's inner side the loop turns in behind it, how
/// many passes round that detour's corners, and over how far from the
/// wheel the light comes back.
const WHEEL_MARGIN: f32 = 0.05;
const NOTCH_ROUNDING: usize = 12;
const WHEEL_FADE: f32 = 0.12;

/// The sky's light (`DayLight::sky`) at which the neons show half as
/// much as at night.
const SKY_REFERENCE: f32 = 0.4;

/// How much of the neons' glow shows against a sky lighting the road with
/// `sky`: all of it at night, less the brighter the day, as a glow does
/// against the ambient it adds to.
pub fn against_sky(sky: f32) -> f32 {
    1.0 / (1.0 + sky.max(0.0) / SKY_REFERENCE)
}

/// A piece of strip: its middle, which way is out from the car (level),
/// its length, and how much of its light gets past the wheels: 0 tucked in
/// behind one, 1 clear of them.
#[derive(Clone, Copy, Debug)]
pub struct Emitter {
    pub at: Vec3,
    pub out: Vec3,
    pub length: f32,
    pub weight: f32,
}

/// The strips `height` over the road: one closed loop [`INSET`] inside
/// an outline given as the body's half-width at each depth
/// (`(z, half_width)`, forward being -z), cut into [`SPACING`] pieces in
/// order around it, counter-clockwise seen from below (+x, then +z).
///
/// The loop is the outline's convex hull, rounded, so no wheel arch or
/// bumper detail folds it once inset; any bend still tighter than the inset
/// is cut out of the inset loop, which is rounded again. Where it would
/// run under a wheel (`wheels`: footprints as `(centre, half extents)` in
/// (x, z)), it turns in behind it, and its pieces there are weighted down
/// to nothing, the tyre hiding their light.
pub fn strips(outline: &[(f32, f32)], height: f32, wheels: &[(Vec2, Vec2)]) -> Vec<Emitter> {
    let points: Vec<Vec2> = outline
        .iter()
        .flat_map(|&(z, w)| [Vec2::new(w, z), Vec2::new(-w, z)])
        .collect();
    let hull = convex_hull(&points);
    if hull.len() < 3 {
        return Vec::new();
    }
    let ring = rounded(resample(&hull));
    let inset: Vec<Vec2> = ring
        .iter()
        .zip(normals(&ring))
        .map(|(point, normal)| *point - normal * INSET)
        .collect();
    // Where the offset reversed, it folded: those points go.
    let count = ring.len();
    let kept: Vec<Vec2> = (0..count)
        .filter(|&index| {
            let (previous, next) = ((index + count - 1) % count, (index + 1) % count);
            (inset[next] - inset[previous]).dot(ring[next] - ring[previous]) > 0.0
        })
        .map(|index| inset[index])
        .collect();
    if kept.len() < 3 {
        return Vec::new();
    }
    let mut ring = rounded(resample(&kept));
    // Around each wheel, in behind it: past its inner side by the margin,
    // the detour's corners rounded off.
    for point in &mut ring {
        for (centre, half) in wheels {
            let inside = centre.x.abs() - half.x - WHEEL_MARGIN;
            if point.x * centre.x > 0.0
                && (point.y - centre.y).abs() < half.y + WHEEL_MARGIN
                && point.x.abs() > inside
            {
                point.x = inside.copysign(point.x);
            }
        }
    }
    let ring = resample(&rounded_by(ring, NOTCH_ROUNDING));
    let length = perimeter(&ring) / ring.len() as f32;
    ring.iter()
        .zip(normals(&ring))
        .map(|(at, out)| {
            let gap = wheels
                .iter()
                .map(|(centre, half)| ((*at - *centre).abs() - *half).max(Vec2::ZERO).length())
                .fold(f32::MAX, f32::min);
            Emitter {
                at: Vec3::new(at.x, height, at.y),
                out: Vec3::new(out.x, 0.0, out.y),
                length,
                weight: ((gap - WHEEL_MARGIN * 1.5) / WHEEL_FADE).clamp(0.0, 1.0),
            }
        })
        .collect()
}

/// The convex hull of `points`, counter-clockwise in (x, z) taken as
/// (x, y) (Andrew's monotone chain).
fn convex_hull(points: &[Vec2]) -> Vec<Vec2> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    sorted.dedup();
    if sorted.len() < 3 {
        return sorted;
    }
    let turn = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut hull: Vec<Vec2> = Vec::with_capacity(sorted.len() * 2);
    for pass in [sorted.clone(), sorted.into_iter().rev().collect()] {
        let start = hull.len();
        for point in pass {
            while hull.len() >= start + 2
                && turn(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
            {
                hull.pop();
            }
            hull.push(point);
        }
        hull.pop();
    }
    hull
}

/// The closed loop through `ring`, again with points [`SPACING`] apart
/// along it.
fn resample(ring: &[Vec2]) -> Vec<Vec2> {
    let count = ring.len();
    let total = perimeter(ring);
    let samples = ((total / SPACING).round() as usize).max(3);
    let step = total / samples as f32;
    let mut out = Vec::with_capacity(samples);
    let (mut edge, mut walked) = (0, 0.0);
    for sample in 0..samples {
        let at = sample as f32 * step;
        loop {
            let length = ring[edge].distance(ring[(edge + 1) % count]);
            if walked + length >= at || edge + 1 == count {
                let t = ((at - walked) / length.max(1e-9)).clamp(0.0, 1.0);
                out.push(ring[edge].lerp(ring[(edge + 1) % count], t));
                break;
            }
            walked += length;
            edge += 1;
        }
    }
    out
}

/// `ring` after [`ROUNDING`] passes of each point to a quarter of either
/// neighbour and half itself.
fn rounded(ring: Vec<Vec2>) -> Vec<Vec2> {
    rounded_by(ring, ROUNDING)
}

/// `ring` after `passes` passes of [`rounded`]'s averaging.
fn rounded_by(mut ring: Vec<Vec2>, passes: usize) -> Vec<Vec2> {
    let count = ring.len();
    for _ in 0..passes {
        ring = (0..count)
            .map(|index| {
                (ring[(index + count - 1) % count] + ring[index] * 2.0 + ring[(index + 1) % count])
                    * 0.25
            })
            .collect();
    }
    ring
}

/// Each point's outward normal on a counter-clockwise loop.
fn normals(ring: &[Vec2]) -> Vec<Vec2> {
    let count = ring.len();
    (0..count)
        .map(|index| {
            let along = ring[(index + 1) % count] - ring[(index + count - 1) % count];
            Vec2::new(along.y, -along.x).normalize_or_zero()
        })
        .collect()
}

/// The length of the closed loop through `ring`.
fn perimeter(ring: &[Vec2]) -> f32 {
    let count = ring.len();
    (0..count)
        .map(|index| ring[index].distance(ring[(index + 1) % count]))
        .sum()
}

/// The strips' light on the road, `y = 0` in the car's space: the radiance
/// a white strip gives road of [`ROAD_ALBEDO`], one texel per [`TEXEL`],
/// rows along +z.
pub struct GlowMap {
    pub width: u32,
    pub height: u32,
    /// The corners of the area baked, (x, z).
    pub min: Vec2,
    pub max: Vec2,
    pub texels: Vec<f32>,
}

/// Bake `emitters`' light on the road.
pub fn bake(emitters: &[Emitter]) -> GlowMap {
    let (mut min, mut max) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for emitter in emitters {
        let at = Vec2::new(emitter.at.x, emitter.at.z);
        min = min.min(at - REACH);
        max = max.max(at + REACH);
    }
    if min.x > max.x {
        return GlowMap {
            width: 1,
            height: 1,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            texels: vec![0.0],
        };
    }
    let size = ((max - min) / TEXEL).ceil();
    let (width, height) = (size.x as u32, size.y as u32);
    // Whole texels, centred on the strips: a car symmetric across bakes
    // symmetric texels.
    let centre = (min + max) * 0.5;
    min = centre - size * TEXEL * 0.5;
    max = centre + size * TEXEL * 0.5;
    let (outer, inner) = (CONE.cos(), (CONE * 0.6).cos());
    let aims: Vec<Vec3> = emitters
        .iter()
        .map(|emitter| (Vec3::NEG_Y * TILT.cos() + emitter.out * TILT.sin()).normalize())
        .collect();
    let mut texels = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for column in 0..width {
            let at = min + (Vec2::new(column as f32, row as f32) + 0.5) * TEXEL;
            let ground = Vec3::new(at.x, 0.0, at.y);
            let mut irradiance = 0.0;
            for (emitter, aim) in emitters.iter().zip(&aims) {
                let to = ground - emitter.at;
                let distance_squared = to.length_squared();
                if distance_squared >= REACH * REACH || distance_squared < 1e-8 {
                    continue;
                }
                let direction = to / distance_squared.sqrt();
                let t = ((direction.dot(*aim) - outer) / (inner - outer)).clamp(0.0, 1.0);
                let cone = t * t * (3.0 - 2.0 * t);
                let ratio = distance_squared / (REACH * REACH);
                let window = (1.0 - ratio * ratio).powi(2);
                irradiance += INTENSITY
                    * emitter.length
                    * emitter.weight
                    * cone
                    * window
                    * (-direction.y).max(0.0)
                    / distance_squared;
            }
            texels.push(irradiance * ROAD_ALBEDO / std::f32::consts::PI);
        }
    }
    let peak = texels.iter().copied().fold(0.0, f32::max);
    if peak > 0.0 {
        for texel in &mut texels {
            *texel = peak * (*texel / peak).powf(CONTRAST);
        }
    }
    GlowMap {
        width,
        height,
        min,
        max,
        texels,
    }
}

/// The baked decal and the streak: each a mesh in the car's space, and
/// `drive.neon` over its uv.
pub struct Underglow {
    layers: [Layer; 2],
    /// The colour last set, or `None` while off.
    color: Option<Vec3>,
}

struct Layer {
    mesh: Mesh,
    material: Material,
    bindings: MaterialBindings,
}

impl Layer {
    /// `mesh` drawn with `graph`, reading `glow`.
    fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        registry: &NodeRegistry,
        graph: Graph,
        glow: &GlowMap,
        mesh: &MeshData,
    ) -> Result<Self, Box<dyn Error>> {
        graph
            .validate(registry)
            .map_err(|error| format!("{}: {error}", graph.name()))?;
        let mut config = MaterialConfig::default().with_tags(Tags::from_iter([NEON_TAG]));
        config.cast_shadow = false;
        let material = Material::with_lighting(&graph, registry, &config, renderer.lighting())?;
        let mut bindings = renderer.material_bindings(&gpu.device, &material);
        bindings.set_texture(GLOW, &upload(gpu, glow))?;
        bindings.set_sampler(
            SAMPLER,
            &gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(SAMPLER),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        )?;
        bindings.set(COLOR, Value::Vec3([0.0; 3]))?;
        bindings.upload(&gpu.device, &gpu.queue)?;
        Ok(Layer {
            mesh: Mesh::upload(&gpu.device, graph.name(), mesh),
            material,
            bindings,
        })
    }
}

impl Underglow {
    /// Bake `emitters`, in the car's space, and load the decal and the
    /// streak along them.
    pub fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        registry: &NodeRegistry,
        emitters: &[Emitter],
    ) -> Result<Self, Box<dyn Error>> {
        let hidden = emitters.iter().filter(|e| e.weight < 1.0).count();
        println!("  neons: {} pieces, {hidden} by the wheels", emitters.len());
        let map = bake(emitters);
        let peak = map.texels.iter().copied().fold(0.0, f32::max);
        let decal = Layer::new(
            gpu,
            renderer,
            registry,
            graph(registry, false)?,
            &map,
            &quad(&map),
        )?;
        let streak = Layer::new(
            gpu,
            renderer,
            registry,
            graph(registry, true)?,
            &falloff(peak * STREAK_SHARE),
            &ribbon(emitters),
        )?;
        Ok(Underglow {
            layers: [decal, streak],
            color: None,
        })
    }

    /// Light the neons in `color`, or turn them off.
    pub fn set_color(
        &mut self,
        gpu: &GpuContext,
        color: Option<Vec3>,
    ) -> Result<(), Box<dyn Error>> {
        if color != self.color {
            if let Some(color) = color {
                for layer in &mut self.layers {
                    layer.bindings.set(COLOR, Value::Vec3(color.to_array()))?;
                    layer.bindings.upload(&gpu.device, &gpu.queue)?;
                }
            }
            self.color = color;
        }
        Ok(())
    }

    /// Its draws at `transform`, the car's space to the world, while on.
    pub fn draws(&self, transform: Mat4) -> impl Iterator<Item = DrawItem<'_>> {
        self.layers
            .iter()
            .filter(|_| self.color.is_some())
            .map(move |layer| {
                DrawItem::new(&layer.mesh, &layer.material)
                    .with_bindings(&layer.bindings)
                    .with_transform(transform)
            })
    }
}

/// The streak's brightness: across it along v, rising softly off the
/// strip to `peak`, then easing to nothing at the outer edge, sharp at the
/// strip with a long faint tail so a stretched streak reads as a
/// reflection, not a slab; along u, its strip's weight (0 behind a
/// wheel), where the profile turns about the corner into a rounded end.
fn falloff(peak: f32) -> GlowMap {
    const SIZE: u32 = 64;
    let mut texels = Vec::with_capacity((SIZE * SIZE) as usize);
    for row in 0..SIZE {
        let v = (row as f32 + 0.5) / SIZE as f32;
        let rise = (v / STREAK_RISE).min(1.0);
        let s = (v - STREAK_RISE).max(0.0) / (1.0 - STREAK_RISE);
        for column in 0..SIZE {
            let u = (column as f32 + 0.5) / SIZE as f32;
            let r = Vec2::new(1.0 - u, s).length().min(1.0);
            texels.push(peak * (1.0 - r).powi(5) * rise * rise * (3.0 - 2.0 * rise));
        }
    }
    GlowMap {
        width: SIZE,
        height: SIZE,
        min: Vec2::ZERO,
        max: Vec2::ONE,
        texels,
    }
}

/// One ribbon on the road along the loop of `emitters`: per piece, a
/// vertex under it (v = 0) and one [`STREAK_WIDTH`] out (v = 1), each pair
/// shared by the quads either side, the last joined to the first. A
/// vertex's normal is the loop's outward one, which `drive.neon_streak`
/// moves the outer one along; its u the piece's weight, which rounds the
/// streak off behind the wheels, where the quads between pieces of no
/// weight are left out.
pub fn ribbon(emitters: &[Emitter]) -> MeshData {
    let count = emitters.len();
    let fades: Vec<f32> = emitters.iter().map(|emitter| emitter.weight).collect();
    let mut vertices = Vec::with_capacity(count * 2);
    for (emitter, fade) in emitters.iter().zip(&fades) {
        let inner = Vec3::new(emitter.at.x, LIFT, emitter.at.z);
        for (edge, at) in [(0.0, inner), (1.0, inner + emitter.out * STREAK_WIDTH)] {
            vertices.push(Vertex {
                position: at.to_array(),
                normal: emitter.out.to_array(),
                tangent: [1.0, 0.0, 0.0, 1.0],
                uv: [*fade, edge],
            });
        }
    }
    let mut indices = Vec::with_capacity(count * 6);
    for index in 0..count {
        let next = (index + 1) % count;
        if fades[index] == 0.0 && fades[next] == 0.0 {
            continue;
        }
        let (a, b) = (index as u32 * 2, index as u32 * 2 + 1);
        let (c, d) = (next as u32 * 2, next as u32 * 2 + 1);
        // Counter-clockwise from above, as the terrain's.
        let position = |vertex: u32| Vec3::from_array(vertices[vertex as usize].position);
        let up = (position(c) - position(a))
            .cross(position(b) - position(a))
            .y
            > 0.0;
        indices.extend(if up {
            [a, c, b, b, c, d]
        } else {
            [a, b, c, b, d, c]
        });
    }
    MeshData::new(vertices, indices)
}

/// The quad over the area `map` covers, facing up.
fn quad(map: &GlowMap) -> MeshData {
    let corner = |u: f32, v: f32| Vertex {
        position: [
            map.min.x + (map.max.x - map.min.x) * u,
            LIFT,
            map.min.y + (map.max.y - map.min.y) * v,
        ],
        normal: [0.0, 1.0, 0.0],
        tangent: [1.0, 0.0, 0.0, 1.0],
        uv: [u, v],
    };
    let vertices = vec![
        corner(0.0, 0.0),
        corner(1.0, 0.0),
        corner(0.0, 1.0),
        corner(1.0, 1.0),
    ];
    // Counter-clockwise from above, as the terrain's.
    MeshData::new(vertices, vec![0, 2, 1, 1, 2, 3])
}

/// `map` as a filterable single-channel half-float texture.
fn upload(gpu: &GpuContext, map: &GlowMap) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: map.width,
        height: map.height,
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("drive neon glow"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = map
        .texels
        .iter()
        .flat_map(|value| crate::car::f16_bits(*value).to_le_bytes())
        .collect();
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(map.width * 2),
            rows_per_image: None,
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// `drive.neon` over the mesh's uv, reading its texture through its
/// sampler, tinted by the colour parameter, into the fragment output; for
/// the `streak`, its outer vertices moved by `drive.neon_streak`. The
/// surface output stays at its defaults: the colour written is the node's
/// alone, which leaves the material unlit (ADR 0089).
fn graph(registry: &NodeRegistry, streak: bool) -> Result<Graph, Box<dyn Error>> {
    let mut graph = Graph::new(if streak {
        "drive neon streak"
    } else {
        "drive neon"
    });
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(registry, from, to)
            .map_err(|error| format!("drive neon: {error}"))
    };
    let node = graph.add(Node::new("drive.neon").with_param("knee", Value::F32(WHITE_KNEE)));
    graph.set_stage(node, StageConstraint::Fragment);
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let output = graph.add_node(abi::FRAGMENT_OUTPUT_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    wire(&mut graph, (uv, "out"), (node, "uv"))?;
    for (def, socket, slot) in [
        ("texture.texture_2d", "glow", GLOW),
        ("texture.sampler", "samp", SAMPLER),
    ] {
        let input = graph.add_resolved(registry, Node::new(def).with_setting("name", slot));
        wire(&mut graph, (input, "out"), (node, socket))?;
    }
    let color = graph.add(
        Node::new("param.value")
            .with_setting("name", COLOR)
            .with_param("value", Value::Vec3([1.0; 3])),
    );
    wire(&mut graph, (color, "out"), (node, "color"))?;
    // Every vertex towards the eye by the depth bias; the streak's outer
    // ones stretched too.
    let (offset, inputs): (NodeId, &[(&str, &str)]) = if streak {
        let stretch = graph.add(
            Node::new("drive.neon_streak")
                .with_param("width", Value::F32(STREAK_WIDTH))
                .with_param("stretch", Value::F32(STREAK_STRETCH))
                .with_param("toward", Value::F32(STREAK_TOWARD))
                .with_param("bias", Value::F32(DEPTH_BIAS)),
        );
        let inputs: &[(&str, &str)] = &[
            ("normal_matrix", "normal_matrix"),
            ("view_direction", "view"),
            ("object_normal", "out_dir"),
            ("uv", "uv"),
        ];
        (stretch, inputs)
    } else {
        let lift =
            graph.add(Node::new("drive.toward_eye").with_param("distance", Value::F32(DEPTH_BIAS)));
        let inputs: &[(&str, &str)] = &[
            ("normal_matrix", "normal_matrix"),
            ("view_direction", "view"),
        ];
        (lift, inputs)
    };
    graph.set_stage(offset, StageConstraint::Vertex);
    for (field, socket) in inputs {
        let input = graph.add_node(abi::context_node_id(field));
        wire(&mut graph, (input, "out"), (offset, socket))?;
    }
    let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
    wire(
        &mut graph,
        (offset, "out"),
        (vertex, abi::SOCKET_POSITION_OFFSET),
    )?;
    wire(
        &mut graph,
        (node, "out"),
        (output, abi::SOCKET_FRAGMENT_COLOR),
    )?;
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A car 2 m wide and 4.4 m long, its nose and tail rounded off.
    fn outline() -> Vec<(f32, f32)> {
        (0..=44)
            .map(|step| {
                let z = -2.2 + step as f32 * 0.1;
                let end = (z.abs() - 1.6).max(0.0) / 0.6;
                (z, 1.0 - 0.5 * end * end)
            })
            .collect()
    }

    /// A box 2 m wide and 4.4 m long, square-cornered, its sides notched
    /// by two wheel arches: what folds a naive inset.
    fn boxy() -> Vec<(f32, f32)> {
        (0..=44)
            .map(|step| {
                let z = -2.2 + step as f32 * 0.1;
                let arch = ((z.abs() - 1.3).abs() < 0.35) as u8 as f32 * 0.15;
                (z, 1.0 - arch)
            })
            .collect()
    }

    /// Whether `emitters` go once around, evenly, never turning back.
    fn one_smooth_loop(emitters: &[Emitter]) {
        let count = emitters.len();
        assert!(count > 100);
        let mut turned = 0.0;
        for index in 0..count {
            let [a, b, c] = [0, 1, 2].map(|step| {
                let at = emitters[(index + step) % count].at;
                Vec2::new(at.x, at.z)
            });
            assert!((b - a).length() < SPACING * 1.5, "evenly spaced");
            let turn = (b - a).angle_to(c - b);
            assert!(turn >= -1e-3, "convex, never turning back: {turn}");
            assert!(turn < 0.2, "no sharp bend: {turn}");
            turned += turn;
        }
        assert!((turned - std::f32::consts::TAU).abs() < 0.05, "{turned}");
    }

    #[test]
    fn the_strips_are_one_smooth_loop_inside_the_outline() {
        let emitters = strips(&outline(), 0.18, &[]);
        one_smooth_loop(&emitters);
        let width = |z: f32| 1.0 - 0.5 * ((z.abs() - 1.6).max(0.0) / 0.6).powi(2);
        for emitter in &emitters {
            let at = emitter.at;
            assert!(at.x.abs() < width(at.z) - INSET * 0.5, "{at}");
            assert!(at.z.abs() < 2.2 - INSET * 0.5, "{at}");
            // Out from the car, level.
            assert!(Vec2::new(at.x, at.z).dot(Vec2::new(emitter.out.x, emitter.out.z)) > 0.0);
            assert_eq!(emitter.out.y, 0.0);
        }
        // Around the nose too: the strip bends with it.
        let nose: Vec<&Emitter> = emitters.iter().filter(|e| e.at.z < -2.0 + INSET).collect();
        assert!(nose.iter().any(|e| e.at.x.abs() < 0.2));
        assert!(nose.iter().any(|e| e.at.x.abs() > 0.5));
    }

    #[test]
    fn square_corners_and_wheel_arches_neither_fold_nor_spike_the_loop() {
        let emitters = strips(&boxy(), 0.18, &[]);
        one_smooth_loop(&emitters);
        for emitter in &emitters {
            assert!(emitter.at.x.abs() < 1.0 - INSET * 0.9, "{}", emitter.at);
            assert!(emitter.at.z.abs() < 2.2 - INSET * 0.9, "{}", emitter.at);
        }
    }

    #[test]
    fn the_streak_is_one_closed_ribbon_facing_up() {
        let emitters = strips(&boxy(), 0.18, &[]);
        let mesh = ribbon(&emitters);
        assert_eq!(mesh.vertices.len(), emitters.len() * 2);
        // A quad per piece, the last closing the loop: every vertex shared.
        assert_eq!(mesh.indices.len(), emitters.len() * 6);
        let mut uses = vec![0; mesh.vertices.len()];
        for index in &mesh.indices {
            uses[*index as usize] += 1;
        }
        assert!(
            uses.iter().all(|uses| *uses >= 3),
            "shared by both neighbours"
        );
        let at = |index: u32| Vec3::from_array(mesh.vertices[index as usize].position);
        for pair in mesh.vertices.chunks(2) {
            let (inner, outer) = (&pair[0], &pair[1]);
            assert_eq!((inner.uv[1], outer.uv[1]), (0.0, 1.0));
            let out = Vec3::from_array(outer.position) - Vec3::from_array(inner.position);
            assert!((out.length() - STREAK_WIDTH).abs() < 1e-4);
            assert!(out.normalize().dot(Vec3::from_array(outer.normal)) > 0.999);
        }
        for triangle in mesh.indices.chunks(3) {
            let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
            assert!((at(b) - at(a)).cross(at(c) - at(a)).y > 0.0, "faces up");
        }
    }

    #[test]
    fn the_loop_turns_in_behind_the_wheels_and_their_light_is_cut() {
        let wheels: Vec<(Vec2, Vec2)> = [(-0.8, -1.3), (0.8, -1.3), (-0.8, 1.3), (0.8, 1.3)]
            .map(|(x, z)| (Vec2::new(x, z), Vec2::new(0.13, 0.33)))
            .into();
        let emitters = strips(&boxy(), 0.18, &wheels);
        // Behind each wheel, the loop runs inside its inner side.
        for (centre, half) in &wheels {
            let beside: Vec<&Emitter> = emitters
                .iter()
                .filter(|e| e.at.x * centre.x > 0.0 && (e.at.z - centre.y).abs() < half.y * 0.5)
                .collect();
            assert!(!beside.is_empty());
            for emitter in beside {
                assert!(
                    emitter.at.x.abs() < centre.x.abs() - half.x,
                    "{}",
                    emitter.at
                );
                assert_eq!(emitter.weight, 0.0);
            }
        }
        // Its light, baked, is gone there and still beside the doors.
        let map = bake(&emitters);
        let at = |x: f32, z: f32| {
            let cell = ((Vec2::new(x, z) - map.min) / TEXEL).as_uvec2();
            map.texels[(cell.y * map.width + cell.x) as usize]
        };
        assert!(
            at(1.1, 1.3) < at(1.1, 0.0) * 0.2,
            "{} {}",
            at(1.1, 1.3),
            at(1.1, 0.0)
        );
        let mesh = ribbon(&emitters);
        let inside = |index: u32| {
            let at = mesh.vertices[index as usize].position;
            wheels.iter().any(|(centre, half)| {
                ((Vec2::new(at[0], at[2]) - *centre).abs() - *half).max_element() < 0.0
            })
        };
        // Four cuts, each a gap in the loop.
        assert!(mesh.indices.len() < emitters.len() * 6);
        let fades: Vec<f32> = mesh.vertices.iter().step_by(2).map(|v| v.uv[0]).collect();
        let cuts = (0..fades.len())
            .filter(|&i| fades[i] == 0.0 && fades[(i + 1) % fades.len()] > 0.0)
            .count();
        assert_eq!(cuts, 4);
        for triangle in mesh.indices.chunks(3) {
            assert!(!triangle.iter().all(|&index| inside(index)), "on a wheel");
        }
        // Away from the wheels, untouched.
        assert!(fades.contains(&1.0));
    }

    #[test]
    fn the_glow_fades_as_the_sky_brightens() {
        let at = |hour: f32| against_sky(crate::sky::DayLight::at(hour).sky);
        assert_eq!(at(0.0), 1.0);
        assert!(at(18.6) < at(21.0) && at(18.6) > at(12.0));
        assert!(at(12.0) < 0.2, "{}", at(12.0));
    }

    #[test]
    fn the_bake_is_black_at_its_edges_and_brightest_beside_the_car() {
        let emitters = strips(&outline(), 0.18, &[]);
        let map = bake(&emitters);
        let texel = |column: u32, row: u32| map.texels[(row * map.width + column) as usize];
        for column in 0..map.width {
            assert!(texel(column, 0) < 1e-4 && texel(column, map.height - 1) < 1e-4);
        }
        for row in 0..map.height {
            assert!(texel(0, row) < 1e-4 && texel(map.width - 1, row) < 1e-4);
        }
        // Beside the door, half a metre out, against two metres out.
        let at = |x: f32, z: f32| {
            let cell = ((Vec2::new(x, z) - map.min) / TEXEL).as_uvec2();
            texel(cell.x, cell.y)
        };
        assert!(at(1.3, 0.0) > 0.0);
        assert!(at(1.3, 0.0) > 4.0 * at(3.0, 0.0));
        // Symmetric across the car.
        assert!((at(1.3, 0.0) - at(-1.3, 0.0)).abs() < at(1.3, 0.0) * 0.01);
    }
}
