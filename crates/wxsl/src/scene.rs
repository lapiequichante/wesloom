//! Turning a [`wxsl_core::scene::Scene`] into something the renderer can
//! draw.
//!
//! This is the facade's job and nobody else's. `wxsl-core` owns the scene
//! *document* and must not know what a `wgpu` buffer is; `wxsl-render`
//! takes a draw list and must not know how to read a file or where a node
//! registry comes from. Resolving one into the other needs both halves at
//! once, which is precisely what this crate is for
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//!
//! ```text
//!   scene::Scene ──load──> SceneResources ──draw_list──> DrawList
//!   (meshes by name,       (GPU meshes,                  (what the
//!    graphs, instances)     compiled materials)           renderer takes)
//! ```

use std::path::{Path, PathBuf};

use wxsl_core::node::NodeRegistry;
use wxsl_core::scene::{MeshSource, Scene, SceneError, Tags};
use wxsl_render::bindings::MaterialBindings;
use wxsl_render::draw::{DrawItem, DrawList, InstanceAttributes};
use wxsl_render::material::{Material, MaterialOptions};
use wxsl_render::mesh::Mesh;
use wxsl_render::renderer::Renderer;
use wxsl_render::RenderError;
use wxsl_render::{glam, wgpu};

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
    /// application fills them in — the same gap
    /// [`SceneResources::create_bindings`] leaves for textures.
    attributes: Vec<InstanceAttributes>,
    instances: Vec<ResolvedInstance>,
}

struct ResolvedInstance {
    mesh: usize,
    material: usize,
    transform: glam::Mat4,
    tags: Tags,
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
        let invalid = scene.validate();
        if !invalid.is_empty() {
            return Err(LoadError::Invalid(invalid));
        }

        let mut meshes = Vec::with_capacity(scene.meshes.len());
        for entry in &scene.meshes {
            meshes.push(mesh_from(device, &entry.name, &entry.source, base)?);
        }

        let mut materials = Vec::with_capacity(scene.materials.len());
        for entry in &scene.materials {
            materials.push(
                Material::with_options(
                    &entry.graph,
                    registry,
                    &MaterialOptions {
                        macros: entry.macros.clone(),
                        cast_shadow: entry.cast_shadow,
                        receive_shadow: entry.receive_shadow,
                    },
                )
                .map_err(|error| LoadError::Material {
                    material: entry.name.clone(),
                    error,
                })?,
            );
        }

        let instances: Vec<ResolvedInstance> = scene
            .instances
            .iter()
            .map(|instance| ResolvedInstance {
                mesh: instance.mesh,
                material: instance.material,
                transform: glam::Mat4::from_cols_array(&instance.transform),
                tags: scene.instance_tags(instance).clone(),
            })
            .collect();

        Ok(SceneResources {
            meshes,
            materials,
            bindings: Vec::new(),
            attributes: vec![InstanceAttributes::new(); instances.len()],
            instances,
        })
    }

    /// The draws this scene contributes, in instance order.
    pub fn draw_list(&self) -> DrawList<'_> {
        self.instances
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let mut item = DrawItem::new(
                    &self.meshes[instance.mesh],
                    &self.materials[instance.material],
                )
                .with_transform(instance.transform)
                .with_tags(&instance.tags);
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

    /// Give every material somewhere to put its parameters, textures and
    /// samplers, at the values its graph declared.
    ///
    /// Separate from [`SceneResources::load`] because it is the one step
    /// that needs a [`Renderer`]: a bind group needs a layout, and the
    /// layouts are shared per interface shape by the renderer that will
    /// draw with them.
    ///
    /// Also separate from [`SceneResources::upload`], because a scene
    /// document has no way to name an image yet: a material that declares
    /// a texture is finished by the application, through
    /// [`SceneResources::bindings_mut`], in between the two calls.
    pub fn create_bindings(&mut self, device: &wgpu::Device, renderer: &mut Renderer) {
        self.bindings = self
            .materials
            .iter()
            .map(|material| renderer.material_bindings(device, material))
            .collect();
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

fn mesh_from(
    device: &wgpu::Device,
    name: &str,
    source: &MeshSource,
    base: Option<&Path>,
) -> Result<Mesh, LoadError> {
    Ok(match source {
        MeshSource::Cube { size } => Mesh::cube(device, *size),
        MeshSource::Sphere { radius } => Mesh::sphere(device, *radius),
        MeshSource::Plane { size } => Mesh::plane(device, *size),
        MeshSource::Torus {
            radius,
            tube_radius,
        } => Mesh::torus(device, *radius, *tube_radius),
        MeshSource::File { path, primitive } => {
            let resolved = match base {
                Some(base) => base.join(path),
                None => PathBuf::from(path),
            };
            load_file(device, name, &resolved, *primitive)?
        }
    })
}

#[cfg(feature = "gltf")]
fn load_file(
    device: &wgpu::Device,
    name: &str,
    path: &Path,
    primitive: Option<usize>,
) -> Result<Mesh, LoadError> {
    let data = match primitive {
        Some(index) => {
            let primitives = wxsl_render::gltf::load(path)?;
            primitives.into_iter().nth(index).ok_or_else(|| {
                LoadError::Render(RenderError::Import {
                    path: path.display().to_string(),
                    reason: format!("no primitive {index}"),
                })
            })?
        }
        None => wxsl_render::gltf::load_merged(path)?,
    };
    Ok(Mesh::upload(device, name, &data))
}

#[cfg(not(feature = "gltf"))]
fn load_file(
    _device: &wgpu::Device,
    _name: &str,
    path: &Path,
    _primitive: Option<usize>,
) -> Result<Mesh, LoadError> {
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
    /// A material's graph would not compile.
    Material {
        /// Which material.
        material: String,
        /// What the compiler said.
        error: RenderError,
    },
    /// A material's bind group could not be finished — almost always a
    /// texture the scene document has no way to name yet.
    Bindings {
        /// Which material.
        material: String,
        /// What was missing.
        error: RenderError,
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
            LoadError::Material { material, error } => {
                write!(f, "cannot compile material `{material}`: {error}")
            }
            LoadError::Bindings { material, error } => {
                write!(f, "cannot bind material `{material}`: {error}")
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
            LoadError::Invalid(_) => None,
        }
    }
}

impl From<RenderError> for LoadError {
    fn from(value: RenderError) -> Self {
        LoadError::Render(value)
    }
}
