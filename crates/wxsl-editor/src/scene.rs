//! The scene mode's document, and the half of its edits that needs no
//! device (ADR 0082).
//!
//! A [`SceneDoc`] is the scene document plus what its `embedded` sources
//! index. The geometry and images sit behind `Arc`, so a history snapshot
//! copies only the document. [`edit_scene`] applies the scene commands;
//! uploading and compiling is the editor's, at the next frame, guided by
//! the [`SceneChanges`] an edit reports. Picking is here too: rays against
//! bounds, then triangles, on the CPU.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::{Mat4, Vec3};
use wxsl_core::node::NodeRegistry;
use wxsl_core::scene::{MeshSource, Scene, Tags, TextureSource};
use wxsl_render::{Camera, MeshData};

use crate::command::{Command, CommandError};

/// The open scene.
#[derive(Clone, Debug)]
pub(crate) struct SceneDoc {
    /// The document.
    pub document: Scene,
    /// What `file` paths are relative to: the document's directory.
    pub base: Option<PathBuf>,
    /// What `embedded` mesh sources index.
    pub meshes: Arc<Vec<MeshData>>,
    /// What `embedded` texture sources index, encoded.
    pub images: Arc<Vec<Vec<u8>>>,
}

impl SceneDoc {
    /// A document with nothing embedded.
    pub fn new(document: Scene, base: Option<PathBuf>) -> Self {
        SceneDoc {
            document,
            base,
            meshes: Arc::default(),
            images: Arc::default(),
        }
    }

    /// Read a `.scene.json`, or import a `.gltf`/`.glb`.
    pub fn open(path: &Path, registry: &NodeRegistry) -> Result<Self, CommandError> {
        let failed = |reason: String| CommandError::Scene(format!("{}: {reason}", path.display()));
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if extension == "gltf" || extension == "glb" {
            return import(path, registry);
        }
        let text = std::fs::read_to_string(path).map_err(|error| failed(error.to_string()))?;
        let document: Scene =
            serde_json::from_str(&text).map_err(|error| failed(error.to_string()))?;
        let invalid = document.validate();
        if let Some(error) = invalid.first() {
            return Err(failed(error.to_string()));
        }
        Ok(SceneDoc::new(
            document,
            path.parent().map(Path::to_path_buf),
        ))
    }

    /// Write the document as JSON. Refused while anything is embedded,
    /// which only the import that supplied it could resolve.
    pub fn save(&self, path: &Path) -> Result<(), CommandError> {
        let embedded_mesh = self
            .document
            .meshes
            .iter()
            .any(|mesh| matches!(mesh.source, MeshSource::Embedded { .. }));
        let embedded_texture = self
            .document
            .textures
            .iter()
            .any(|texture| matches!(texture.source, TextureSource::Embedded { .. }));
        if embedded_mesh || embedded_texture {
            return Err(CommandError::Scene(
                "this scene holds imported meshes or images, which a .scene.json cannot \
                 carry yet; save the material graphs instead"
                    .to_string(),
            ));
        }
        let mut document = self.document.clone();
        if let (Some(base), Some(target)) = (self.base.as_deref(), path.parent()) {
            if base != target {
                rebase_files(&mut document, base);
            }
        }
        let text = serde_json::to_string_pretty(&document)
            .map_err(|error| CommandError::Scene(error.to_string()))?;
        std::fs::write(path, text)
            .map_err(|error| CommandError::Scene(format!("{}: {error}", path.display())))
    }

    /// Add `other` to this scene: its meshes, images, textures, materials,
    /// nodes, instances, lights and cameras, with every index shifted past
    /// what is already here.
    pub fn append(&mut self, other: SceneDoc) {
        let SceneDoc {
            document: mut added,
            base,
            meshes,
            images,
        } = other;
        if base != self.base {
            if let Some(base) = base.as_deref() {
                rebase_files(&mut added, base);
            }
        }
        let scene = &mut self.document;
        let (mesh_offset, material_offset, texture_offset, node_offset) = (
            scene.meshes.len(),
            scene.materials.len(),
            scene.textures.len(),
            scene.nodes.len(),
        );
        let (supplied_offset, image_offset) = (self.meshes.len(), self.images.len());
        for mut mesh in added.meshes {
            if let MeshSource::Embedded { index } = &mut mesh.source {
                *index += supplied_offset;
            }
            scene.meshes.push(mesh);
        }
        for mut texture in added.textures {
            if let TextureSource::Embedded { index } = &mut texture.source {
                *index += image_offset;
            }
            scene.textures.push(texture);
        }
        for mut material in added.materials {
            for texture in material.bindings.textures.values_mut() {
                *texture += texture_offset;
            }
            scene.materials.push(material);
        }
        for mut node in added.nodes {
            if let Some(parent) = &mut node.parent {
                *parent += node_offset;
            }
            scene.nodes.push(node);
        }
        for mut instance in added.instances {
            instance.mesh += mesh_offset;
            instance.material += material_offset;
            if let Some(node) = &mut instance.node {
                *node += node_offset;
            }
            scene.instances.push(instance);
        }
        if let Some(mut environment) = added.environment {
            for light in &mut environment.lights {
                if let Some(node) = &mut light.node {
                    *node += node_offset;
                }
            }
            for camera in &mut environment.cameras {
                if let Some(node) = &mut camera.node {
                    *node += node_offset;
                }
            }
            let block = scene.environment.get_or_insert_with(Default::default);
            block.lights.extend(environment.lights);
            block.cameras.extend(environment.cameras);
        }
        if !meshes.is_empty() {
            Arc::make_mut(&mut self.meshes).extend(meshes.iter().cloned());
        }
        if !images.is_empty() {
            Arc::make_mut(&mut self.images).extend(images.iter().cloned());
        }
    }
}

/// Make every relative `file` path absolute against `base`.
fn rebase_files(scene: &mut Scene, base: &Path) {
    let absolute = |path: &mut String| {
        if Path::new(path.as_str()).is_relative() {
            *path = base.join(path.as_str()).display().to_string();
        }
    };
    for mesh in &mut scene.meshes {
        if let MeshSource::File { path, .. } = &mut mesh.source {
            absolute(path);
        }
    }
    for texture in &mut scene.textures {
        if let TextureSource::File { path } = &mut texture.source {
            absolute(path);
        }
    }
}

#[cfg(feature = "gltf")]
fn import(path: &Path, registry: &NodeRegistry) -> Result<SceneDoc, CommandError> {
    let imported = wxsl_gltf::import(path, registry)
        .map_err(|error| CommandError::Scene(format!("{}: {error}", path.display())))?;
    Ok(SceneDoc {
        meshes: Arc::new(
            imported
                .meshes
                .iter()
                .map(wxsl_render::gltf::mesh_data)
                .collect(),
        ),
        images: Arc::new(imported.images),
        base: imported.directory,
        document: imported.scene,
    })
}

#[cfg(not(feature = "gltf"))]
fn import(path: &Path, _registry: &NodeRegistry) -> Result<SceneDoc, CommandError> {
    Err(CommandError::Scene(format!(
        "{}: this editor was built without the `gltf` feature",
        path.display()
    )))
}

/// What a scene edit changed, so the next frame redoes only that.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SceneChanges {
    /// Meshes, materials or instances were added or removed, or the scene
    /// was replaced: load it again.
    pub reload: bool,
    /// Transforms, mesh or material assignments: re-read the instances.
    pub instances: bool,
    /// These materials' configurations or graphs: recompile them.
    pub materials: BTreeSet<usize>,
}

impl SceneChanges {
    /// Whether anything changed.
    pub fn is_empty(&self) -> bool {
        !self.reload && !self.instances && self.materials.is_empty()
    }

    /// Fold `other` in.
    pub fn merge(&mut self, other: SceneChanges) {
        self.reload |= other.reload;
        self.instances |= other.instances;
        self.materials.extend(other.materials);
    }

    /// What it takes to go from `old` to `new`.
    pub fn between(old: Option<&SceneDoc>, new: Option<&SceneDoc>) -> Self {
        let (Some(old), Some(new)) = (old, new) else {
            return SceneChanges {
                reload: old.is_some() || new.is_some(),
                ..SceneChanges::default()
            };
        };
        let (a, b) = (&old.document, &new.document);
        let same_shape = a.meshes == b.meshes
            && a.textures == b.textures
            && a.materials.len() == b.materials.len()
            && a.instances.len() == b.instances.len()
            && Arc::ptr_eq(&old.meshes, &new.meshes)
            && Arc::ptr_eq(&old.images, &new.images)
            && old.base == new.base;
        if !same_shape {
            return SceneChanges {
                reload: true,
                ..SceneChanges::default()
            };
        }
        SceneChanges {
            reload: false,
            instances: a.instances != b.instances || a.nodes != b.nodes,
            materials: a
                .materials
                .iter()
                .zip(&b.materials)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(index, _)| index)
                .collect(),
        }
    }
}

/// Whether `command` edits the scene, so the scene is kept before it runs.
pub(crate) fn mentions_scene(command: &Command) -> bool {
    match command {
        Command::OpenScene { .. }
        | Command::ImportGltf { .. }
        | Command::CloseScene
        | Command::SetTransform { .. }
        | Command::SetInstanceMaterial { .. }
        | Command::SetInstanceName { .. }
        | Command::SetMaterialTags { .. }
        | Command::SetRenderOrder { .. }
        | Command::SetMaxLayers { .. }
        | Command::SetLight { .. }
        | Command::RemoveInstances { .. } => true,
        Command::Batch { commands } => commands.iter().any(mentions_scene),
        _ => false,
    }
}

/// Apply a scene command to `scene`. On error the scene may be half
/// changed; the caller restores it.
pub(crate) fn edit_scene(
    scene: &mut Option<SceneDoc>,
    registry: &NodeRegistry,
    command: &Command,
) -> Result<SceneChanges, CommandError> {
    let reload = SceneChanges {
        reload: true,
        ..SceneChanges::default()
    };
    let instances = SceneChanges {
        instances: true,
        ..SceneChanges::default()
    };
    match command {
        Command::OpenScene { path } => {
            *scene = Some(SceneDoc::open(Path::new(path), registry)?);
            return Ok(reload);
        }
        Command::ImportGltf { path } => {
            let imported = import(Path::new(path), registry)?;
            match scene.as_mut() {
                Some(open) => open.append(imported),
                None => *scene = Some(imported),
            }
            return Ok(reload);
        }
        Command::CloseScene => {
            *scene = None;
            return Ok(reload);
        }
        _ => {}
    }
    let document = &mut scene.as_mut().ok_or(CommandError::NoScene)?.document;
    let instance_count = document.instances.len();
    let material_count = document.materials.len();
    let instance = |index: usize| {
        if index < instance_count {
            Ok(index)
        } else {
            Err(CommandError::NoSuchInstance(index))
        }
    };
    let material = |index: usize| {
        if index < material_count {
            Ok(index)
        } else {
            Err(CommandError::NoSuchMaterial(index))
        }
    };
    Ok(match command {
        Command::SetTransform {
            instance: index,
            transform,
        } => {
            document.instances[instance(*index)?].transform = *transform;
            instances
        }
        Command::SetInstanceMaterial {
            instance: index,
            material: assigned,
        } => {
            document.instances[instance(*index)?].material = material(*assigned)?;
            instances
        }
        Command::SetInstanceName {
            instance: index,
            name,
        } => {
            document.instances[instance(*index)?].name = name.clone();
            SceneChanges::default()
        }
        Command::SetMaterialTags {
            material: index,
            tags,
        } => {
            let index = material(*index)?;
            document.materials[index].config.tags = tags.iter().cloned().collect::<Tags>();
            SceneChanges {
                instances: true,
                materials: BTreeSet::from([index]),
                ..SceneChanges::default()
            }
        }
        Command::SetRenderOrder {
            material: index,
            order,
        } => {
            let index = material(*index)?;
            document.materials[index].config.render_order = *order;
            SceneChanges {
                materials: BTreeSet::from([index]),
                ..SceneChanges::default()
            }
        }
        Command::SetMaxLayers {
            material: index,
            layers,
        } => {
            let index = material(*index)?;
            document.materials[index].config.max_layers = *layers;
            SceneChanges {
                materials: BTreeSet::from([index]),
                ..SceneChanges::default()
            }
        }
        Command::SetLight {
            light,
            color,
            intensity,
        } => {
            let entry = document
                .environment
                .as_mut()
                .and_then(|block| block.lights.get_mut(*light))
                .ok_or(CommandError::NoSuchLight(*light))?;
            entry.color = *color;
            entry.intensity = *intensity;
            // The environment is rebuilt from the document every frame.
            SceneChanges::default()
        }
        Command::RemoveInstances { instances: removed } => {
            let mut removed: Vec<usize> = removed
                .iter()
                .map(|&index| instance(index))
                .collect::<Result<_, _>>()?;
            removed.sort_unstable();
            removed.dedup();
            for index in removed.into_iter().rev() {
                document.instances.remove(index);
            }
            reload
        }
        _ => unreachable!("not a scene command: the editor applies it"),
    })
}

/// A box, in whatever space its corners are in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    /// The box around `points`, or `None` for none.
    pub fn around(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        points.into_iter().fold(None, |bounds, point| {
            Some(match bounds {
                None => Bounds {
                    min: point,
                    max: point,
                },
                Some(Bounds { min, max }) => Bounds {
                    min: min.min(point),
                    max: max.max(point),
                },
            })
        })
    }

    /// A mesh's bounds in its own space.
    pub fn of(mesh: &MeshData) -> Option<Self> {
        Bounds::around(mesh.vertices.iter().map(|v| Vec3::from_array(v.position)))
    }

    /// The box around this one moved by `transform`.
    pub fn transformed(&self, transform: Mat4) -> Self {
        let corners = (0..8).map(|corner| {
            transform.transform_point3(Vec3::new(
                if corner & 1 == 0 {
                    self.min.x
                } else {
                    self.max.x
                },
                if corner & 2 == 0 {
                    self.min.y
                } else {
                    self.max.y
                },
                if corner & 4 == 0 {
                    self.min.z
                } else {
                    self.max.z
                },
            ))
        });
        Bounds::around(corners).expect("eight corners")
    }

    /// Where a ray enters the box, as a distance along it, if it does.
    pub fn hit(&self, origin: Vec3, direction: Vec3) -> Option<f32> {
        let inverse = direction.recip();
        let a = (self.min - origin) * inverse;
        let b = (self.max - origin) * inverse;
        let near = a.min(b).max_element();
        let far = a.max(b).min_element();
        (far >= near.max(0.0)).then_some(near.max(0.0))
    }

    /// The union of two boxes.
    pub fn union(&self, other: &Bounds) -> Bounds {
        Bounds {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

/// The ray through a viewport point given in normalized device
/// coordinates (`x` right, `y` up, both in -1..1).
pub(crate) fn ray(camera: &Camera, ndc: glam::Vec2) -> (Vec3, Vec3) {
    let forward = (camera.target - camera.eye).normalize_or(Vec3::NEG_Z);
    let right = forward.cross(camera.up).normalize_or(Vec3::X);
    let up = right.cross(forward);
    let half = (camera.fov_y * 0.5).tan();
    let direction =
        (forward + right * ndc.x * half * camera.aspect + up * ndc.y * half).normalize();
    (camera.eye, direction)
}

/// The nearest instance a ray hits, and how far along it. `meshes[i]` is
/// the geometry of `scene.meshes[i]`, `bounds[i]` its bounds.
pub(crate) fn pick(
    scene: &Scene,
    meshes: &[MeshData],
    bounds: &[Option<Bounds>],
    origin: Vec3,
    direction: Vec3,
) -> Option<(usize, f32)> {
    let mut candidates: Vec<(usize, f32, Mat4)> = scene
        .instances
        .iter()
        .enumerate()
        .filter_map(|(index, instance)| {
            let world = Mat4::from_cols_array(&scene.instance_world(instance));
            let entry = bounds.get(instance.mesh).copied().flatten()?;
            let distance = entry.transformed(world).hit(origin, direction)?;
            Some((index, distance, world))
        })
        .collect();
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut best: Option<(usize, f32)> = None;
    for (index, entry, world) in candidates {
        if best.is_some_and(|(_, distance)| distance < entry) {
            break;
        }
        let mesh = &meshes[scene.instances[index].mesh];
        // Into the mesh's space, where the triangles are; a distance along
        // the local ray is converted back by the length of the step.
        let inverse = world.inverse();
        let local_origin = inverse.transform_point3(origin);
        let local_direction = inverse.transform_vector3(direction);
        let scale = local_direction.length();
        if scale <= f32::EPSILON {
            continue;
        }
        let local_direction = local_direction / scale;
        let position = |vertex: u32| Vec3::from_array(mesh.vertices[vertex as usize].position);
        for triangle in mesh.indices.chunks_exact(3) {
            let hit = triangle_hit(
                local_origin,
                local_direction,
                [
                    position(triangle[0]),
                    position(triangle[1]),
                    position(triangle[2]),
                ],
            );
            if let Some(local) = hit {
                let distance = local / scale;
                if best.is_none_or(|(_, nearest)| distance < nearest) {
                    best = Some((index, distance));
                }
            }
        }
    }
    best
}

/// Möller–Trumbore, both faces.
fn triangle_hit(origin: Vec3, direction: Vec3, [a, b, c]: [Vec3; 3]) -> Option<f32> {
    let (ab, ac) = (b - a, c - a);
    let p = direction.cross(ac);
    let determinant = ab.dot(p);
    if determinant.abs() < 1e-9 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let t = origin - a;
    let u = t.dot(p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = t.cross(ab);
    let v = direction.dot(q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = ac.dot(q) * inverse;
    (distance > 0.0).then_some(distance)
}

/// The viewport camera: it orbits a target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Orbit {
    pub target: Vec3,
    pub distance: f32,
    /// Around the vertical axis, radians.
    pub yaw: f32,
    /// Above the horizon, radians.
    pub pitch: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        Orbit {
            target: Vec3::ZERO,
            distance: 5.0,
            yaw: 0.65,
            pitch: 0.4,
        }
    }
}

impl Orbit {
    /// Framed on `bounds`, at the default angles.
    pub fn framing(bounds: Bounds) -> Self {
        let radius = ((bounds.max - bounds.min).length() * 0.5).max(0.01);
        Orbit {
            target: (bounds.min + bounds.max) * 0.5,
            distance: radius * 2.6,
            ..Orbit::default()
        }
    }

    /// Turn by a drag of `delta` pixels.
    pub fn turn(&mut self, delta: glam::Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.5, 1.5);
    }

    /// Move closer by `steps` scroll steps, or away for negative ones.
    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * (-steps * 0.1).exp()).max(0.01);
    }

    /// The camera, for a viewport of `aspect`.
    pub fn camera(&self, aspect: f32) -> Camera {
        let offset = Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        ) * self.distance;
        Camera {
            eye: self.target + offset,
            target: self.target,
            aspect,
            near: (self.distance * 0.01).max(0.001),
            far: (self.distance * 20.0).max(100.0),
            ..Camera::default()
        }
    }
}

/// Every instance's world bounds together.
pub(crate) fn scene_bounds(scene: &Scene, bounds: &[Option<Bounds>]) -> Option<Bounds> {
    scene
        .instances
        .iter()
        .filter_map(|instance| {
            let world = Mat4::from_cols_array(&scene.instance_world(instance));
            Some(
                bounds
                    .get(instance.mesh)
                    .copied()
                    .flatten()?
                    .transformed(world),
            )
        })
        .reduce(|a, b| a.union(&b))
}

/// A transform as translation, rotation in degrees (XYZ Euler) and scale,
/// the way the inspector shows it.
pub(crate) fn decompose(transform: &[f32; 16]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let (scale, rotation, translation) =
        Mat4::from_cols_array(transform).to_scale_rotation_translation();
    let (x, y, z) = rotation.to_euler(glam::EulerRot::XYZ);
    (
        translation.to_array(),
        [x.to_degrees(), y.to_degrees(), z.to_degrees()],
        scale.to_array(),
    )
}

/// The inverse of [`decompose`].
pub(crate) fn compose(translation: [f32; 3], degrees: [f32; 3], scale: [f32; 3]) -> [f32; 16] {
    let rotation = glam::Quat::from_euler(
        glam::EulerRot::XYZ,
        degrees[0].to_radians(),
        degrees[1].to_radians(),
        degrees[2].to_radians(),
    );
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(scale),
        rotation,
        Vec3::from_array(translation),
    )
    .to_cols_array()
}

#[cfg(test)]
mod tests {
    use wxsl_core::scene::{Instance, MaterialEntry, MeshEntry, SceneNode};
    use wxsl_core::Graph;

    use super::*;

    fn cubes() -> (Scene, Vec<MeshData>) {
        let mut scene = Scene::new("cubes");
        scene.add_mesh(MeshEntry::new("cube", MeshSource::Cube { size: 1.0 }));
        scene.add_material(MaterialEntry::new("plain", Graph::new("plain")));
        let at = |x: f32| Mat4::from_translation(Vec3::new(x, 0.0, 0.0)).to_cols_array();
        scene.add_instance(Instance::new(0, 0).with_transform(at(0.0)));
        scene.add_instance(Instance::new(0, 0).with_transform(at(3.0)));
        let (vertices, indices) = wxsl_render::mesh::cube_geometry(1.0);
        (scene, vec![MeshData::new(vertices, indices)])
    }

    #[test]
    fn a_ray_picks_the_nearest_instance_it_actually_hits() {
        let (scene, meshes) = cubes();
        let bounds: Vec<_> = meshes.iter().map(Bounds::of).collect();
        // Along +x from the left: the first cube, its face at x = -0.5.
        let (index, distance) =
            pick(&scene, &meshes, &bounds, Vec3::new(-5.0, 0.0, 0.0), Vec3::X).expect("hits");
        assert_eq!(index, 0);
        assert!((distance - 4.5).abs() < 1e-4, "{distance}");
        // From the right, the second.
        let (index, _) = pick(
            &scene,
            &meshes,
            &bounds,
            Vec3::new(9.0, 0.0, 0.0),
            Vec3::NEG_X,
        )
        .expect("hits");
        assert_eq!(index, 1);
        // Between them, looking up: nothing.
        assert_eq!(
            pick(&scene, &meshes, &bounds, Vec3::new(1.5, -5.0, 0.0), Vec3::Y),
            None
        );
    }

    #[test]
    fn a_ray_through_the_bounds_but_not_the_triangles_misses() {
        let mut scene = Scene::new("torus");
        scene.add_mesh(MeshEntry::new(
            "torus",
            MeshSource::Torus {
                radius: 1.0,
                tube_radius: 0.25,
            },
        ));
        scene.add_material(MaterialEntry::new("plain", Graph::new("plain")));
        scene.add_instance(Instance::new(0, 0));
        let (vertices, indices) = wxsl_render::mesh::torus_geometry(1.0, 0.25, 64, 24);
        let meshes = vec![MeshData::new(vertices, indices)];
        let bounds: Vec<_> = meshes.iter().map(Bounds::of).collect();
        // Down through the hole, which is inside the box.
        assert_eq!(
            pick(
                &scene,
                &meshes,
                &bounds,
                Vec3::new(0.0, 5.0, 0.0),
                Vec3::NEG_Y
            ),
            None
        );
        // Down through the tube.
        assert!(pick(
            &scene,
            &meshes,
            &bounds,
            Vec3::new(1.0, 5.0, 0.0),
            Vec3::NEG_Y
        )
        .is_some());
    }

    #[test]
    fn a_picked_instance_under_a_scaled_node_reports_a_world_distance() {
        let (mut scene, meshes) = cubes();
        let node = scene.add_node(
            SceneNode::new("big")
                .with_transform(Mat4::from_scale(Vec3::splat(2.0)).to_cols_array()),
        );
        scene.instances[0].node = Some(node);
        scene.instances.truncate(1);
        let bounds: Vec<_> = meshes.iter().map(Bounds::of).collect();
        let (_, distance) =
            pick(&scene, &meshes, &bounds, Vec3::new(-5.0, 0.0, 0.0), Vec3::X).expect("hits");
        // The face is at x = -1 once doubled.
        assert!((distance - 4.0).abs() < 1e-4, "{distance}");
    }

    #[test]
    fn the_centre_of_the_viewport_looks_at_the_orbit_target() {
        let orbit = Orbit::default();
        let camera = orbit.camera(1.5);
        let (origin, direction) = ray(&camera, glam::Vec2::ZERO);
        let towards = (orbit.target - origin).normalize();
        assert!(direction.dot(towards) > 0.9999);
        // Right of centre leans right.
        let (_, right) = ray(&camera, glam::Vec2::new(1.0, 0.0));
        let side = (camera.target - camera.eye).cross(camera.up);
        assert!(right.dot(side) > 0.0);
    }

    #[test]
    fn appending_a_scene_shifts_every_index_past_what_is_there() {
        let (first, meshes) = cubes();
        let mut open = SceneDoc::new(first, None);
        let mut second = Scene::new("imported");
        second.add_mesh(MeshEntry::new("part", MeshSource::Embedded { index: 0 }));
        second.add_texture(wxsl_core::scene::TextureEntry::new(
            "albedo",
            TextureSource::Embedded { index: 0 },
        ));
        second.add_material(
            MaterialEntry::new("imported", Graph::new("imported")).with_texture("albedo", 0),
        );
        let root = second.add_node(SceneNode::new("root"));
        let child = second.add_node(SceneNode::new("child").with_parent(root));
        second.add_instance(Instance::new(0, 0).with_node(child));
        let mut imported = SceneDoc::new(second, None);
        imported.meshes = Arc::new(meshes.clone());
        imported.images = Arc::new(vec![vec![1, 2, 3]]);
        open.meshes = Arc::new(meshes);

        open.append(imported);
        let scene = &open.document;
        assert!(scene.validate().is_empty(), "{:?}", scene.validate());
        assert_eq!(scene.meshes.len(), 2);
        assert_eq!(
            scene.meshes[1].source,
            MeshSource::Embedded { index: 1 },
            "past the supplied mesh already there"
        );
        assert_eq!(scene.materials[1].bindings.textures["albedo"], 0);
        assert_eq!(scene.nodes[1].parent, Some(0));
        let added = &scene.instances[2];
        assert_eq!((added.mesh, added.material, added.node), (1, 1, Some(1)));
        assert_eq!(open.meshes.len(), 2);
        assert_eq!(open.images.len(), 1);
    }

    #[test]
    fn a_transform_survives_its_decomposition() {
        let transform = compose([1.0, -2.0, 0.5], [10.0, 20.0, -30.0], [1.0, 2.0, 0.5]);
        let (translation, degrees, scale) = decompose(&transform);
        let again = compose(translation, degrees, scale);
        for (a, b) in transform.iter().zip(again) {
            assert!((a - b).abs() < 1e-5, "{transform:?} vs {again:?}");
        }
    }

    #[test]
    fn a_change_between_two_scenes_is_as_small_as_it_can_be() {
        let (scene, _) = cubes();
        let old = SceneDoc::new(scene, None);
        let mut new = old.clone();
        new.document.instances[1].transform = Mat4::IDENTITY.to_cols_array();
        let changes = SceneChanges::between(Some(&old), Some(&new));
        assert_eq!(
            changes,
            SceneChanges {
                instances: true,
                ..SceneChanges::default()
            }
        );
        new.document.materials[0].config.render_order = 3;
        assert_eq!(
            SceneChanges::between(Some(&old), Some(&new)).materials,
            BTreeSet::from([0])
        );
        new.document.instances.pop();
        assert!(SceneChanges::between(Some(&old), Some(&new)).reload);
        assert!(SceneChanges::between(None, Some(&new)).reload);
    }
}
