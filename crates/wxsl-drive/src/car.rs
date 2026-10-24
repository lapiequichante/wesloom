//! me_car: the glTF scene (ADR 0080), its driver's skin playing the
//! `driving` clip from a bone texture (ADR 0099), set down on its wheels
//! and moved along the road by one transform per frame. The wheels roll,
//! and the front ones steer into the bends: their nodes turn about their
//! axles and the instances are re-read. Everything else, the body, the
//! cabin and the driver, pitches on the springs as the car speeds up or
//! brakes. Each
//! each draw remembers last frame's transform so the velocity pass sees
//! the spin. When the file has a grayscale PRT bake beside it
//! (`me_car.gray.prt`, ADR 0092), its materials read it: the sky's light
//! is shadowed by the car itself. A decimated copy of the file, when there
//! is one, casts the car's shadows in its place, posed and animated with it.

use std::error::Error;
use std::path::Path;

use glam::{Mat4, Vec2, Vec3};
use wxsl::asset::{BoneTexture, ImportedScene, BONE_FRAME_PARAM, BONE_TEXTURE_SLOT};
use wxsl::core::graph::{Graph, Node, NodeId};
use wxsl::core::node::{NodeRegistry, ValueType};
use wxsl::core::scene::{MaterialEntry, MeshSource};
use wxsl::core::Value;
use wxsl::render::mesh::MeshData;
use wxsl::render::scene::SceneImage;
use wxsl::render::{DrawItem, GpuContext, Renderer};
use wxsl::scene::SceneResources;

use crate::lights::CarLamps;
use crate::neon::{self, Emitter};
use crate::road::Frame;

/// The rate the clips are baked into the bone textures at.
const BONE_FPS: f32 = 60.0;
/// The clip the driver plays.
const DRIVING_CLIP: &str = "driving";

/// One skin's baked clips, uploaded.
struct Bones {
    baked: BoneTexture,
    view: wgpu::TextureView,
}

impl Bones {
    /// Bake skin `skin` through every clip and upload it as half floats.
    fn upload(gpu: &GpuContext, imported: &ImportedScene, skin: usize) -> Self {
        let baked = BoneTexture::bake(imported, skin, BONE_FPS);
        let size = wgpu::Extent3d {
            width: baked.width,
            height: baked.height,
            depth_or_array_layers: 1,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("drive bones"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = baked
            .texels
            .iter()
            .flatten()
            .flat_map(|value| f16_bits(*value).to_le_bytes())
            .collect();
        gpu.queue.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(baked.width * 8),
                rows_per_image: None,
            },
            size,
        );
        Bones {
            baked,
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        }
    }
}

/// `value` as IEEE half-float bits, rounded to nearest.
pub fn f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent >= 31 {
        return sign | 0x7c00;
    }
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let mantissa = (mantissa | 0x80_0000) >> (1 - exponent);
        return sign | ((mantissa + 0x1000) >> 13) as u16;
    }
    sign | (((exponent as u32) << 10) + ((mantissa + 0x1000) >> 13)) as u16
}

/// The car, loaded and ready to draw.
pub struct Car {
    imported: ImportedScene,
    bones: Vec<Bones>,
    clip: Option<usize>,
    pub resources: SceneResources,
    /// From the file's space to the car's: centred, wheels on y = 0,
    /// facing -z.
    align: Mat4,
    /// The car's box in its own space.
    pub size: Vec3,
    /// Whether it has glass, which the frame draws in the refraction tier.
    pub refractive: bool,
    wheels: Vec<Wheel>,
    /// The materials of the headlights' lenses, which glow at night.
    lenses: Vec<usize>,
    /// The headlights' centres in the car's space, left then right, when
    /// the file has them.
    headlights: Option<[Vec3; 2]>,
    /// The headlights' two optics each, left then right, inside the
    /// housing just behind the glass: what lights the housing from within.
    optics: Option<[Vec3; 4]>,
    /// How large one optic is: a quarter of its headlight across, at most
    /// half its height.
    optic_radius: f32,
    /// The tail lights' centres, the same way.
    tail_lights: Option<[Vec3; 2]>,
    /// The tail lights' materials and the emission the file gives them.
    tails: Vec<(usize, [f32; 3])>,
    /// How lit the tail lights are as position lights (0 by day, 1 at
    /// night) and as brake lights (0 to 1).
    tail_glow: (f32, f32),
    /// Each draw's transform in the file's space last frame, in
    /// [`SceneResources::draw_list`] order.
    previous_locals: Vec<Mat4>,
    /// The decimated stand-in the shadow passes draw instead of the car,
    /// tagged [`crate::lights::CAR_SHADOW_TAG`].
    shadow: Option<Box<Car>>,
    /// The body's pitch this frame and last, radians, nose up positive.
    pitch: (f32, f32),
    /// Whether its materials read a PRT bake.
    prt: bool,
    /// The PRT strength last set.
    prt_strength: f32,
    /// The body paint's material, when the file has it, and whether it is
    /// compiled for an iridescent paint.
    paint: Option<(usize, bool)>,
    /// The paint's flakes ([`crate::flakes`]), for the car itself; the
    /// shadow's stand-in keeps the file's.
    flakes: Option<wgpu::TextureView>,
    /// The windows' materials.
    windows: Vec<usize>,
}

/// A wheel: where its axle is, its radius, whether it steers, and the
/// nodes that turn with it.
struct Wheel {
    center: Vec3,
    radius: f32,
    /// Half its width, across the car.
    half_width: f32,
    /// Ahead of the car's middle: it steers.
    front: bool,
    /// Every node under it, which the body's pitch leaves alone.
    nodes: Vec<usize>,
    /// (node, its rest transform, its parent's world transform, whether
    /// it rolls): the rim, the tyre and the disc roll, the calliper only
    /// steers.
    parts: Vec<(usize, Mat4, Mat4, bool)>,
}

/// The materials of me_car's headlights, which the file leaves unlit.
const LENS_MATERIALS: &[&str] = &["Headlight", "hl inside", "ll shell"];
/// The parameter their glow is, and its radiance at night at an optic's
/// middle: half again the street lamps' own.
const LENS_GLOW: &str = "headlight_glow";
const LENS_RADIANCE: f32 = 350.0;

/// Give the headlights' materials an emission the night can turn up: a
/// `headlight_glow` parameter, the radiance at the middle of each optic,
/// through `drive.headlight_glow`'s hot spots into their surface's
/// `emissive`, which the file leaves unwired. They cast no shadow, so the
/// beam, starting just ahead of the glass, is never caught by its edge.
/// Their indices.
fn light_lenses(imported: &mut ImportedScene) -> Result<Vec<usize>, Box<dyn Error>> {
    use wxsl::core::graph::SocketRef;
    let surface = |graph: &Graph| {
        graph
            .nodes()
            .find(|(_, node)| node.def == wxsl::core::abi::SURFACE_OUTPUT_ID)
            .map(|(id, _)| id)
    };
    let lenses: Vec<usize> = imported
        .scene
        .materials
        .iter()
        .enumerate()
        .filter(|(_, material)| {
            LENS_MATERIALS.contains(&material.name.as_str())
                && surface(&material.graph).is_some_and(|output| {
                    material
                        .graph
                        .edge_into(&SocketRef::new(output, "emissive"))
                        .is_none()
                })
        })
        .map(|(index, _)| index)
        .collect();
    // The optics' middles on the glass, the two headlights told apart
    // about the middle of the four, and how wide one optic is: a quarter
    // of a headlight's box across.
    let boxes = lamp_boxes(imported, &lenses, Vec3::ZERO)
        .map(|[left, right]| (left.0.x.min(right.0.x) + left.1.x.max(right.1.x)) * 0.5)
        .and_then(|middle| lamp_boxes(imported, &lenses, Vec3::new(middle, 0.0, 0.0)));
    let middles = boxes.map(|boxes| optic_middles(boxes, 0.0));
    let width = boxes.map_or(0.0, |[left, right]| {
        ((left.1.x - left.0.x) + (right.1.x - right.0.x)) * 0.125
    });
    let registry = crate::world::registry();
    for &index in &lenses {
        // From the file's space into the lens' own, where its fragments'
        // positions are: the hot spots ride with the mesh.
        let world = imported
            .scene
            .instances
            .iter()
            .find(|instance| instance.material == index)
            .map_or(Mat4::IDENTITY, |instance| {
                Mat4::from_cols_array(&imported.scene.instance_world(instance))
            });
        let to_object = world.inverse();
        let scale = world.transform_vector3(Vec3::X).length().max(1e-6);
        let material = &mut imported.scene.materials[index];
        material.config.cast_shadow = false;
        let name = material.name.clone();
        let graph = &mut material.graph;
        let output = surface(graph).expect("filtered on its surface output");
        let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
            graph
                .wire(&registry, from, to)
                .map_err(|error| format!("{name}: {error}"))
        };
        let glow = graph.add(
            Node::new("param.value")
                .with_setting("name", LENS_GLOW)
                .with_param("value", Value::Vec3([0.0; 3])),
        );
        let mut spots = Node::new("drive.headlight_glow")
            .with_param("radius", Value::F32(width * OPTIC_SPOT / scale))
            .with_param("rim", Value::F32(OPTIC_RIM));
        // Without headlights to find, the spots are nowhere and the glass
        // glows at its rim alone.
        let middles = middles.unwrap_or([Vec3::splat(1e9); 4]);
        for (socket, middle) in ["a", "b", "c", "d"].into_iter().zip(middles) {
            spots = spots.with_param(
                socket,
                Value::Vec3(to_object.transform_point3(middle).to_array()),
            );
        }
        let spots = graph.add(spots);
        // Read per vertex and interpolated: the fragment stage has no
        // object space.
        let position = graph.add_node(wxsl::core::abi::context_node_id("object_position"));
        graph.set_stage(position, wxsl::core::graph::StageConstraint::Vertex);
        wire(graph, (glow, "out"), (spots, "glow"))?;
        wire(graph, (position, "out"), (spots, "position"))?;
        wire(graph, (spots, "out"), (output, "emissive"))?;
    }
    Ok(lenses)
}

/// The box of each lamp of `materials`, left then right, in the file's
/// space.
fn lamp_boxes(
    imported: &ImportedScene,
    materials: &[usize],
    center: Vec3,
) -> Option<[(Vec3, Vec3); 2]> {
    let scene = &imported.scene;
    let mut sides = [(Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)); 2];
    for instance in scene
        .instances
        .iter()
        .filter(|instance| materials.contains(&instance.material))
    {
        let MeshSource::Embedded { index } = scene.meshes[instance.mesh].source else {
            continue;
        };
        let world = Mat4::from_cols_array(&scene.instance_world(instance));
        for vertex in &imported.meshes[index].vertices {
            let point = world.transform_point3(Vec3::from_array(vertex.position));
            let side = &mut sides[usize::from(point.x > center.x)];
            side.0 = side.0.min(point);
            side.1 = side.1.max(point);
        }
    }
    (sides[0].0.x <= sides[0].1.x && sides[1].0.x <= sides[1].1.x).then_some(sides)
}

/// Where each lamp of `boxes` sends its beam from: the middle of its box
/// across and up, past its outer face — well ahead of a headlight's glass
/// (`-z`), just behind a tail light's. Out there the housing never
/// shadows its own beam, and the body around the lamp is behind the
/// light, out of its cone: from inside the housing, the beam lit the
/// paint around the glass from a few centimetres, far brighter than the
/// glass itself glows.
fn lamp_centers(boxes: [(Vec3, Vec3); 2], front: bool) -> [Vec3; 2] {
    boxes.map(|(min, max)| {
        let middle = (min + max) * 0.5;
        let z = if front {
            min.z - HEADLIGHT_CLEARANCE
        } else {
            max.z + LAMP_CLEARANCE
        };
        Vec3::new(middle.x, middle.y, z)
    })
}

/// A headlight's two optics' middles, side by side across its box,
/// halfway up and `depth` behind the glass: the left headlight's outer
/// then inner, then the right's.
fn optic_middles(
    [(left_min, left_max), (right_min, right_max)]: [(Vec3, Vec3); 2],
    depth: f32,
) -> [Vec3; 4] {
    let pair = |min: Vec3, max: Vec3| {
        let at = |across: f32| {
            Vec3::new(
                min.x + (max.x - min.x) * across,
                (min.y + max.y) * 0.5,
                min.z + depth,
            )
        };
        [at(0.25), at(0.75)]
    };
    let [a, b] = pair(left_min, left_max);
    let [c, d] = pair(right_min, right_max);
    [a, b, c, d]
}

/// How far past the tail lights' lenses their light starts, and how far
/// ahead of the headlights' glass their beams do.
const LAMP_CLEARANCE: f32 = 0.03;
const HEADLIGHT_CLEARANCE: f32 = 0.16;
/// How far behind a headlight's glass its optics' lights are.
const OPTIC_DEPTH: f32 = 0.05;
/// An optic's hot spot: how far from its middle its glow falls to 1/e,
/// as a share of the optic's width, and the share left at its edge.
const OPTIC_SPOT: f32 = 0.35;
const OPTIC_RIM: f32 = 0.08;
/// How many slices along the car the underglow's outline is measured in,
/// and up to what height off the road the body counts for it.
const NEON_BINS: usize = 96;
const NEON_BODY_HEIGHT: f32 = 0.45;
/// How high the underglow's strips are, in wheel radii off the road: just
/// under the sills.
const NEON_HEIGHT: f32 = 0.55;
/// The material of me_car's tail lights.
const TAIL_MATERIAL: &str = "RearLights";
/// The emission parameter glTF import gives a material.
const EMISSIVE_PARAM: &str = "emissive_factor";
/// The tail lights' emission at night and braking, on top, as fractions
/// of the file's: dimmer than the file at night, so braking shows over
/// what the tone map would otherwise already saturate.
const TAIL_GLOW: f32 = 0.3;
const BRAKE_GLOW: f32 = 3.0;

/// The material of me_car's body.
const PAINT_MATERIAL: &str = "Paint 1 Carmine";
/// The texture its flakes are, a normal map.
const FLAKES_SLOT: &str = "normal_texture";
/// How many times the generated flakes repeat across the body's uv, where
/// the file's 128² map repeats 80 times.
const FLAKES_REPEAT: f32 = 8.0;

/// Repeat `entry`'s normal map `repeat` times across its uv. The import
/// bakes `KHR_texture_transform`'s rotation and scale into the rows of two
/// `vector.dot` nodes; the paint samples nothing else through one, and
/// its offset is whole tiles, which a random map does not show.
fn repeat_flakes(entry: &mut MaterialEntry, repeat: f32) {
    let dots: Vec<NodeId> = entry
        .graph
        .nodes()
        .filter(|(_, node)| node.def == "vector.dot")
        .map(|(id, _)| id)
        .collect();
    for id in dots {
        let Some(node) = entry.graph.node_mut(id) else {
            continue;
        };
        if let Some(Value::Vec2(row)) = node.params.get_mut("b") {
            let length = Vec2::from(*row).length();
            if length > 0.0 {
                *row = (Vec2::from(*row) * (repeat / length)).to_array();
            }
        }
    }
}

/// A body paint, over the clear-coated, flaked paint me_car comes with.
pub struct Paint {
    pub name: &'static str,
    pub base_color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    /// The flakes' normal map strength, 0 for a smooth paint.
    pub flakes: f32,
    /// The clear coat's strength and roughness.
    pub clearcoat: (f32, f32),
    /// A thin film instead of the clear coat: its IOR, and its thickness
    /// in nm on the sides and on the top, between which it follows the
    /// normal: the colours band along the body's curves.
    pub iridescence: Option<(f32, f32, f32)>,
}

/// The paints the `car color` slider steps through: first a hornet
/// yellow, brighter than me_car's own fully metallic orange.
pub const PAINTS: [Paint; 6] = [
    Paint {
        name: "yellow",
        base_color: [1.0, 0.3, 0.0],
        metallic: 0.35,
        roughness: 0.3,
        flakes: 0.5,
        clearcoat: (1.0, 0.053),
        iridescence: None,
    },
    Paint {
        name: "metallic red",
        base_color: [0.42, 0.012, 0.016],
        metallic: 1.0,
        roughness: 0.38,
        flakes: 0.9,
        clearcoat: (1.0, 0.03),
        iridescence: None,
    },
    Paint {
        name: "blue clear coat",
        base_color: [0.01, 0.06, 0.32],
        metallic: 0.0,
        roughness: 0.45,
        flakes: 0.0,
        clearcoat: (1.0, 0.0),
        iridescence: None,
    },
    Paint {
        name: "white",
        base_color: [0.85, 0.85, 0.83],
        metallic: 0.0,
        roughness: 0.35,
        flakes: 0.15,
        clearcoat: (1.0, 0.02),
        iridescence: None,
    },
    Paint {
        name: "matte black",
        base_color: [0.01, 0.01, 0.01],
        metallic: 0.9,
        roughness: 0.8,
        flakes: 0.5,
        clearcoat: (0.0, 0.5),
        iridescence: None,
    },
    Paint {
        name: "pearl",
        base_color: [0.8, 0.8, 0.82],
        metallic: 1.0,
        roughness: 0.12,
        flakes: 0.3,
        clearcoat: (0.0, 0.0),
        iridescence: Some((2.0, 250.0, 800.0)),
    },
];

/// The materials of the windows the `window color` slider tints: the side
/// windows and the windscreen, not the lamps' glass.
const GLASS_MATERIALS: &[&str] = &["Windows", "Glass"];

/// The headlights' cover glass, which the windows' clear look replaces
/// too (moi.glb tints it orange); the slider leaves it clear.
const LAMP_GLASS: &[&str] = &["Glass.front"];

/// The surface fields a window look sets and the parameters feeding them,
/// wired when the file has none: the tint and coverage, the share let
/// through, the IOR and a thin film. The glass is thin-walled and drawn in
/// the refraction tier (ADR 0093), which reads these from the surface
/// whatever the lighting model, so a look is bindings only, never a
/// recompile.
const GLASS_PARAMS: [(&str, &str); 6] = [
    ("base_color", "base_color_factor"),
    ("transmission", "transmission_factor"),
    ("ior", "ior"),
    ("iridescence_strength", "iridescence_factor"),
    ("iridescence_ior", "iridescence_ior"),
    ("iridescence_thickness", "iridescence_thickness"),
];

/// A look for the windows.
pub struct Glass {
    pub name: &'static str,
    /// The tint, its coverage (alpha), the share of light let through and
    /// the IOR.
    pub body: ([f32; 4], f32, f32),
    /// A thin film: its IOR and thickness in nm.
    pub iridescence: Option<(f32, f32)>,
}

impl Glass {
    /// Its [`GLASS_PARAMS`] values.
    fn values(&self) -> [Value; 6] {
        let (tint, transmission, ior) = self.body;
        let (film_ior, thickness) = self.iridescence.unwrap_or((1.3, 300.0));
        let film = if self.iridescence.is_some() { 1.0 } else { 0.0 };
        [
            Value::Vec4(tint),
            Value::F32(transmission),
            Value::F32(ior),
            Value::F32(film),
            Value::F32(film_ior),
            Value::F32(thickness),
        ]
    }
}

/// The looks the `window color` slider steps through. The first is the
/// windows' and the headlights' glass at load: a faint blue-green, as
/// float glass is seen through, letting most light through.
pub const GLASSES: [Glass; 4] = [
    Glass {
        name: "transparent",
        body: ([0.86, 0.95, 0.92, 1.0], 0.92, 1.5),
        iridescence: None,
    },
    Glass {
        name: "smoked",
        body: ([0.1, 0.1, 0.11, 1.0], 1.0, 1.5),
        iridescence: None,
    },
    // me_car.glb's windows' film over a darker, less clear pane.
    Glass {
        name: "iridescent",
        body: ([0.3, 0.3, 0.32, 1.0], 0.8, 1.5),
        iridescence: Some((1.8, 330.0)),
    },
    // A dark, dense pane under a thinner film: a mirrored visor's teal.
    Glass {
        name: "teal mirror",
        body: ([0.2, 0.2, 0.2, 1.0], 1.0, 2.6),
        iridescence: Some((2.0, 280.0)),
    },
];

/// Give each window and headlight glass material in `imported` every
/// [`GLASS_PARAMS`] parameter, its value the first look's, so that it is
/// also what a recompile restores; the windows' material indices.
fn glass_params(
    imported: &mut ImportedScene,
    registry: &NodeRegistry,
) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut windows = Vec::new();
    for (index, entry) in imported.scene.materials.iter_mut().enumerate() {
        let window = GLASS_MATERIALS.contains(&entry.name.as_str());
        if !window && !LAMP_GLASS.contains(&entry.name.as_str()) {
            continue;
        }
        let graph = &mut entry.graph;
        let [surface] = graph.surface_outputs(registry)[..] else {
            return Err(format!("{}: one surface output expected", entry.name).into());
        };
        for ((field, name), value) in GLASS_PARAMS.into_iter().zip(GLASSES[0].values()) {
            let present = graph.nodes().find(|(_, node)| {
                node.def == "param.value"
                    && node.settings.get("name").map(String::as_str) == Some(name)
            });
            if let Some((node, _)) = present {
                graph.set_param(node, "value", value);
                continue;
            }
            let node = graph.add(
                Node::new("param.value")
                    .with_setting("name", name)
                    .with_param("value", value),
            );
            graph
                .wire(registry, (node, "out"), (surface, field))
                .map_err(|error| format!("{}: {error}", entry.name))?;
        }
        if window {
            windows.push(index);
        }
    }
    Ok(windows)
}

/// Turn the clear-coated paint `entry` into a thin-film one: the
/// iridescent model, its film fed by parameters, the thickness mixed from
/// `iridescence_thickness_min` (sides) to `iridescence_thickness` (top) by
/// the world normal's height. The clear coat's outputs stay wired; the
/// model does not read them.
fn iridescent_paint(
    entry: &mut MaterialEntry,
    registry: &NodeRegistry,
) -> Result<(), Box<dyn Error>> {
    let graph = &mut entry.graph;
    let [surface] = graph.surface_outputs(registry)[..] else {
        return Err(format!("{}: one surface output expected", entry.name).into());
    };
    let name = entry.name.clone();
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(registry, from, to)
            .map_err(|error| format!("{name}: {error}"))
    };
    let param = |graph: &mut Graph, name: &str, value: f32| {
        graph.add(
            Node::new("param.value")
                .with_setting("name", name)
                .with_param("value", Value::F32(value)),
        )
    };
    for (field, name, value) in [
        ("iridescence_strength", "iridescence_factor", 1.0),
        ("iridescence_ior", "iridescence_ior", 1.3),
    ] {
        let node = param(graph, name, value);
        wire(graph, (node, "out"), (surface, field))?;
    }
    let thin = param(graph, "iridescence_thickness_min", 250.0);
    let thick = param(graph, "iridescence_thickness", 650.0);
    let normal = graph.add(Node::new("input.world_normal"));
    let axes = graph.add(Node::new("convert.split.vec3f"));
    wire(graph, (normal, "out"), (axes, "v"))?;
    let mix = graph.add(Node::new("math.mix"));
    graph
        .set_generic(registry, mix, "T", ValueType::F32)
        .map_err(|error| format!("{}: {error}", entry.name))?;
    wire(graph, (thin, "out"), (mix, "a"))?;
    wire(graph, (thick, "out"), (mix, "b"))?;
    wire(graph, (axes, "y"), (mix, "t"))?;
    wire(graph, (mix, "out"), (surface, "iridescence_thickness"))?;
    entry.config.model = Some("wxsl.iridescent".to_string());
    Ok(())
}

/// Wire the grayscale PRT bake beside `path`, if there is one, into
/// `imported`'s materials and its streams onto `meshes`; whether there
/// was.
///
/// The transfer is a ratio of shadowed to open irradiance, so the sky it
/// is lit by only needs the right shape, not the hour's colour: bright
/// overhead, dim from below.
fn attach_prt(
    imported: &mut ImportedScene,
    meshes: &mut [MeshData],
    path: &Path,
) -> Result<bool, Box<dyn Error>> {
    use wxsl::render::mesh::AttributeValues;
    use wxsl_prt::{Bake, Transfer};
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let file = path.with_file_name(format!("{stem}.{}.prt", Transfer::Gray.name()));
    if !file.exists() {
        return Ok(false);
    }
    let bake = Bake::read(std::io::BufReader::new(std::fs::File::open(&file)?))
        .and_then(|bake| bake.check(imported).map(|()| bake))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    for (index, mesh) in meshes.iter_mut().enumerate() {
        let streams = bake
            .streams(index)
            .ok_or_else(|| format!("{}: bake and model disagree", file.display()))?;
        for (name, values) in bake.transfer.streams().iter().zip(streams) {
            mesh.attributes
                .insert(name.to_string(), AttributeValues::Vec4(values));
        }
    }
    let (width, height) = (64, 32);
    let sky: Vec<[f32; 3]> = (0..width * height)
        .map(|texel| {
            let (u, v) = (
                ((texel % width) as f32 + 0.5) / width as f32,
                ((texel / width) as f32 + 0.5) / height as f32,
            );
            let up = wxsl_prt::sh::equirect_direction(u, v).y;
            [if up > 0.0 { 0.4 + 0.6 * up } else { 0.08 }; 3]
        })
        .collect();
    let environment = wxsl_prt::sh::project_equirect(width, height, &sky);
    let registry = crate::world::registry();
    for material in &mut imported.scene.materials {
        wxsl_prt::apply(&mut material.graph, &registry, &environment, bake.transfer)?;
    }
    println!("  PRT: {}", file.display());
    Ok(true)
}

/// Whether `imported` has glass, which the frame draws in the refraction
/// tier.
fn refractive(imported: &ImportedScene) -> bool {
    imported
        .scene
        .materials
        .iter()
        .any(|material| material.config.tags.contains(wxsl::asset::TAG_REFRACTIVE))
}

/// How much more the front wheels steer than the bend asks: a motorway
/// bend needs under a degree, which reads as no steering at all.
const STEER_EXAGGERATION: f32 = 8.0;

/// The body's pitch per m/s² of acceleration, radians: about a degree
/// and a half under hard braking.
const PITCH_PER_ACCELERATION: f32 = 0.002;

/// The wheel nodes' name prefix in me_car.
const WHEEL_PREFIX: &str = "Tire";

/// Every node under `root`, itself included.
fn subtree(scene: &wxsl::core::scene::Scene, root: usize) -> Vec<usize> {
    (0..scene.nodes.len())
        .filter(|&node| {
            let mut current = Some(node);
            while let Some(at) = current {
                if at == root {
                    return true;
                }
                current = scene.nodes[at].parent;
            }
            false
        })
        .collect()
}

/// The car's wheels: nodes named `Tire…`, with the box of everything
/// under each. The ones ahead of the wheels' middle are the front ones.
fn wheels(imported: &ImportedScene) -> Vec<Wheel> {
    let scene = &imported.scene;
    let mut wheels = Vec::new();
    for (root, node) in scene.nodes.iter().enumerate() {
        if !node.name.starts_with(WHEEL_PREFIX) {
            continue;
        }
        let under = subtree(scene, root);
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for instance in scene
            .instances
            .iter()
            .filter(|instance| instance.node.is_some_and(|node| under.contains(&node)))
        {
            let MeshSource::Embedded { index } = scene.meshes[instance.mesh].source else {
                continue;
            };
            let world = Mat4::from_cols_array(&scene.instance_world(instance));
            for vertex in &imported.meshes[index].vertices {
                let point = world.transform_point3(Vec3::from_array(vertex.position));
                min = min.min(point);
                max = max.max(point);
            }
        }
        if min.x > max.x {
            continue;
        }
        let parent = Mat4::from_cols_array(&scene.node_world(root));
        let parts = scene
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, child)| child.parent == Some(root))
            .map(|(index, child)| {
                let rolls = !child.name.starts_with("Brake") || child.name.starts_with("BrakeDisc");
                (
                    index,
                    Mat4::from_cols_array(&child.transform),
                    parent,
                    rolls,
                )
            })
            .collect();
        wheels.push(Wheel {
            center: (min + max) * 0.5,
            radius: ((max.y - min.y) * 0.5).max(0.05),
            half_width: ((max.x - min.x) * 0.5).max(0.05),
            front: false,
            nodes: under,
            parts,
        });
    }
    // Forward is -z.
    let middle =
        wheels.iter().map(|wheel| wheel.center.z).sum::<f32>() / wheels.len().max(1) as f32;
    for wheel in &mut wheels {
        wheel.front = wheel.center.z < middle;
    }
    wheels
}

/// Bind what the car makes rather than reads from the file, which
/// compiling a material again unbinds: each skinned material's bones, and
/// the paint's `flakes`.
fn bind_generated(
    resources: &mut SceneResources,
    imported: &ImportedScene,
    bones: &[Bones],
    paint: Option<(usize, bool)>,
    flakes: Option<&wgpu::TextureView>,
) -> Result<(), Box<dyn Error>> {
    for (material, skin) in imported.material_skins.iter().enumerate() {
        if let (Some(skin), Some(bindings)) = (skin, resources.bindings_mut(material)) {
            bindings.set_texture(BONE_TEXTURE_SLOT, &bones[*skin].view)?;
        }
    }
    if let (Some((material, _)), Some(flakes)) = (paint, flakes) {
        if let Some(bindings) = resources.bindings_mut(material) {
            bindings.set_texture(FLAKES_SLOT, flakes)?;
        }
    }
    Ok(())
}

impl Car {
    /// Import `path` and load it against the frame `install` builds,
    /// which is told whether the car has glass. With `shadow`, a decimated
    /// copy of the same file in the same space, that copy casts the car's
    /// shadows and the car casts none.
    pub fn open(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        path: &Path,
        shadow: Option<&Path>,
        install: impl FnOnce(&mut Renderer, bool) -> Result<(), Box<dyn Error>>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut imported = Self::import(path, &[])?;
        if shadow.is_some() {
            for material in &mut imported.scene.materials {
                material.config.cast_shadow = false;
            }
        }
        let refractive = refractive(&imported);
        install(renderer, refractive)?;
        let flakes = crate::flakes::generate(gpu, crate::flakes::SIZE);
        let mut car = Self::load(gpu, renderer, imported, path, Some(flakes))?;
        if let Some(path) = shadow {
            let imported = Self::import(path, &[crate::lights::CAR_SHADOW_TAG])?;
            let mut stand_in = Self::load(gpu, renderer, imported, path, None)?;
            // Where the car is, whatever the copy's own box.
            stand_in.align = car.align;
            car.shadow = Some(Box::new(stand_in));
        }
        Ok(car)
    }

    /// Import `path`, its lenses lit and casting no shadow, every material
    /// tagged as the car's and with `tags`.
    fn import(path: &Path, tags: &[&str]) -> Result<ImportedScene, Box<dyn Error>> {
        let mut imported = wxsl::asset::import_gltf(path)?;
        // What the headlights' static shadow draws (ADR 0101).
        for material in &mut imported.scene.materials {
            material.config.tags.insert(crate::lights::CAR_TAG);
            for tag in tags {
                material.config.tags.insert(*tag);
            }
        }
        for warning in &imported.warnings {
            println!("  {}: {warning}", path.display());
        }
        Ok(imported)
    }

    /// Load `imported`, read from `path`, onto the device, its paint
    /// flaked with `flakes` when given.
    fn load(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        mut imported: ImportedScene,
        path: &Path,
        flakes: Option<wgpu::TextureView>,
    ) -> Result<Self, Box<dyn Error>> {
        let lenses = light_lenses(&mut imported)?;
        let refractive = refractive(&imported);
        let mut meshes: Vec<MeshData> = imported
            .meshes
            .iter()
            .map(wxsl::render::gltf::mesh_data)
            .collect();
        let prt = attach_prt(&mut imported, &mut meshes, path)?;
        let windows = glass_params(&mut imported, &crate::world::registry())?;
        let paint = imported
            .scene
            .materials
            .iter()
            .position(|material| material.name == PAINT_MATERIAL)
            .map(|material| (material, false));
        if let (Some((material, _)), Some(_)) = (paint, &flakes) {
            repeat_flakes(&mut imported.scene.materials[material], FLAKES_REPEAT);
        }
        let bones: Vec<Bones> = (0..imported.skins.len())
            .map(|skin| Bones::upload(gpu, &imported, skin))
            .collect();
        let mut resources = SceneResources::load_resolved(
            &gpu.device,
            &imported.scene,
            &crate::world::registry(),
            imported.directory.as_deref(),
            renderer.lighting(),
            &[],
            &meshes,
        )?;
        resources.set_images(
            imported
                .images
                .iter()
                .map(|bytes| SceneImage::Encoded(bytes.clone()))
                .collect(),
        );
        resources.create_bindings(&gpu.device, &gpu.queue, renderer)?;
        let flakes = flakes.filter(|_| paint.is_some());
        bind_generated(&mut resources, &imported, &bones, paint, flakes.as_ref())?;
        let clip = imported
            .animations
            .iter()
            .position(|animation| animation.name == DRIVING_CLIP)
            .or((!imported.animations.is_empty()).then_some(0));
        let (min, max) = bounds(&imported, clip);
        let center = (min + max) * 0.5;
        let align = Mat4::from_translation(Vec3::new(-center.x, -min.y, -center.z));
        let to_car = |[left, right]: [Vec3; 2]| {
            [align.transform_point3(left), align.transform_point3(right)]
        };
        let head_boxes = lamp_boxes(&imported, &lenses, center);
        let headlights = head_boxes.map(|boxes| to_car(lamp_centers(boxes, true)));
        let optics = head_boxes.map(|boxes| {
            optic_middles(boxes, OPTIC_DEPTH).map(|optic| align.transform_point3(optic))
        });
        let optic_radius = head_boxes.map_or(0.0, |boxes| {
            boxes
                .iter()
                .map(|(min, max)| ((max.x - min.x) * 0.25).min((max.y - min.y) * 0.5))
                .sum::<f32>()
                * 0.5
        });
        let tails: Vec<usize> = imported
            .scene
            .materials
            .iter()
            .enumerate()
            .filter(|(_, material)| material.name == TAIL_MATERIAL)
            .map(|(index, _)| index)
            .collect();
        let tail_lights =
            lamp_boxes(&imported, &tails, center).map(|boxes| to_car(lamp_centers(boxes, false)));
        let tails = tails
            .into_iter()
            .filter_map(
                |material| match resources.bindings_mut(material)?.get(EMISSIVE_PARAM)? {
                    Value::Vec3(emission) => Some((material, emission)),
                    _ => None,
                },
            )
            .collect();
        Ok(Car {
            align,
            lenses,
            headlights,
            optics,
            optic_radius,
            tail_lights,
            tails,
            tail_glow: (0.0, 0.0),
            size: max - min,
            refractive,
            wheels: wheels(&imported),
            pitch: (0.0, 0.0),
            previous_locals: Vec::new(),
            shadow: None,
            imported,
            bones,
            clip,
            resources,
            prt,
            prt_strength: 1.0,
            paint,
            flakes,
            windows,
        })
    }

    /// Light the headlights' lenses and the tail lights for `lights_on`
    /// (0 by day, 1 at night).
    pub fn set_lights_on(
        &mut self,
        gpu: &GpuContext,
        lights_on: f32,
    ) -> Result<(), Box<dyn Error>> {
        for &material in &self.lenses {
            if let Some(bindings) = self.resources.bindings_mut(material) {
                bindings.set(LENS_GLOW, Value::Vec3([LENS_RADIANCE * lights_on; 3]))?;
            }
        }
        self.set_tail_glow(gpu, (lights_on, self.tail_glow.1))
    }

    /// Light the brake lights by `braking`, 0 to 1.
    pub fn set_braking(&mut self, gpu: &GpuContext, braking: f32) -> Result<(), Box<dyn Error>> {
        if (braking - self.tail_glow.1).abs() < 1e-3 {
            return Ok(());
        }
        self.set_tail_glow(gpu, (self.tail_glow.0, braking))
    }

    /// The tail lights' emission, as a fraction of the file's.
    fn tail_emission(&self) -> f32 {
        self.tail_glow.0 * TAIL_GLOW + self.tail_glow.1 * BRAKE_GLOW
    }

    fn set_tail_glow(&mut self, gpu: &GpuContext, glow: (f32, f32)) -> Result<(), Box<dyn Error>> {
        self.tail_glow = glow;
        let brightness = self.tail_emission();
        for &(material, emission) in &self.tails {
            if let Some(bindings) = self.resources.bindings_mut(material) {
                bindings.set(
                    EMISSIVE_PARAM,
                    Value::Vec3(emission.map(|channel| channel * brightness)),
                )?;
            }
        }
        self.resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// Scale the PRT bake's occlusion by `strength`, 0 to 1, when there is
    /// one.
    pub fn set_prt_strength(
        &mut self,
        gpu: &GpuContext,
        strength: f32,
    ) -> Result<(), Box<dyn Error>> {
        if !self.prt {
            return Ok(());
        }
        for material in 0..self.imported.scene.materials.len() {
            if let Some(bindings) = self.resources.bindings_mut(material) {
                bindings.set(wxsl_prt::STRENGTH_PARAM, Value::F32(strength))?;
            }
        }
        self.prt_strength = strength;
        self.resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// Paint the body with `paint`. The paint's parameters are set on
    /// its bindings; going to or from an iridescent paint changes the
    /// material's lighting model, so it is compiled again first, its
    /// parameters back at their defaults (the PRT strength is set again).
    pub fn set_paint(
        &mut self,
        gpu: &GpuContext,
        renderer: &mut Renderer,
        paint: &Paint,
    ) -> Result<(), Box<dyn Error>> {
        let Some((material, iridescent)) = self.paint else {
            return Ok(());
        };
        let film = paint.iridescence.is_some();
        if film != iridescent {
            let mut entry = self.imported.scene.materials[material].clone();
            if film {
                iridescent_paint(&mut entry, &crate::world::registry())?;
            }
            self.resources
                .set_material(
                    &gpu.device,
                    renderer,
                    material,
                    &entry,
                    &crate::world::registry(),
                    &renderer.lighting().clone(),
                    &[],
                )
                .map_err(|error| format!("{}: {error}", entry.name))?;
            self.paint = Some((material, film));
            bind_generated(
                &mut self.resources,
                &self.imported,
                &self.bones,
                self.paint,
                self.flakes.as_ref(),
            )?;
        }
        let Some(bindings) = self.resources.bindings_mut(material) else {
            return Ok(());
        };
        let [r, g, b] = paint.base_color;
        bindings.set("base_color_factor", Value::Vec4([r, g, b, 1.0]))?;
        bindings.set("metallic_factor", Value::F32(paint.metallic))?;
        bindings.set("roughness_factor", Value::F32(paint.roughness))?;
        bindings.set("normal_scale", Value::F32(paint.flakes))?;
        match paint.iridescence {
            Some((ior, thin, thick)) => {
                bindings.set("iridescence_factor", Value::F32(1.0))?;
                bindings.set("iridescence_ior", Value::F32(ior))?;
                bindings.set("iridescence_thickness_min", Value::F32(thin))?;
                bindings.set("iridescence_thickness", Value::F32(thick))?;
            }
            None => {
                let (coat, coat_roughness) = paint.clearcoat;
                bindings.set("clearcoat_factor", Value::F32(coat))?;
                bindings.set("clearcoat_roughness_factor", Value::F32(coat_roughness))?;
            }
        }
        if self.prt {
            bindings.set(wxsl_prt::STRENGTH_PARAM, Value::F32(self.prt_strength))?;
        }
        self.resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// Give the windows the look `glass`.
    pub fn set_glass(&mut self, gpu: &GpuContext, glass: &Glass) -> Result<(), Box<dyn Error>> {
        for &material in &self.windows {
            let Some(bindings) = self.resources.bindings_mut(material) else {
                continue;
            };
            for ((_, name), value) in GLASS_PARAMS.into_iter().zip(glass.values()) {
                bindings.set(name, value)?;
            }
        }
        self.resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// Show the surface debug view `view` (ADR 0085) on every material,
    /// or shade again for 0: each is recompiled under the view's macro, as
    /// the editor's scene mode does, and its bone texture bound again. The
    /// lenses' glow, the bone frame, the paint and the PRT strength are
    /// parameters, which go back to their defaults: set them again
    /// ([`Car::set_lights_on`], [`Car::animate`], [`Car::set_paint`],
    /// [`Car::set_glass`], [`Car::set_prt_strength`]).
    pub fn set_debug_view(
        &mut self,
        gpu: &GpuContext,
        renderer: &mut Renderer,
        view: i32,
    ) -> Result<(), Box<dyn Error>> {
        use wxsl::core::lighting::{debug_views, DEBUG_MATERIAL_MACRO, DEBUG_VIEW_MACRO};
        use wxsl::core::macros::MacroValue;
        let registry = crate::world::registry();
        let lighting = renderer.lighting().clone();
        let by_material = debug_views()
            .iter()
            .any(|known| known.id == view && known.label == "material id");
        for (index, entry) in self.imported.scene.materials.iter().enumerate() {
            let mut entry = entry.clone();
            if self.paint == Some((index, true)) {
                iridescent_paint(&mut entry, &registry)?;
            }
            if view != 0 {
                entry
                    .config
                    .macros
                    .set(DEBUG_VIEW_MACRO, MacroValue::Int(view));
                if by_material {
                    entry
                        .config
                        .macros
                        .set(DEBUG_MATERIAL_MACRO, MacroValue::Int(index as i32));
                }
            }
            self.resources
                .set_material(
                    &gpu.device,
                    renderer,
                    index,
                    &entry,
                    &registry,
                    &lighting,
                    &[],
                )
                .map_err(|error| format!("{}: {error}", entry.name))?;
        }
        bind_generated(
            &mut self.resources,
            &self.imported,
            &self.bones,
            self.paint,
            self.flakes.as_ref(),
        )?;
        Ok(())
    }

    /// Show the driving clip at `time` seconds, the wheels rolled for
    /// `distance` metres and the front ones steered into a bend of
    /// `curvature` (1/m, positive to the left), the body pitched for an
    /// `acceleration` along the road (m/s², negative braking).
    pub fn animate(
        &mut self,
        gpu: &GpuContext,
        time: f32,
        distance: f32,
        curvature: f32,
        acceleration: f32,
    ) -> Result<(), Box<dyn Error>> {
        self.pitch = (acceleration * PITCH_PER_ACCELERATION, self.pitch.0);
        let steer = (self.wheelbase() * curvature).atan() * STEER_EXAGGERATION;
        self.previous_locals = self
            .resources
            .draw_list()
            .items()
            .iter()
            .map(|item| item.transform)
            .collect();
        let scene = &mut self.imported.scene;
        for wheel in &self.wheels {
            // Forward is -z, so the top of the wheel turns towards -z: a
            // negative turn about +x. A positive turn about +y takes -z
            // towards -x, the car's left.
            let steered = Mat4::from_rotation_y(if wheel.front { steer } else { 0.0 });
            let roll = Mat4::from_rotation_x(-(distance / wheel.radius) % std::f32::consts::TAU);
            for (node, rest, parent, rolls) in &wheel.parts {
                let turn = Mat4::from_translation(wheel.center)
                    * steered
                    * if *rolls { roll } else { Mat4::IDENTITY }
                    * Mat4::from_translation(-wheel.center);
                let local = parent.inverse() * turn * *parent * *rest;
                scene.nodes[*node].transform = local.to_cols_array();
            }
        }
        if !self.wheels.is_empty() {
            self.resources.set_instances(scene)?;
        }
        for (material, skin) in self.imported.material_skins.iter().enumerate() {
            let (Some(skin), Some(bindings)) = (skin, self.resources.bindings_mut(material)) else {
                continue;
            };
            let frame = match self.clip {
                Some(clip) => self.bones[*skin].baked.frame(clip, time),
                None => 0.0,
            };
            bindings.set(BONE_FRAME_PARAM, Value::F32(frame))?;
        }
        self.resources.upload(&gpu.device, &gpu.queue)?;
        if let Some(shadow) = &mut self.shadow {
            shadow.animate(gpu, time, distance, curvature, acceleration)?;
        }
        Ok(())
    }

    /// The body's pitch by `angle` radians, nose up positive, in the
    /// car's space: about the axis across the middle of the axles.
    fn tilt(&self, angle: f32) -> Mat4 {
        let middle = self.wheels.iter().map(|wheel| wheel.center).sum::<Vec3>()
            / self.wheels.len().max(1) as f32;
        let pivot = self.align.transform_point3(middle);
        // Forward is -z: a positive turn about +x lifts it.
        Mat4::from_translation(pivot)
            * Mat4::from_rotation_x(angle)
            * Mat4::from_translation(-pivot)
    }

    /// Whether draw `draw` is part of a wheel, which stays on the road.
    fn unsprung(&self, draw: usize) -> bool {
        let Some(node) = self
            .resources
            .draw_instance(draw)
            .and_then(|instance| self.imported.scene.instances[instance].node)
        else {
            return false;
        };
        self.on_a_wheel(node)
    }

    /// From the rear axle to the front one, metres.
    fn wheelbase(&self) -> f32 {
        let axle = |front: bool| {
            let (sum, count) = self
                .wheels
                .iter()
                .filter(|wheel| wheel.front == front)
                .fold((0.0, 0), |(sum, count), wheel| {
                    (sum + wheel.center.z, count + 1)
                });
            sum / count.max(1) as f32
        };
        axle(false) - axle(true)
    }

    /// From the file's space to the car's, which a drawing of something
    /// baked in the file's space (the ground grid) goes through.
    pub fn align(&self) -> Mat4 {
        self.align
    }

    /// The car's transform on the road: in the lane `lateral` metres right
    /// of the centre line, along `frame`.
    pub fn placement(&self, frame: &Frame, lateral: f32) -> Mat4 {
        let position = frame.at(lateral, 0.0);
        // The car faces -z: its +z axis points backwards along the road.
        Mat4::from_cols(
            frame.right.extend(0.0),
            frame.up.extend(0.0),
            (-frame.forward).extend(0.0),
            position.extend(1.0),
        )
    }

    /// The car's lamps, in its own space: where the file's lamps are, or
    /// guessed from its box, pitched with the body.
    pub fn lamps(&self) -> CarLamps {
        let half = self.size * 0.5;
        let side = half.x * 0.68;
        let (front, back) = (-half.z - 0.05, half.z + 0.05);
        let tilt = self.tilt(self.pitch.0);
        let pitched = |lamps: [Vec3; 2]| lamps.map(|lamp| tilt.transform_point3(lamp));
        CarLamps {
            head: pitched(self.headlights.unwrap_or([
                Vec3::new(-side, self.size.y * 0.42, front),
                Vec3::new(side, self.size.y * 0.42, front),
            ])),
            tail: pitched(self.tail_lights.unwrap_or([
                Vec3::new(-side, self.size.y * 0.55, back),
                Vec3::new(side, self.size.y * 0.55, back),
            ])),
            optics: self
                .optics
                .map(|optics| optics.map(|optic| tilt.transform_point3(optic))),
            optic_radius: self.optic_radius,
            tail_glow: self.tail_emission() / TAIL_GLOW,
        }
    }

    /// The underglow's loop (`neon::strips`) inside the outline of the
    /// body's lower part, seen from above, wheels left out.
    pub fn neon_strips(&self) -> Vec<Emitter> {
        let scene = &self.imported.scene;
        let half = self.size * 0.5;
        let mut widths = [0.0f32; NEON_BINS];
        for instance in scene
            .instances
            .iter()
            .filter(|instance| !instance.node.is_some_and(|node| self.on_a_wheel(node)))
        {
            let MeshSource::Embedded { index } = scene.meshes[instance.mesh].source else {
                continue;
            };
            let world = self.align * Mat4::from_cols_array(&scene.instance_world(instance));
            for vertex in &self.imported.meshes[index].vertices {
                let point = world.transform_point3(Vec3::from_array(vertex.position));
                if point.y > NEON_BODY_HEIGHT {
                    continue;
                }
                let bin = ((point.z + half.z) / self.size.z * NEON_BINS as f32) as usize;
                let width = &mut widths[bin.min(NEON_BINS - 1)];
                *width = width.max(point.x.abs());
            }
        }
        // Each width the mean of its neighbours', over the mesh's details.
        let filled: Vec<(f32, f32)> = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 0.0)
            .map(|(bin, width)| {
                (
                    (bin as f32 + 0.5) / NEON_BINS as f32 * self.size.z - half.z,
                    *width,
                )
            })
            .collect();
        let outline: Vec<(f32, f32)> = (0..filled.len())
            .map(|index| {
                let near = &filled[index.saturating_sub(2)..(index + 3).min(filled.len())];
                let mean = near.iter().map(|(_, width)| width).sum::<f32>() / near.len() as f32;
                (filled[index].0, mean)
            })
            .collect();
        let height = self
            .wheels
            .iter()
            .map(|wheel| wheel.radius * NEON_HEIGHT)
            .reduce(f32::min)
            .unwrap_or(0.12);
        neon::strips(&outline, height, &self.wheel_footprints())
    }

    /// Each wheel's footprint on the road, in the car's space: its centre
    /// (x, z) and its half extents across and along the car.
    pub fn wheel_footprints(&self) -> Vec<(Vec2, Vec2)> {
        self.wheels
            .iter()
            .map(|wheel| {
                let at = self.align.transform_point3(wheel.center);
                (
                    Vec2::new(at.x, at.z),
                    Vec2::new(wheel.half_width, wheel.radius),
                )
            })
            .collect()
    }

    /// Whether node `node` is part of a wheel.
    fn on_a_wheel(&self, node: usize) -> bool {
        self.wheels.iter().any(|wheel| wheel.nodes.contains(&node))
    }

    /// Its draws at `transform`, last frame at `previous`, the body
    /// pitched, its stand-in's after them.
    pub fn draws(&self, transform: Mat4, previous: Mat4) -> Vec<DrawItem<'_>> {
        let (wheels, then_wheels) = (transform * self.align, previous * self.align);
        let body = transform * self.tilt(self.pitch.0) * self.align;
        let then_body = previous * self.tilt(self.pitch.1) * self.align;
        let mut draws: Vec<DrawItem<'_>> = self
            .resources
            .draw_list()
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let mut item = *item;
                let (now, then) = if self.unsprung(index) {
                    (wheels, then_wheels)
                } else {
                    (body, then_body)
                };
                let local = item.transform;
                let before = self.previous_locals.get(index).copied().unwrap_or(local);
                item.transform = now * local;
                item.previous = Some(then * before);
                item.bounds = item.bounds.map(|(min, max)| transform_box(now, min, max));
                item
            })
            .collect();
        if let Some(shadow) = &self.shadow {
            draws.extend(shadow.draws(transform, previous));
        }
        draws
    }
}

/// The world box around box `min..max` moved by `transform`.
pub fn transform_box(transform: Mat4, min: Vec3, max: Vec3) -> (Vec3, Vec3) {
    let (mut low, mut high) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for corner in 0..8 {
        let point = Vec3::new(
            if corner & 1 == 0 { min.x } else { max.x },
            if corner & 2 == 0 { min.y } else { max.y },
            if corner & 4 == 0 { min.z } else { max.z },
        );
        let moved = transform.transform_point3(point);
        low = low.min(moved);
        high = high.max(moved);
    }
    (low, high)
}

/// The box around every instance's vertices, a skinned one's where the
/// start of `clip` puts its joints.
fn bounds(imported: &ImportedScene, clip: Option<usize>) -> (Vec3, Vec3) {
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut posed = imported.scene.clone();
    if let Some(animation) = clip.and_then(|clip| imported.animations.get(clip)) {
        animation.pose(0.0, &imported.scene.nodes, &mut posed.nodes);
    }
    let scene = &imported.scene;
    for instance in &scene.instances {
        let MeshSource::Embedded { index } = scene.meshes[instance.mesh].source else {
            continue;
        };
        let geometry = &imported.meshes[index];
        let world = Mat4::from_cols_array(&scene.instance_world(instance));
        let joints: Vec<Mat4> = match imported.material_skins[instance.material] {
            Some(skin) => {
                let skin = &imported.skins[skin];
                skin.joint_matrices(&posed, &scene.node_world(skin.node))
                    .iter()
                    .map(Mat4::from_cols_array)
                    .collect()
            }
            None => Vec::new(),
        };
        for (at, vertex) in geometry.vertices.iter().enumerate() {
            let mut point = Vec3::from_array(vertex.position);
            if let (false, Some(indices), Some(weights)) =
                (joints.is_empty(), &geometry.joints, &geometry.weights)
            {
                point = indices[at]
                    .iter()
                    .zip(weights[at])
                    .filter_map(|(&joint, weight)| {
                        Some(joints.get(joint as usize)?.transform_point3(point) * weight)
                    })
                    .sum();
            }
            let point = world.transform_point3(point);
            min = min.min(point);
            max = max.max(point);
        }
    }
    if min.x > max.x {
        return (Vec3::splat(-1.0), Vec3::splat(1.0));
    }
    (min, max)
}
