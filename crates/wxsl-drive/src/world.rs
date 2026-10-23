//! Everything but the car, on the device: the viaduct and its furniture,
//! the terrain, the city and the lamps, with their materials. What does
//! not move and shares a material is merged into one mesh, except the
//! buildings: one mesh per kind, instanced, each building's seed in its
//! instance row.
//!
//! The materials are graphs over the demo's own nodes, `drive.*`: each
//! `.wxsl` file under `shaders/` is a node (ADR 0020), derived when the
//! demo starts and mounted at `package::drive::<name>`, the way the
//! editor mounts a `user.*` function. Every one of them reads the pixel's
//! footprint, so each is pinned to the fragment stage.

use std::error::Error;
use std::path::Path;

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::{AttributeDecl, Graph, Node, NodeId, StageConstraint};
use wxsl::core::material::MaterialConfig;
use wxsl::core::node::{NodeRegistry, ValueType};
use wxsl::core::scene::Tags;
use wxsl::core::Value;
use wxsl::render::MeshData;
use wxsl::render::{
    DrawItem, GpuContext, InstanceAttributes, Material, MaterialBindings, Mesh, Renderer,
    ShaderLibrary,
};

use crate::city::{self, Building, Kind};
use crate::halo::{self, Halos};
use crate::road::{self, Lamp, Road};
use crate::terrain::Ground;

/// The demo's nodes: (module name, source).
const NODES: &[(&str, &str)] = &[
    ("road_surface", include_str!("../shaders/road_surface.wxsl")),
    ("facade", include_str!("../shaders/facade.wxsl")),
    ("ground", include_str!("../shaders/ground.wxsl")),
    ("textured", include_str!("../shaders/textured.wxsl")),
    ("car_ground", include_str!("../shaders/car_ground.wxsl")),
    ("neon", include_str!("../shaders/neon.wxsl")),
    ("neon_streak", include_str!("../shaders/neon_streak.wxsl")),
    ("toward_eye", include_str!("../shaders/toward_eye.wxsl")),
    ("lamp_far", include_str!("../shaders/lamp_far.wxsl")),
    ("lamp_halo", include_str!("../shaders/lamp_halo.wxsl")),
    (
        "lamp_halo_glow",
        include_str!("../shaders/lamp_halo_glow.wxsl"),
    ),
    ("lamp_lens", include_str!("../shaders/lamp_lens.wxsl")),
    (
        "car_ground_transfer",
        include_str!("../shaders/car_ground_transfer.wxsl"),
    ),
];

/// The parameter that says whether it is night, on every material that
/// glows.
pub const LIGHTS_ON: &str = "lights_on";

/// The node library and the shader library with the demo's nodes added.
pub fn libraries() -> Result<(NodeRegistry, ShaderLibrary), Box<dyn Error>> {
    let mut registry = wxsl::stdlib::registry();
    let mut library = wxsl::stdlib_library();
    for (name, source) in NODES {
        let path = format!("package::drive::{name}");
        let definition = wxsl_lang::node_from_source(source, &path)
            .map_err(|errors| format!("shaders/{name}.wxsl: {errors:?}"))?;
        registry.register(definition);
        library.insert(path, *source);
    }
    Ok((registry, library))
}

/// A material and its bindings.
pub struct Surface {
    pub material: Material,
    pub bindings: MaterialBindings,
}

impl Surface {
    /// `graph`'s material, its draws carrying `tags`.
    fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        graph: &Graph,
        registry: &NodeRegistry,
        tags: &[&str],
    ) -> Result<Self, Box<dyn Error>> {
        graph
            .validate(registry)
            .map_err(|error| format!("{}: {error}", graph.name()))?;
        let config = MaterialConfig::default().with_tags(Tags::from_iter(tags.iter().copied()));
        let material = Material::with_lighting(graph, registry, &config, renderer.lighting())?;
        let bindings = renderer.material_bindings(&gpu.device, &material);
        Ok(Surface { material, bindings })
    }

    fn item<'a>(&'a self, mesh: &'a Mesh) -> DrawItem<'a> {
        DrawItem::new(mesh, &self.material).with_bindings(&self.bindings)
    }
}

/// One of the demo's nodes, fed from the fragment context, its own
/// parameters, the named textures and samplers and the per-instance
/// attributes (each named after its socket), wired into the surface
/// output.
#[allow(clippy::too_many_arguments)]
fn node_graph(
    registry: &NodeRegistry,
    name: &str,
    node: &str,
    context: &[(&str, &str)],
    resources: &[Resource],
    constants: &[(&str, Value)],
    parameters: &[(&str, Value)],
    instanced: &[(&str, ValueType)],
    outputs: &[&str],
) -> Result<Graph, Box<dyn Error>> {
    let mut graph = Graph::new(name);
    let surface = graph.add(Node::new(node));
    graph.set_stage(surface, StageConstraint::Fragment);
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(registry, from, to)
            .map_err(|error| format!("{name}: {error}"))
    };
    for (socket, field) in context {
        let input = graph.add_node(abi::context_node_id(field));
        wire(&mut graph, (input, "out"), (surface, socket))?;
    }
    for resource in resources {
        let (def, socket, slot) = match resource {
            Resource::Texture { socket, slot } => ("texture.texture_2d", socket, slot),
            Resource::Sampler { socket, slot } => ("texture.sampler", socket, slot),
        };
        let input = graph.add_resolved(registry, Node::new(def).with_setting("name", *slot));
        wire(&mut graph, (input, "out"), (surface, socket))?;
    }
    for (socket, value) in constants {
        graph.set_param(surface, *socket, *value);
    }
    for (socket, value) in parameters {
        let parameter = graph.add(
            Node::new("param.value")
                .with_setting("name", *socket)
                .with_param("value", *value),
        );
        wire(&mut graph, (parameter, "out"), (surface, socket))?;
    }
    for (socket, ty) in instanced {
        graph.declare_attribute(AttributeDecl::instance(*socket, *ty));
        let input = graph.add(Node::new("input.attribute").with_setting("name", *socket));
        graph
            .set_generic(registry, input, "T", *ty)
            .map_err(|error| format!("{name}: {error}"))?;
        wire(&mut graph, (input, "out"), (surface, socket))?;
    }
    for socket in outputs {
        wire(&mut graph, (surface, socket), (output, socket))?;
    }
    Ok(graph)
}

/// A texture or a sampler a node reads, by the name it is bound under.
enum Resource {
    Texture {
        socket: &'static str,
        slot: &'static str,
    },
    Sampler {
        socket: &'static str,
        slot: &'static str,
    },
}

/// The asphalt's texture set, under `resources/textures/`: (slot, file,
/// sRGB).
const ROAD_TEXTURES: [(&str, &str, bool); 3] = [
    ("road_albedo", "highway/highway-lanes_albedo.png", true),
    ("road_normal", "highway/highway-lanes_normal-ogl.png", false),
    (
        "road_roughness",
        "highway/highway-lanes_roughness.png",
        false,
    ),
];
/// The sampler the asphalt reads its textures through.
const ROAD_SAMPLER: &str = "road_sampler";

/// The concrete's texture set, as the asphalt's: albedo, normal,
/// roughness and metalness, in `drive.textured`'s order. Concrete has no
/// metal: its last slot is bound to a black texel.
const CONCRETE_TEXTURES: [(&str, &str, bool); 3] = [
    (
        "concrete_albedo",
        "concrete_wall/concrete_wall_007_diff_1k.jpg",
        true,
    ),
    (
        "concrete_normal",
        "concrete_wall/concrete_wall_007_nor_gl_1k.png",
        false,
    ),
    (
        "concrete_roughness",
        "concrete_wall/concrete_wall_007_rough_1k.png",
        false,
    ),
];
const CONCRETE_METAL: &str = "concrete_metal";
const CONCRETE_SAMPLER: &str = "concrete_sampler";

/// `drive.textured` over the mesh's metric uv, reading the four textures
/// named by `slots` through `sampler`, a copy every `tile` metres.
fn textured_graph(
    registry: &NodeRegistry,
    name: &str,
    slots: [&'static str; 4],
    sampler: &'static str,
    tile: f32,
) -> Result<Graph, Box<dyn Error>> {
    let [albedo, normal_map, roughness_map, metal_map] = slots;
    node_graph(
        registry,
        name,
        "drive.textured",
        &[
            ("uv", "uv"),
            ("world", "world_position"),
            ("geometric_normal", "world_normal"),
        ],
        &[
            Resource::Texture {
                socket: "albedo",
                slot: albedo,
            },
            Resource::Texture {
                socket: "normal_map",
                slot: normal_map,
            },
            Resource::Texture {
                socket: "roughness_map",
                slot: roughness_map,
            },
            Resource::Texture {
                socket: "metal_map",
                slot: metal_map,
            },
            Resource::Sampler {
                socket: "samp",
                slot: sampler,
            },
        ],
        &[("tile", Value::F32(tile))],
        &[],
        &[],
        &["base_color", "roughness", "metallic", "normal"],
    )
}

/// A plain surface: a colour, a roughness and a metalness.
fn plain_graph(name: &str, color: [f32; 3], roughness: f32, metallic: f32) -> Graph {
    let mut graph = Graph::new(name);
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3(color));
    graph.set_param(output, "roughness", Value::F32(roughness));
    graph.set_param(output, "metallic", Value::F32(metallic));
    graph
}

/// The lamps' paint, metallic grey: (colour, roughness, metalness).
const LAMP_PAINT: ([f32; 3], f32, f32) = ([0.5, 0.51, 0.53], 0.4, 0.8);
/// A lamp lens' glow at full night.
const LAMP_GLOW: [f32; 3] = [30.0, 22.0, 14.0];

pub struct World {
    pub road: Road,
    pub lamps: Vec<Lamp>,
    buildings: Vec<Building>,
    /// Each building's instance row: its seed.
    seeds: Vec<InstanceAttributes>,
    /// (mesh, surface index into `surfaces`, its bounds).
    meshes: Vec<(Mesh, usize, (Vec3, Vec3))>,
    /// One per [`Kind`], in [`Kind::ALL`]'s order.
    kinds: Vec<Mesh>,
    surfaces: Vec<Surface>,
    /// The far lamps' sprites.
    halos: Halos,
    /// The lenses' glow and the screen's height last set.
    glow: [f32; 3],
    screen_height: f32,
}

/// Indices into [`World::surfaces`].
const ASPHALT: usize = 0;
const CONCRETE: usize = 1;
const STEEL: usize = 2;
const GLOW: usize = 3;
const GROUND: usize = 4;
const FACADE: usize = 5;
const RAIL: usize = 6;

impl World {
    pub fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        registry: &NodeRegistry,
    ) -> Result<Self, Box<dyn Error>> {
        let road = Road::new();
        let ground = Ground::new(&road);
        let road_meshes = road.meshes(|x, z| ground.height(x, z));
        let lamps = road.lamps();
        let mut poles = MeshData::new(Vec::new(), Vec::new());
        let mut glow = MeshData::new(Vec::new(), Vec::new());
        let mut lenses = Vec::new();
        for lamp in &lamps {
            lenses.extend(lamp_parts(lamp, &mut poles, &mut glow));
        }
        let buildings = city::buildings(&road, |x, z| ground.height(x, z));
        let terrain = ground.mesh(city::paving(&buildings));
        let device = &gpu.device;
        let ground_outputs = ["base_color", "roughness", "emissive"];
        let graphs = [
            node_graph(
                registry,
                "asphalt",
                "drive.road_surface",
                &[
                    ("road", "uv"),
                    ("world", "world_position"),
                    ("geometric_normal", "world_normal"),
                ],
                &[
                    Resource::Texture {
                        socket: "albedo",
                        slot: ROAD_TEXTURES[0].0,
                    },
                    Resource::Texture {
                        socket: "normal_map",
                        slot: ROAD_TEXTURES[1].0,
                    },
                    Resource::Texture {
                        socket: "roughness_map",
                        slot: ROAD_TEXTURES[2].0,
                    },
                    Resource::Sampler {
                        socket: "samp",
                        slot: ROAD_SAMPLER,
                    },
                ],
                &[
                    ("half_width", Value::F32(road::HALF_WIDTH)),
                    ("inner_edge", Value::F32(road::INNER_EDGE)),
                    ("lane_width", Value::F32(road::LANE_WIDTH)),
                ],
                &[],
                &[],
                &["base_color", "roughness", "normal"],
            )?,
            textured_graph(
                registry,
                "concrete",
                [
                    CONCRETE_TEXTURES[0].0,
                    CONCRETE_TEXTURES[1].0,
                    CONCRETE_TEXTURES[2].0,
                    CONCRETE_METAL,
                ],
                CONCRETE_SAMPLER,
                2.5,
            )?,
            plain_graph("lamp", LAMP_PAINT.0, LAMP_PAINT.1, LAMP_PAINT.2),
            halo::lens_graph(registry, LAMP_PAINT, LAMP_GLOW)?,
            node_graph(
                registry,
                "ground",
                "drive.ground",
                &[
                    ("world", "world_position"),
                    ("normal", "world_normal"),
                    ("paving", "uv"),
                ],
                &[],
                &[],
                &[(LIGHTS_ON, Value::F32(1.0))],
                &[],
                &ground_outputs,
            )?,
            node_graph(
                registry,
                "facade",
                "drive.facade",
                &[("world", "world_position"), ("normal", "world_normal")],
                &[],
                &[],
                &[(LIGHTS_ON, Value::F32(1.0))],
                &[(city::SEED, ValueType::F32)],
                &["base_color", "metallic", "roughness", "emissive"],
            )?,
            plain_graph("rail", [0.3, 0.3, 0.3], 0.8, 0.9),
        ];
        let mut surfaces = graphs
            .iter()
            .enumerate()
            .map(|(index, graph)| {
                // The deck shadows the headlights' beams (`lights::DECK_TAG`).
                let tags: &[&str] = if [ASPHALT, CONCRETE].contains(&index) {
                    &[crate::lights::DECK_TAG]
                } else {
                    &[]
                };
                Surface::new(gpu, renderer, graph, registry, tags)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/textures");
        for (surface, textures, sampler) in [
            (ASPHALT, &ROAD_TEXTURES[..], ROAD_SAMPLER),
            (CONCRETE, &CONCRETE_TEXTURES[..], CONCRETE_SAMPLER),
        ] {
            let bindings = &mut surfaces[surface].bindings;
            for (slot, file, srgb) in textures {
                bindings.set_texture(slot, &load_texture(gpu, &folder.join(file), *srgb)?)?;
            }
            bindings.set_sampler(
                sampler,
                &device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some(sampler),
                    address_mode_u: wgpu::AddressMode::Repeat,
                    address_mode_v: wgpu::AddressMode::Repeat,
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    mipmap_filter: wgpu::MipmapFilterMode::Linear,
                    anisotropy_clamp: 16,
                    ..Default::default()
                }),
            )?;
        }
        surfaces[CONCRETE]
            .bindings
            .set_texture(CONCRETE_METAL, &black_texel(gpu))?;
        for surface in &mut surfaces {
            surface.bindings.upload(device, &gpu.queue)?;
        }
        let meshes = [
            ("drive asphalt", &road_meshes.asphalt, ASPHALT),
            ("drive concrete", &road_meshes.concrete, CONCRETE),
            ("drive rails", &road_meshes.steel, RAIL),
            ("drive lamps", &poles, STEEL),
            ("drive lamp heads", &glow, GLOW),
            ("drive terrain", &terrain, GROUND),
        ]
        .into_iter()
        .flat_map(|(label, data, surface)| {
            let size = if surface == GROUND {
                TERRAIN_CHUNK
            } else {
                CHUNK
            };
            chunks(data, size)
                .into_iter()
                .map(move |(piece, bounds)| (Mesh::upload(device, label, &piece), surface, bounds))
        })
        .collect();
        let seeds = buildings
            .iter()
            .map(|building| InstanceAttributes::new().with(city::SEED, Value::F32(building.seed)))
            .collect();
        let halos = Halos::new(gpu, renderer, registry, &lenses)?;
        Ok(World {
            halos,
            glow: LAMP_GLOW,
            screen_height: 1080.0,
            meshes,
            kinds: Kind::ALL
                .iter()
                .map(|kind| Mesh::upload(device, kind.label(), &kind.mesh()))
                .collect(),
            surfaces,
            buildings,
            seeds,
            lamps,
            road,
        })
    }

    /// How many buildings the city has.
    pub fn building_count(&self) -> usize {
        self.buildings.len()
    }

    /// Light the windows, the neon, the street lights and the lamp heads
    /// for `lights_on`.
    pub fn set_lights_on(
        &mut self,
        gpu: &GpuContext,
        lights_on: f32,
    ) -> Result<(), Box<dyn Error>> {
        for surface in [FACADE, GROUND] {
            let bindings = &mut self.surfaces[surface].bindings;
            bindings.set(LIGHTS_ON, Value::F32(lights_on))?;
            bindings.upload(&gpu.device, &gpu.queue)?;
        }
        self.glow = LAMP_GLOW.map(|channel| channel * lights_on);
        self.set_lamps(gpu)
    }

    /// Tell the far lamps' sprites, and the lenses they take over from,
    /// how many pixels high the screen is.
    pub fn set_screen_height(
        &mut self,
        gpu: &GpuContext,
        height: u32,
    ) -> Result<(), Box<dyn Error>> {
        if self.screen_height != height as f32 {
            self.screen_height = height as f32;
            self.set_lamps(gpu)?;
        }
        Ok(())
    }

    fn set_lamps(&mut self, gpu: &GpuContext) -> Result<(), Box<dyn Error>> {
        let lens = &mut self.surfaces[GLOW].bindings;
        lens.set(halo::GLOW, Value::Vec3(self.glow))?;
        lens.set(halo::SCREEN_HEIGHT, Value::F32(self.screen_height))?;
        lens.upload(&gpu.device, &gpu.queue)?;
        self.halos.set(gpu, self.glow, self.screen_height)
    }

    /// Everything to draw. Nothing here moves, so no draw needs a
    /// previous transform. The buildings come by kind, so the ones a pass
    /// keeps are drawn in a few instanced calls.
    pub fn draws(&self) -> Vec<DrawItem<'_>> {
        let mut draws: Vec<DrawItem<'_>> = self
            .meshes
            .iter()
            .map(|(mesh, surface, (min, max))| {
                self.surfaces[*surface].item(mesh).with_bounds(*min, *max)
            })
            .collect();
        for (building, seed) in self.buildings.iter().zip(&self.seeds) {
            let (min, max) = building.bounds;
            draws.push(
                self.surfaces[FACADE]
                    .item(&self.kinds[building.kind as usize])
                    .with_transform(building.transform)
                    .with_attributes(seed)
                    .with_bounds(min, max),
            );
        }
        draws.extend(self.halos.draws());
        draws
    }
}

/// The ground a piece of a merged mesh covers, metres square: small enough
/// that the camera and each shadow tile cull most of the road.
pub const CHUNK: f32 = 150.0;
/// The terrain's pieces: it stretches kilometres past the road, so
/// coarser, or its draws outnumber everything else's.
const TERRAIN_CHUNK: f32 = 600.0;

/// `mesh` cut into pieces by the [`CHUNK`] square each triangle's centre
/// lies in, each with its bounds, in a fixed order.
pub fn chunks(mesh: &MeshData, size: f32) -> Vec<(MeshData, (Vec3, Vec3))> {
    use std::collections::{BTreeMap, HashMap};
    let mut pieces: BTreeMap<(i32, i32), (MeshData, HashMap<u32, u32>)> = BTreeMap::new();
    for triangle in mesh.indices.chunks(3) {
        let center = triangle
            .iter()
            .map(|&index| Vec3::from_array(mesh.vertices[index as usize].position))
            .sum::<Vec3>()
            / 3.0;
        let key = (
            (center.x / size).floor() as i32,
            (center.z / size).floor() as i32,
        );
        let (piece, remap) = pieces
            .entry(key)
            .or_insert_with(|| (MeshData::new(Vec::new(), Vec::new()), HashMap::new()));
        for &index in triangle {
            let local = *remap.entry(index).or_insert_with(|| {
                piece.vertices.push(mesh.vertices[index as usize]);
                piece.vertices.len() as u32 - 1
            });
            piece.indices.push(local);
        }
    }
    pieces
        .into_values()
        .map(|(piece, _)| {
            let (min, max) = piece.vertices.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(min, max), vertex| {
                    let point = Vec3::from_array(vertex.position);
                    (min.min(point), max.max(point))
                },
            );
            (piece, (min, max))
        })
        .collect()
}

/// A median lamp as boxes: the pole, the arm and the heads' housings in
/// `steel`, a lens under each housing in `glow`, which therefore shines
/// down and to the sides but never up. Returns the lenses' centres.
fn lamp_parts(lamp: &Lamp, steel: &mut MeshData, glow: &mut MeshData) -> [Vec3; 2] {
    let across = lamp.across;
    // x across the road, y up, z along it.
    let basis = |center: Vec3, size: Vec3| {
        Mat4::from_cols(
            across.extend(0.0),
            Vec3::Y.extend(0.0),
            across.cross(Vec3::Y).extend(0.0),
            center.extend(1.0),
        ) * Mat4::from_scale(size)
    };
    let top = lamp.heads[0].midpoint(lamp.heads[1]);
    road::push_box(
        steel,
        basis(
            lamp.foot.midpoint(top) + Vec3::Y * 0.1,
            Vec3::new(0.18, top.y - lamp.foot.y + 0.2, 0.18),
        ),
    );
    road::push_box(
        steel,
        basis(
            top + Vec3::Y * 0.25,
            Vec3::new(road::ARM_REACH * 2.0 + 0.6, 0.14, 0.14),
        ),
    );
    lamp.heads.map(|head| {
        // The housing, a little wider than the lens, hangs from the arm.
        road::push_box(
            steel,
            basis(head + Vec3::Y * (HOUSING_BOTTOM + HOUSING.y * 0.5), HOUSING),
        );
        let lens = head + Vec3::Y * (HOUSING_BOTTOM - halo::LENS.y * 0.5);
        road::push_box(glow, basis(lens, halo::LENS));
        lens
    })
}

/// A lamp head's housing, metres (across, high, along), and how far over
/// the head its underside is, the lens just under it.
const HOUSING: Vec3 = Vec3::new(0.4, 0.09, 0.38);
const HOUSING_BOTTOM: f32 = 0.105;

/// One black texel, for a texture slot with nothing to read.
fn black_texel(gpu: &GpuContext) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("drive black texel"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &[0, 0, 0, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// A PNG or a JPEG on the device with its whole mip chain, halved on the CPU (in
/// sRGB for a colour, which darkens the far mips a little).
fn load_texture(
    gpu: &GpuContext,
    path: &Path,
    srgb: bool,
) -> Result<wgpu::TextureView, Box<dyn Error>> {
    let mut level = image::open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .to_rgba8();
    let (width, height) = level.dimensions();
    let levels = width.max(height).ilog2() + 1;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: path.file_name().and_then(|name| name.to_str()),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for mip in 0..levels {
        if mip > 0 {
            let (w, h) = level.dimensions();
            level = image::imageops::resize(
                &level,
                (w / 2).max(1),
                (h / 2).max(1),
                image::imageops::FilterType::Triangle,
            );
        }
        let (w, h) = level.dimensions();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &level,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
    Ok(texture.create_view(&wgpu::TextureViewDescriptor::default()))
}
