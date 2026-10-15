//! Turning a [`wxsl_core::scene::Scene`] into something the renderer can
//! draw.
//!
//! A loader beside the renderer, not part of it: the [`Renderer`] still
//! takes a draw list and never holds a scene. This module needs the node
//! registry and the device at once, which this crate already has; it lives
//! here rather than in the facade so the editor's scene mode can use it
//! ([ADR 0082](../../../docs/adr/0082-the-editors-scene-mode-and-scene-resources-in-the-renderer-crate.md)).
//! The facade re-exports it as `wxsl::scene`.
//!
//! ```text
//!   scene::Scene ──load──> SceneResources ──draw_list──> DrawList
//!   (meshes by name,       (GPU meshes,                  (what the
//!    graphs, instances)     compiled materials)           renderer takes)
//! ```
//!
//! A version 2 document also names its textures and each material's
//! bindings, which [`SceneResources::create_bindings`] resolves, and may
//! carry lights and cameras, which [`environment`] translates
//! ([ADR 0079](../../../docs/adr/0079-scene-document-v2-textures-bindings-nodes-environment.md)).

use std::path::{Path, PathBuf};

use crate::bindings::MaterialBindings;
use crate::draw::{DrawItem, DrawList, InstanceAttributes};
use crate::environment::{Camera, Environment, Light};
use crate::material::Material;
use crate::mesh::{Mesh, MeshData};
use crate::renderer::Renderer;
use crate::setup::{check_scene, Incompatibility};
use crate::RenderError;
use wxsl_core::node::NodeRegistry;
use wxsl_core::scene::{
    Bindings, ColorSpace, Filter, MeshSource, SamplerEntry, Scene, SceneError, SceneLightKind,
    Tags, TextureEntry, TextureSource, Wrap,
};

/// A scene's meshes and materials, on the GPU and compiled.
///
/// Holds the resources so a [`DrawList`] can borrow them: the list is
/// rebuilt every frame and owns nothing, which is what keeps culling and
/// sorting the application's business.
pub struct SceneResources {
    meshes: Vec<Mesh>,
    materials: Vec<Material>,
    /// One per material, once [`SceneResources::bind`] has run. Empty
    /// before that, and a material declaring nothing never needs one.
    bindings: Vec<MaterialBindings>,
    /// One per *instance*, not per material: what the draw supplies for
    /// the per-instance attributes its material declares. A scene
    /// document has no way to name these, so they start empty and the
    /// application fills them in.
    attributes: Vec<InstanceAttributes>,
    instances: Vec<ResolvedInstance>,
    /// The instances in draw order: by material, then mesh, so identical
    /// pairs are adjacent and the renderer draws each run with one call
    /// (ADR 0086).
    order: Vec<usize>,
    /// Each mesh's local box, `None` for a mesh with no vertices.
    mesh_bounds: Vec<Option<(glam::Vec3, glam::Vec3)>>,
    /// Per material, whether its vertex stage may move vertices, which
    /// leaves its draws without a box, so they are never culled.
    displaces: Vec<bool>,
    /// What the document binds, per material, applied by
    /// [`SceneResources::create_bindings`].
    material_bindings: Vec<Bindings>,
    /// The document's textures, uploaded by
    /// [`SceneResources::create_bindings`].
    texture_entries: Vec<TextureEntry>,
    textures: Vec<SceneTexture>,
    /// What an `embedded` texture source indexes.
    images: Vec<SceneImage>,
    base: Option<PathBuf>,
}

/// An image a host supplies with a scene document, for its `embedded`
/// texture sources — a GLB's images, say.
#[derive(Clone, Debug)]
pub enum SceneImage {
    /// Decoded pixels, 8-bit RGBA, rows top to bottom.
    Rgba8 {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// `width * height * 4` bytes.
        pixels: Vec<u8>,
    },
    /// A PNG file's bytes, decoded on upload. Needs the `images` feature.
    Encoded(Vec<u8>),
}

struct SceneTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

struct ResolvedInstance {
    mesh: usize,
    material: usize,
    transform: glam::Mat4,
    tags: Tags,
    /// The mesh's box in world space.
    bounds: Option<(glam::Vec3, glam::Vec3)>,
}

impl SceneResources {
    /// Upload every mesh and compile every material of `scene`.
    ///
    /// `base` is the directory file paths are resolved against — normally
    /// the scene document's own directory, so a scene and its `.glb` move
    /// together.
    pub fn load(
        device: &wgpu::Device,
        scene: &Scene,
        registry: &NodeRegistry,
        base: Option<&Path>,
    ) -> Result<Self, LoadError> {
        Self::load_with_lighting(
            device,
            scene,
            registry,
            base,
            &wxsl_core::lighting::LightingSet::default(),
        )
    }

    /// The same, with every material's model resolved against the lighting
    /// set the surrounding pipeline enables.
    ///
    /// The same set must go to the renderer
    /// (`Renderer::set_lighting`); a scene naming a model the
    /// set does not enable is an error here, naming the material.
    pub fn load_with_lighting(
        device: &wgpu::Device,
        scene: &Scene,
        registry: &NodeRegistry,
        base: Option<&Path>,
        lighting: &wxsl_core::lighting::LightingSet,
    ) -> Result<Self, LoadError> {
        Self::load_with_plan(device, scene, registry, base, lighting, &[])
    }

    /// The same again, against the *whole* plan the pipeline publishes: its
    /// lighting set and its feature channels.
    ///
    /// Both halves, because both shape the G-buffer struct a material is
    /// generated for (plan2 P12, ADR 0037). The renderer must be running
    /// the same pair — `set_lighting` and `set_features` — and a material
    /// resolved against another plan is a named error at the first frame.
    ///
    /// The capability check ([`check_scene`], ADR 0044) runs before
    /// anything is built: every way the scene does not fit the plan is
    /// returned by name, so a scene whose second material also mismatches
    /// is refused once, completely, with no mesh uploaded on its behalf.
    pub fn load_with_plan(
        device: &wgpu::Device,
        scene: &Scene,
        registry: &NodeRegistry,
        base: Option<&Path>,
        lighting: &wxsl_core::lighting::LightingSet,
        features: &[wxsl_core::lighting::ChannelRequest],
    ) -> Result<Self, LoadError> {
        Self::load_supplied(device, scene, registry, base, lighting, features, &[])
    }

    /// An imported glTF file's scene (ADR 0080), with its meshes and
    /// images, against the pipeline's plan as in
    /// [`SceneResources::load_with_plan`]. The lighting set must enable
    /// every model the import names; [`ImportedScene::lighting`] is that
    /// set.
    ///
    /// [`ImportedScene::lighting`]: wxsl_gltf::ImportedScene::lighting
    #[cfg(feature = "gltf")]
    pub fn load_imported(
        device: &wgpu::Device,
        imported: &wxsl_gltf::ImportedScene,
        registry: &NodeRegistry,
        lighting: &wxsl_core::lighting::LightingSet,
        features: &[wxsl_core::lighting::ChannelRequest],
    ) -> Result<Self, LoadError> {
        let mut resources = Self::load_supplied(
            device,
            &imported.scene,
            registry,
            imported.directory.as_deref(),
            lighting,
            features,
            &imported
                .meshes
                .iter()
                .map(crate::gltf::mesh_data)
                .collect::<Vec<_>>(),
        )?;
        resources.set_images(
            imported
                .images
                .iter()
                .map(|bytes| SceneImage::Encoded(bytes.clone()))
                .collect(),
        );
        Ok(resources)
    }

    fn load_supplied(
        device: &wgpu::Device,
        scene: &Scene,
        registry: &NodeRegistry,
        base: Option<&Path>,
        lighting: &wxsl_core::lighting::LightingSet,
        features: &[wxsl_core::lighting::ChannelRequest],
        supplied: &[MeshData],
    ) -> Result<Self, LoadError> {
        let invalid = scene.validate();
        if !invalid.is_empty() {
            return Err(LoadError::Invalid(invalid));
        }
        let incompatible = check_scene(scene, registry, lighting, features);
        if !incompatible.is_empty() {
            return Err(LoadError::Incompatible(incompatible));
        }
        let meshes = scene
            .meshes
            .iter()
            .map(|entry| mesh_data(&entry.name, &entry.source, base, supplied))
            .collect::<Result<Vec<_>, _>>()?;
        Self::build(device, scene, registry, base, lighting, features, &meshes)
    }

    /// The same as [`SceneResources::load_with_plan`], with every mesh
    /// already resolved: `meshes[i]` is the geometry of `scene.meshes[i]`,
    /// whatever its source says (see [`mesh_data`]).
    pub fn load_resolved(
        device: &wgpu::Device,
        scene: &Scene,
        registry: &NodeRegistry,
        base: Option<&Path>,
        lighting: &wxsl_core::lighting::LightingSet,
        features: &[wxsl_core::lighting::ChannelRequest],
        meshes: &[MeshData],
    ) -> Result<Self, LoadError> {
        let invalid = scene.validate();
        if !invalid.is_empty() {
            return Err(LoadError::Invalid(invalid));
        }
        let incompatible = check_scene(scene, registry, lighting, features);
        if !incompatible.is_empty() {
            return Err(LoadError::Incompatible(incompatible));
        }
        Self::build(device, scene, registry, base, lighting, features, meshes)
    }

    /// Compile and upload a scene that passed its checks.
    fn build(
        device: &wgpu::Device,
        scene: &Scene,
        registry: &NodeRegistry,
        base: Option<&Path>,
        lighting: &wxsl_core::lighting::LightingSet,
        features: &[wxsl_core::lighting::ChannelRequest],
        meshes: &[MeshData],
    ) -> Result<Self, LoadError> {
        if meshes.len() != scene.meshes.len() {
            return Err(LoadError::Render(RenderError::Import {
                path: scene.name.clone(),
                reason: format!(
                    "{} meshes resolved for {} in the document",
                    meshes.len(),
                    scene.meshes.len()
                ),
            }));
        }
        let mesh_bounds: Vec<_> = meshes.iter().map(local_bounds).collect();
        let meshes: Vec<Mesh> = scene
            .meshes
            .iter()
            .zip(meshes)
            .map(|(entry, data)| Mesh::upload(device, &entry.name, data))
            .collect();

        let mut materials: Vec<Material> = Vec::with_capacity(scene.materials.len());
        for (index, entry) in scene.materials.iter().enumerate() {
            // A material generated already for an identical entry is
            // cloned, not generated again (ADR 0086).
            let same = scene.materials[..index]
                .iter()
                .position(|earlier| earlier.graph == entry.graph && earlier.config == entry.config);
            if let Some(same) = same {
                materials.push(materials[same].clone());
                continue;
            }
            materials.push(
                // The document's own configuration, with the one field a
                // document cannot author filled in from the pipeline's plan
                // (ADR 0038).
                Material::with_lighting(
                    &entry.graph,
                    registry,
                    &entry.config.clone().with_features(features.to_vec()),
                    lighting,
                )
                .map_err(|error| LoadError::Material {
                    material: entry.name.clone(),
                    error,
                })?,
            );
        }

        let instances = resolve_instances(scene, &mesh_bounds);

        Ok(SceneResources {
            meshes,
            materials,
            bindings: Vec::new(),
            attributes: vec![InstanceAttributes::new(); instances.len()],
            order: draw_order(&instances),
            instances,
            mesh_bounds,
            displaces: scene
                .materials
                .iter()
                .map(|entry| displaces(&entry.graph))
                .collect(),
            material_bindings: scene
                .materials
                .iter()
                .map(|entry| entry.bindings.clone())
                .collect(),
            texture_entries: scene.textures.clone(),
            textures: Vec::new(),
            images: Vec::new(),
            base: base.map(Path::to_path_buf),
        })
    }

    /// Supply the images the document's `embedded` texture sources index,
    /// before [`SceneResources::create_bindings`].
    pub fn set_images(&mut self, images: Vec<SceneImage>) {
        self.images = images;
    }

    /// The draws this scene contributes, ordered by material and mesh so
    /// the renderer draws identical pairs with one instanced call, and
    /// each with its world box so a pass can cull it (ADR 0086).
    /// [`SceneResources::draw_instance`] maps a draw back to its instance.
    pub fn draw_list(&self) -> DrawList<'_> {
        self.order
            .iter()
            .map(|&index| {
                let instance = &self.instances[index];
                let mut item = DrawItem::new(
                    &self.meshes[instance.mesh],
                    &self.materials[instance.material],
                )
                .with_transform(instance.transform)
                .with_tags(&instance.tags);
                if let Some((min, max)) = instance.bounds {
                    if !self.displaces[instance.material] {
                        item = item.with_bounds(min, max);
                    }
                }
                if let Some(bindings) = self.bindings.get(instance.material) {
                    item = item.with_bindings(bindings);
                }
                if let Some(attributes) = self.attributes.get(index) {
                    item = item.with_attributes(attributes);
                }
                item
            })
            .collect()
    }

    /// The document instance [`SceneResources::draw_list`]'s draw `draw`
    /// draws.
    pub fn draw_instance(&self, draw: usize) -> Option<usize> {
        self.order.get(draw).copied()
    }

    /// The per-instance attributes instance `index` supplies, to fill in.
    ///
    /// A scene document names meshes, materials and transforms; it has no
    /// vocabulary for "this cube's tint". So a material that declares a
    /// per-instance attribute leaves a hole here, and the application is
    /// what fills it — which is also the only way a *per-instance* value
    /// could work, since the document has one material and many
    /// instances
    /// ([ADR 0024](../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
    pub fn instance_attributes_mut(&mut self, index: usize) -> Option<&mut InstanceAttributes> {
        self.attributes.get_mut(index)
    }

    /// How many instances there are, so a caller can fill every one.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Upload the document's textures and give every material its
    /// parameters, textures and samplers: the values its graph declared,
    /// overridden by what the document binds.
    ///
    /// Separate from [`SceneResources::load`] because it is the one step
    /// that needs a [`Renderer`]: a bind group needs a layout, and the
    /// layouts are shared per interface shape by the renderer that will
    /// draw with them.
    ///
    /// A binding naming a slot the material does not declare is an error
    /// naming the material. A slot the document leaves unbound can still be
    /// filled through [`SceneResources::bindings_mut`] before
    /// [`SceneResources::upload`].
    pub fn create_bindings(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
    ) -> Result<(), LoadError> {
        self.textures = self
            .texture_entries
            .iter()
            .map(|entry| {
                let image = self.image_for(entry)?;
                Ok(upload_texture(device, queue, entry, &image))
            })
            .collect::<Result<_, LoadError>>()?;

        self.bindings = (0..self.materials.len())
            .map(|index| self.bind(device, renderer, index))
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    /// Material `index`'s bind group: its declared values, overridden by
    /// what the document binds.
    fn bind(
        &self,
        device: &wgpu::Device,
        renderer: &mut Renderer,
        index: usize,
    ) -> Result<MaterialBindings, LoadError> {
        let (material, document) = (&self.materials[index], &self.material_bindings[index]);
        let mut bindings = renderer.material_bindings(device, material);
        let failed = |error| LoadError::Bindings {
            material: material.name.clone(),
            error,
        };
        for (name, value) in &document.params {
            bindings.set(name, *value).map_err(failed)?;
        }
        for (slot, &texture) in &document.textures {
            let texture = self
                .textures
                .get(texture)
                .ok_or_else(|| LoadError::Texture {
                    texture: texture.to_string(),
                    reason: "not uploaded; call create_bindings first".to_string(),
                })?;
            let is_sampler = material.interface().resources.iter().any(|resource| {
                resource.name.as_str() == slot && resource.ty == wxsl_core::node::ValueType::Sampler
            });
            if is_sampler {
                bindings.set_sampler(slot, &texture.sampler)
            } else {
                bindings.set_texture(slot, &texture.view)
            }
            .map_err(failed)?;
        }
        Ok(bindings)
    }

    /// Recompile material `index` from `entry` and rebind it, reusing the
    /// uploaded textures: one edit in the editor's material canvas, live
    /// in the scene (ADR 0082). On an error the old material stays.
    #[allow(clippy::too_many_arguments)]
    pub fn set_material(
        &mut self,
        device: &wgpu::Device,
        renderer: &mut Renderer,
        index: usize,
        entry: &wxsl_core::scene::MaterialEntry,
        registry: &NodeRegistry,
        lighting: &wxsl_core::lighting::LightingSet,
        features: &[wxsl_core::lighting::ChannelRequest],
    ) -> Result<(), LoadError> {
        if index >= self.materials.len() {
            return Err(LoadError::Render(RenderError::Import {
                path: entry.name.clone(),
                reason: format!("no material {index} in the loaded scene"),
            }));
        }
        let material = Material::with_lighting(
            &entry.graph,
            registry,
            &entry.config.clone().with_features(features.to_vec()),
            lighting,
        )
        .map_err(|error| LoadError::Material {
            material: entry.name.clone(),
            error,
        })?;
        let old = std::mem::replace(&mut self.materials[index], material);
        let old_bindings =
            std::mem::replace(&mut self.material_bindings[index], entry.bindings.clone());
        if index < self.bindings.len() {
            match self.bind(device, renderer, index) {
                Ok(bindings) => self.bindings[index] = bindings,
                Err(error) => {
                    self.materials[index] = old;
                    self.material_bindings[index] = old_bindings;
                    return Err(error);
                }
            }
        }
        self.displaces[index] = displaces(&entry.graph);
        Ok(())
    }

    /// Re-read every instance's transform, mesh, material and tags from
    /// `scene`, which must have as many meshes, materials and instances as
    /// the one loaded. Cheap: nothing is uploaded or compiled.
    pub fn set_instances(&mut self, scene: &Scene) -> Result<(), LoadError> {
        let invalid = scene.validate();
        if !invalid.is_empty() {
            return Err(LoadError::Invalid(invalid));
        }
        if scene.meshes.len() != self.meshes.len()
            || scene.materials.len() != self.materials.len()
            || scene.instances.len() != self.instances.len()
        {
            return Err(LoadError::Render(RenderError::Import {
                path: scene.name.clone(),
                reason: "the scene's meshes, materials or instances changed; load it again"
                    .to_string(),
            }));
        }
        self.instances = resolve_instances(scene, &self.mesh_bounds);
        self.order = draw_order(&self.instances);
        Ok(())
    }

    /// The decoded pixels for `entry`.
    fn image_for(&self, entry: &TextureEntry) -> Result<Decoded, LoadError> {
        let failed = |reason: String| LoadError::Texture {
            texture: entry.name.clone(),
            reason,
        };
        match &entry.source {
            TextureSource::Embedded { index } => match self.images.get(*index) {
                Some(SceneImage::Rgba8 {
                    width,
                    height,
                    pixels,
                }) => {
                    if pixels.len() != (*width as usize) * (*height as usize) * 4 {
                        return Err(failed(format!(
                            "embedded image {index} is not {width}×{height} RGBA8"
                        )));
                    }
                    Ok(Decoded {
                        width: *width,
                        height: *height,
                        pixels: pixels.clone(),
                    })
                }
                Some(SceneImage::Encoded(bytes)) => decode(bytes).map_err(failed),
                None => Err(failed(format!(
                    "no embedded image {index}; the host supplied {}",
                    self.images.len()
                ))),
            },
            TextureSource::File { path } => {
                let resolved = match &self.base {
                    Some(base) => base.join(path),
                    None => PathBuf::from(path),
                };
                let bytes = std::fs::read(&resolved)
                    .map_err(|error| failed(format!("{}: {error}", resolved.display())))?;
                decode(&bytes).map_err(failed)
            }
        }
    }

    /// Push every material's parameters and resources to the GPU.
    ///
    /// Cheap when nothing changed, so it is safe every frame. A texture
    /// the graph declared and nobody bound is reported here, naming the
    /// material and the texture, rather than drawn as black.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(), LoadError> {
        for (index, bindings) in self.bindings.iter_mut().enumerate() {
            bindings
                .upload(device, queue)
                .map_err(|error| LoadError::Bindings {
                    material: self.materials[index].name.clone(),
                    error,
                })?;
        }
        Ok(())
    }

    /// One material's bind group, for setting a parameter or supplying a
    /// texture. `None` before [`SceneResources::create_bindings`] has run.
    pub fn bindings_mut(&mut self, material: usize) -> Option<&mut MaterialBindings> {
        self.bindings.get_mut(material)
    }

    /// The uploaded meshes, in document order.
    pub fn meshes(&self) -> &[Mesh] {
        &self.meshes
    }

    /// The compiled materials, in document order.
    pub fn materials(&self) -> &[Material] {
        &self.materials
    }
}

fn resolve_instances(
    scene: &Scene,
    mesh_bounds: &[Option<(glam::Vec3, glam::Vec3)>],
) -> Vec<ResolvedInstance> {
    scene
        .instances
        .iter()
        .map(|instance| {
            let transform = glam::Mat4::from_cols_array(&scene.instance_world(instance));
            ResolvedInstance {
                mesh: instance.mesh,
                material: instance.material,
                transform,
                tags: scene.instance_tags(instance).clone(),
                bounds: mesh_bounds[instance.mesh].map(|(min, max)| {
                    wxsl_frame::environment::transform_bounds(transform, min, max)
                }),
            }
        })
        .collect()
}

/// The instances by material, then mesh, then document order.
fn draw_order(instances: &[ResolvedInstance]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..instances.len()).collect();
    order.sort_by_key(|&index| (instances[index].material, instances[index].mesh));
    order
}

/// A mesh's local box.
fn local_bounds(data: &MeshData) -> Option<(glam::Vec3, glam::Vec3)> {
    let mut positions = data
        .vertices
        .iter()
        .map(|vertex| glam::Vec3::from(vertex.position));
    let first = positions.next()?;
    Some(positions.fold((first, first), |(min, max), p| (min.min(p), max.max(p))))
}

/// Whether `graph` has a vertex stage, which may move vertices outside the
/// mesh's box.
fn displaces(graph: &wxsl_core::graph::Graph) -> bool {
    graph
        .nodes()
        .any(|(_, node)| node.def == wxsl_core::abi::VERTEX_OUTPUT_ID)
}

/// The geometry one mesh source describes, on the CPU.
///
/// `base` resolves a `file` path; `supplied` is what an `embedded` source
/// indexes. The editor resolves every mesh once with this, for picking,
/// and hands the result to [`SceneResources::load_resolved`].
pub fn mesh_data(
    name: &str,
    source: &MeshSource,
    base: Option<&Path>,
    supplied: &[MeshData],
) -> Result<MeshData, LoadError> {
    let (vertices, indices) = match source {
        MeshSource::Cube { size } => crate::mesh::cube_geometry(*size),
        MeshSource::Sphere { radius } => crate::mesh::sphere_geometry(*radius, 48, 32),
        MeshSource::Plane { size } => crate::mesh::plane_geometry(*size, 32),
        MeshSource::Torus {
            radius,
            tube_radius,
        } => crate::mesh::torus_geometry(*radius, *tube_radius, 64, 24),
        MeshSource::File { path, primitive } => {
            let resolved = match base {
                Some(base) => base.join(path),
                None => PathBuf::from(path),
            };
            return load_file(&resolved, *primitive);
        }
        MeshSource::Embedded { index } => {
            return supplied.get(*index).cloned().ok_or_else(|| {
                LoadError::Render(RenderError::Import {
                    path: name.to_string(),
                    reason: format!("no supplied mesh {index}"),
                })
            })
        }
    };
    Ok(MeshData::new(vertices, indices))
}

/// The lights and camera a document's environment block describes, for a
/// viewport of `aspect` (width over height). `None` when the document has
/// no environment block.
///
/// The first camera is the one returned; without one, the default camera.
/// Every other field is the default environment's: ambient, exposure and
/// time are the application's, not the document's.
pub fn environment(scene: &Scene, aspect: f32) -> Option<Environment> {
    let block = scene.environment.as_ref()?;
    let placed = |node| glam::Mat4::from_cols_array(&scene.placement(node));
    let camera = match block.cameras.first() {
        Some(camera) => {
            let world = placed(camera.node);
            let eye = world.transform_point3(glam::Vec3::ZERO);
            Camera {
                eye,
                target: eye + world.transform_vector3(glam::Vec3::NEG_Z),
                up: world.transform_vector3(glam::Vec3::Y),
                fov_y: camera.fov_y,
                aspect,
                near: camera.near,
                far: camera.far,
                ..Camera::default()
            }
        }
        None => Camera {
            aspect,
            ..Camera::default()
        },
    };
    let lights = block
        .lights
        .iter()
        .map(|light| {
            let world = placed(light.node);
            let position = world.transform_point3(glam::Vec3::ZERO);
            let forward = world
                .transform_vector3(glam::Vec3::NEG_Z)
                .normalize_or(glam::Vec3::NEG_Z);
            let color = glam::Vec3::from_array(light.color);
            let mut resolved = match light.kind {
                // `position_or_direction` points *towards* the light.
                SceneLightKind::Directional => Light::directional(-forward, color, light.intensity),
                SceneLightKind::Point => Light::point(position, color, light.intensity),
                SceneLightKind::Spot {
                    inner_angle,
                    outer_angle,
                } => Light::spot(
                    position,
                    forward,
                    inner_angle,
                    outer_angle,
                    color,
                    light.intensity,
                ),
            };
            resolved.range = light.range;
            resolved.casts_shadow = light.casts_shadow;
            resolved
        })
        .collect();
    Some(Environment {
        camera,
        lights,
        ..Environment::default()
    })
}

/// An image as RGBA8, top row first.
struct Decoded {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[cfg(feature = "images")]
fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| error.to_string())?
        .to_rgba8();
    Ok(Decoded {
        width: image.width(),
        height: image.height(),
        pixels: image.into_raw(),
    })
}

#[cfg(not(feature = "images"))]
fn decode(_bytes: &[u8]) -> Result<Decoded, String> {
    Err("this build has no `images` feature to decode it".to_string())
}

/// Upload `image` with a full mip chain, generated on the CPU (ADR 0061).
fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    entry: &TextureEntry,
    image: &Decoded,
) -> SceneTexture {
    let srgb = entry.color_space == ColorSpace::Srgb;
    let levels = mip_chain(image, srgb);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&entry.name),
        size: wgpu::Extent3d {
            width: image.width.max(1),
            height: image.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
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
    for (mip, level) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &level.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(level.width * 4),
                rows_per_image: Some(level.height),
            },
            wgpu::Extent3d {
                width: level.width,
                height: level.height,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&sampler_descriptor(&entry.name, &entry.sampler));
    SceneTexture {
        _texture: texture,
        view,
        sampler,
    }
}

fn sampler_descriptor<'a>(label: &'a str, sampler: &SamplerEntry) -> wgpu::SamplerDescriptor<'a> {
    let filter = |filter| match filter {
        Filter::Nearest => wgpu::FilterMode::Nearest,
        Filter::Linear => wgpu::FilterMode::Linear,
    };
    let wrap = |wrap| match wrap {
        Wrap::Repeat => wgpu::AddressMode::Repeat,
        Wrap::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        Wrap::ClampToEdge => wgpu::AddressMode::ClampToEdge,
    };
    wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wrap(sampler.wrap_u),
        address_mode_v: wrap(sampler.wrap_v),
        mag_filter: filter(sampler.mag),
        min_filter: filter(sampler.min),
        mipmap_filter: match sampler.mipmap {
            Filter::Nearest => wgpu::MipmapFilterMode::Nearest,
            Filter::Linear => wgpu::MipmapFilterMode::Linear,
        },
        ..Default::default()
    }
}

/// Every mip level of `image`, the full-size one first, each a 2×2 box
/// filter of the one above. An sRGB image is averaged in linear light, so a
/// distant checker greys to its true mean rather than too dark.
fn mip_chain(image: &Decoded, srgb: bool) -> Vec<Decoded> {
    let to_linear = |byte: u8| {
        let value = byte as f32 / 255.0;
        if !srgb {
            value
        } else if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let from_linear = |value: f32| {
        let encoded = if !srgb {
            value
        } else if value <= 0.0031308 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
    };

    let mut levels = vec![Decoded {
        width: image.width.max(1),
        height: image.height.max(1),
        pixels: image.pixels.clone(),
    }];
    loop {
        let above = levels.last().expect("starts with one");
        if above.width == 1 && above.height == 1 {
            return levels;
        }
        let (width, height) = ((above.width / 2).max(1), (above.height / 2).max(1));
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let texel = |dx: u32, dy: u32| {
                    let (sx, sy) = (
                        (x * 2 + dx).min(above.width - 1),
                        (y * 2 + dy).min(above.height - 1),
                    );
                    ((sy * above.width + sx) * 4) as usize
                };
                let corners = [texel(0, 0), texel(1, 0), texel(0, 1), texel(1, 1)];
                for channel in 0..4 {
                    let sum: f32 = corners
                        .iter()
                        .map(|&at| above.pixels[at + channel])
                        // Alpha is linear in either colour space.
                        .map(|byte| {
                            if channel == 3 {
                                byte as f32 / 255.0
                            } else {
                                to_linear(byte)
                            }
                        })
                        .sum();
                    let mean = sum / 4.0;
                    pixels.push(if channel == 3 {
                        (mean.clamp(0.0, 1.0) * 255.0).round() as u8
                    } else {
                        from_linear(mean)
                    });
                }
            }
        }
        levels.push(Decoded {
            width,
            height,
            pixels,
        });
    }
}

#[cfg(feature = "gltf")]
fn load_file(path: &Path, primitive: Option<usize>) -> Result<MeshData, LoadError> {
    Ok(match primitive {
        Some(index) => {
            let primitives = crate::gltf::load(path)?;
            primitives.into_iter().nth(index).ok_or_else(|| {
                LoadError::Render(RenderError::Import {
                    path: path.display().to_string(),
                    reason: format!("no primitive {index}"),
                })
            })?
        }
        None => crate::gltf::load_merged(path)?,
    })
}

#[cfg(not(feature = "gltf"))]
fn load_file(path: &Path, _primitive: Option<usize>) -> Result<MeshData, LoadError> {
    // Reported rather than silently drawn as nothing: a scene that loads
    // with one mesh missing is a bug report about lighting.
    Err(LoadError::Render(RenderError::Import {
        path: path.display().to_string(),
        reason: "this build has no `gltf` feature".to_string(),
    }))
}

/// Why a scene could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    /// The document names indices that do not exist.
    Invalid(Vec<SceneError>),
    /// The scene does not fit the plan it was loaded under — a feature a
    /// material pins that the plan does not carry, a model the lighting
    /// set does not enable — every mismatch named, before anything built
    /// (ADR 0044).
    Incompatible(Vec<Incompatibility>),
    /// A material's graph would not compile.
    Material {
        /// Which material.
        material: String,
        /// What the compiler said.
        error: RenderError,
    },
    /// A material's bind group could not be finished: a binding naming a
    /// slot the material does not declare, or a texture nobody bound.
    Bindings {
        /// Which material.
        material: String,
        /// What was missing.
        error: RenderError,
    },
    /// A texture's image could not be read or decoded.
    Texture {
        /// Which texture.
        texture: String,
        /// Why.
        reason: String,
    },
    /// Something else the renderer refused, such as a file it cannot read.
    Render(RenderError),
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoadError::Invalid(errors) => {
                writeln!(f, "the scene document is not consistent:")?;
                for error in errors {
                    writeln!(f, "  {error}")?;
                }
                Ok(())
            }
            LoadError::Incompatible(incompatibilities) => {
                writeln!(f, "the scene does not fit this pipeline:")?;
                for incompatibility in incompatibilities {
                    writeln!(f, "  {incompatibility}")?;
                }
                Ok(())
            }
            LoadError::Material { material, error } => {
                write!(f, "cannot compile material `{material}`: {error}")
            }
            LoadError::Bindings { material, error } => {
                write!(f, "cannot bind material `{material}`: {error}")
            }
            LoadError::Texture { texture, reason } => {
                write!(f, "cannot load texture `{texture}`: {reason}")
            }
            LoadError::Render(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Material { error, .. }
            | LoadError::Bindings { error, .. }
            | LoadError::Render(error) => Some(error),
            LoadError::Invalid(_) | LoadError::Incompatible(_) | LoadError::Texture { .. } => None,
        }
    }
}

impl From<RenderError> for LoadError {
    fn from(value: RenderError) -> Self {
        LoadError::Render(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mip_chain_reaches_one_texel_and_averages_srgb_in_linear_light() {
        // A 2×2 black-and-white checker, plus one odd edge to clamp.
        let image = Decoded {
            width: 3,
            height: 2,
            pixels: [
                [0u8, 0, 0, 255],
                [255; 4],
                [255; 4],
                [255; 4],
                [0, 0, 0, 255],
                [255; 4],
            ]
            .concat(),
        };
        let srgb = mip_chain(&image, true);
        let sizes: Vec<_> = srgb
            .iter()
            .map(|level| (level.width, level.height))
            .collect();
        assert_eq!(sizes, [(3, 2), (1, 1)]);
        // Half white in linear light is 188 in sRGB, not 128.
        assert_eq!(srgb[1].pixels, [188, 188, 188, 255]);
        let linear = mip_chain(&image, false);
        assert_eq!(linear[1].pixels, [128, 128, 128, 255]);
    }
}
