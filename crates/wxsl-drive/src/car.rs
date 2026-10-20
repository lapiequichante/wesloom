//! me_car: the glTF scene (ADR 0080), its driver's skin playing the
//! `driving` clip from a bone texture (ADR 0099), set down on its wheels
//! and moved along the road by one transform per frame. The wheels roll:
//! their nodes turn about their axles and the instances are re-read, and
//! each draw remembers last frame's transform so the velocity pass sees
//! the spin. When the file has a grayscale PRT bake beside it
//! (`me_car.gray.prt`, ADR 0092), its materials read it: the sky's light
//! is shadowed by the car itself. A decimated copy of the file, when there
//! is one, casts the car's shadows in its place, posed and animated with it.

use std::error::Error;
use std::path::Path;

use glam::{Mat4, Vec3};
use wxsl::asset::{BoneTexture, ImportedScene, BONE_FRAME_PARAM, BONE_TEXTURE_SLOT};
use wxsl::core::scene::MeshSource;
use wxsl::core::Value;
use wxsl::render::mesh::MeshData;
use wxsl::render::scene::SceneImage;
use wxsl::render::{DrawItem, GpuContext, Renderer};
use wxsl::scene::SceneResources;

use crate::lights::CarLamps;
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
fn f16_bits(value: f32) -> u16 {
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
    /// The tail lights' centres, the same way.
    tail_lights: Option<[Vec3; 2]>,
    /// Each draw's transform in the file's space last frame, in
    /// [`SceneResources::draw_list`] order.
    previous_locals: Vec<Mat4>,
    /// The decimated stand-in the shadow passes draw instead of the car,
    /// tagged [`crate::lights::CAR_SHADOW_TAG`].
    shadow: Option<Box<Car>>,
}

/// A wheel: where its axle is, its radius, and the nodes that turn with
/// it (the rim, the tyre and the disc; not the calliper).
struct Wheel {
    center: Vec3,
    radius: f32,
    /// (node, its rest transform, its parent's world transform).
    parts: Vec<(usize, Mat4, Mat4)>,
}

/// The materials of me_car's headlights, which the file leaves unlit.
const LENS_MATERIALS: &[&str] = &["Headlight", "hl inside", "ll shell"];
/// The parameter their glow is, and its radiance at night.
const LENS_GLOW: &str = "headlight_glow";
const LENS_RADIANCE: f32 = 30.0;

/// Give the headlights' materials an emission the night can turn up: a
/// `headlight_glow` parameter into their surface's `emissive`, which the
/// file leaves unwired. They cast no shadow, so the beam from the bulb
/// inside them leaves through the glass. Their indices.
fn light_lenses(imported: &mut ImportedScene) -> Result<Vec<usize>, Box<dyn Error>> {
    use wxsl::core::graph::{Node, SocketRef};
    let registry = wxsl::stdlib::registry();
    let mut lenses = Vec::new();
    for (index, material) in imported.scene.materials.iter_mut().enumerate() {
        if !LENS_MATERIALS.contains(&material.name.as_str()) {
            continue;
        }
        material.config.cast_shadow = false;
        let graph = &mut material.graph;
        let Some(output) = graph
            .nodes()
            .find(|(_, node)| node.def == wxsl::core::abi::SURFACE_OUTPUT_ID)
            .map(|(id, _)| id)
        else {
            continue;
        };
        if graph
            .edge_into(&SocketRef::new(output, "emissive"))
            .is_some()
        {
            continue;
        }
        let glow = graph.add(
            Node::new("param.value")
                .with_setting("name", LENS_GLOW)
                .with_param("value", Value::Vec3([0.0; 3])),
        );
        graph
            .wire(&registry, (glow, "out"), (output, "emissive"))
            .map_err(|error| format!("{}: {error}", material.name))?;
        lenses.push(index);
    }
    Ok(lenses)
}

/// Where each lamp of `materials`' light leaves it, left then right, in
/// the file's space: the middle of their box across. A headlight's is its
/// bulb, just behind its housing (`+z`) and high in it
/// ([`BULB_HEIGHT`]), whose glass casts no shadow ([`light_lenses`]); a
/// tail light's is just past its back face, halfway up, so the housing
/// never shadows its own beam.
fn lamp_centers(
    imported: &ImportedScene,
    materials: &[usize],
    center: Vec3,
    front: bool,
) -> Option<[Vec3; 2]> {
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
    (sides[0].0.x <= sides[0].1.x && sides[1].0.x <= sides[1].1.x).then(|| {
        sides.map(|(min, max)| {
            let middle = (min + max) * 0.5;
            let (y, z) = if front {
                (min.y + (max.y - min.y) * BULB_HEIGHT, max.z + BULB_SETBACK)
            } else {
                (middle.y, max.z + LAMP_CLEARANCE)
            };
            Vec3::new(middle.x, y, z)
        })
    })
}

/// How far past the tail lights' lenses their light starts.
const LAMP_CLEARANCE: f32 = 0.03;
/// How far behind the back of a headlight's housing its bulb is: the
/// housing casts no shadow, so its whole depth is lit from behind.
pub const BULB_SETBACK: f32 = 0.08;
/// How high in a headlight's housing its bulb is, from its bottom (0) to
/// its top (1): high enough that the beam, aimed a touch down, lights the
/// whole inside of the housing, not only its lower half.
const BULB_HEIGHT: f32 = 0.75;
/// The material of me_car's tail lights.
const TAIL_MATERIAL: &str = "RearLights";

/// Wire the grayscale PRT bake beside `path`, if there is one, into
/// `imported`'s materials and its streams onto `meshes`.
///
/// The transfer is a ratio of shadowed to open irradiance, so the sky it
/// is lit by only needs the right shape, not the hour's colour: bright
/// overhead, dim from below.
fn attach_prt(
    imported: &mut ImportedScene,
    meshes: &mut [MeshData],
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    use wxsl::render::mesh::AttributeValues;
    use wxsl_prt::{Bake, Transfer};
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let file = path.with_file_name(format!("{stem}.{}.prt", Transfer::Gray.name()));
    if !file.exists() {
        return Ok(());
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
    let registry = wxsl::stdlib::registry();
    for material in &mut imported.scene.materials {
        wxsl_prt::apply(&mut material.graph, &registry, &environment, bake.transfer)?;
    }
    println!("  PRT: {}", file.display());
    Ok(())
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
/// under each.
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
            .filter(|(_, child)| {
                !child.name.starts_with("Brake") || child.name.starts_with("BrakeDisc")
            })
            .map(|(index, child)| (index, Mat4::from_cols_array(&child.transform), parent))
            .collect();
        wheels.push(Wheel {
            center: (min + max) * 0.5,
            radius: ((max.y - min.y) * 0.5).max(0.05),
            parts,
        });
    }
    wheels
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
        let mut car = Self::load(gpu, renderer, imported, path)?;
        if let Some(path) = shadow {
            let imported = Self::import(path, &[crate::lights::CAR_SHADOW_TAG])?;
            let mut stand_in = Self::load(gpu, renderer, imported, path)?;
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

    /// Load `imported`, read from `path`, onto the device.
    fn load(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        mut imported: ImportedScene,
        path: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        let lenses = light_lenses(&mut imported)?;
        let refractive = refractive(&imported);
        let mut meshes: Vec<MeshData> = imported
            .meshes
            .iter()
            .map(wxsl::render::gltf::mesh_data)
            .collect();
        attach_prt(&mut imported, &mut meshes, path)?;
        let bones: Vec<Bones> = (0..imported.skins.len())
            .map(|skin| Bones::upload(gpu, &imported, skin))
            .collect();
        let mut resources = SceneResources::load_resolved(
            &gpu.device,
            &imported.scene,
            &wxsl::stdlib::registry(),
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
        for (material, skin) in imported.material_skins.iter().enumerate() {
            if let (Some(skin), Some(bindings)) = (skin, resources.bindings_mut(material)) {
                bindings.set_texture(BONE_TEXTURE_SLOT, &bones[*skin].view)?;
            }
        }
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
        let headlights = lamp_centers(&imported, &lenses, center, true).map(to_car);
        let tails: Vec<usize> = imported
            .scene
            .materials
            .iter()
            .enumerate()
            .filter(|(_, material)| material.name == TAIL_MATERIAL)
            .map(|(index, _)| index)
            .collect();
        let tail_lights = lamp_centers(&imported, &tails, center, false).map(to_car);
        Ok(Car {
            align,
            lenses,
            headlights,
            tail_lights,
            size: max - min,
            refractive,
            wheels: wheels(&imported),
            previous_locals: Vec::new(),
            shadow: None,
            imported,
            bones,
            clip,
            resources,
        })
    }

    /// Light the headlights' lenses for `lights_on` (0 by day, 1 at night).
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
        self.resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// Show the surface debug view `view` (ADR 0085) on every material,
    /// or shade again for 0: each is recompiled under the view's macro, as
    /// the editor's scene mode does, and its bone texture bound again. The
    /// lenses' glow and the bone frame are parameters, which go back to
    /// their defaults: set them again ([`Car::set_lights_on`],
    /// [`Car::animate`]).
    pub fn set_debug_view(
        &mut self,
        gpu: &GpuContext,
        renderer: &mut Renderer,
        view: i32,
    ) -> Result<(), Box<dyn Error>> {
        use wxsl::core::lighting::{debug_views, DEBUG_MATERIAL_MACRO, DEBUG_VIEW_MACRO};
        use wxsl::core::macros::MacroValue;
        let registry = wxsl::stdlib::registry();
        let lighting = renderer.lighting().clone();
        let by_material = debug_views()
            .iter()
            .any(|known| known.id == view && known.label == "material id");
        for (index, entry) in self.imported.scene.materials.iter().enumerate() {
            let mut entry = entry.clone();
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
        for (material, skin) in self.imported.material_skins.iter().enumerate() {
            if let (Some(skin), Some(bindings)) = (skin, self.resources.bindings_mut(material)) {
                bindings.set_texture(BONE_TEXTURE_SLOT, &self.bones[*skin].view)?;
            }
        }
        Ok(())
    }

    /// Show the driving clip at `time` seconds, the wheels rolled for
    /// `distance` metres.
    pub fn animate(
        &mut self,
        gpu: &GpuContext,
        time: f32,
        distance: f32,
    ) -> Result<(), Box<dyn Error>> {
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
            // negative turn about +x.
            let turn = Mat4::from_translation(wheel.center)
                * Mat4::from_rotation_x(-(distance / wheel.radius) % std::f32::consts::TAU)
                * Mat4::from_translation(-wheel.center);
            for (node, rest, parent) in &wheel.parts {
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
            shadow.animate(gpu, time, distance)?;
        }
        Ok(())
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
    /// guessed from its box.
    pub fn lamps(&self) -> CarLamps {
        let half = self.size * 0.5;
        let side = half.x * 0.68;
        let (front, back) = (-half.z - 0.05, half.z + 0.05);
        CarLamps {
            head: self.headlights.unwrap_or([
                Vec3::new(-side, self.size.y * 0.42, front),
                Vec3::new(side, self.size.y * 0.42, front),
            ]),
            tail: self.tail_lights.unwrap_or([
                Vec3::new(-side, self.size.y * 0.55, back),
                Vec3::new(side, self.size.y * 0.55, back),
            ]),
        }
    }

    /// Its draws at `transform`, last frame at `previous`, its stand-in's
    /// after them.
    pub fn draws(&self, transform: Mat4, previous: Mat4) -> Vec<DrawItem<'_>> {
        let (now, then) = (transform * self.align, previous * self.align);
        let mut draws: Vec<DrawItem<'_>> = self
            .resources
            .draw_list()
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let mut item = *item;
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
