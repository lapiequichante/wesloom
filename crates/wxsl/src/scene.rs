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
use wxsl_render::draw::{DrawItem, DrawList};
use wxsl_render::material::Material;
use wxsl_render::mesh::Mesh;
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
                Material::from_graph_with_macros(&entry.graph, registry, &entry.macros).map_err(
                    |error| LoadError::Material {
                        material: entry.name.clone(),
                        error,
                    },
                )?,
            );
        }

        let instances = scene
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
            instances,
        })
    }

    /// The draws this scene contributes, in instance order.
    pub fn draw_list(&self) -> DrawList<'_> {
        self.instances
            .iter()
            .map(|instance| {
                DrawItem::new(
                    &self.meshes[instance.mesh],
                    &self.materials[instance.material],
                )
                .with_transform(instance.transform)
                .with_tags(&instance.tags)
            })
            .collect()
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
            LoadError::Render(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Material { error, .. } | LoadError::Render(error) => Some(error),
            LoadError::Invalid(_) => None,
        }
    }
}

impl From<RenderError> for LoadError {
    fn from(value: RenderError) -> Self {
        LoadError::Render(value)
    }
}
