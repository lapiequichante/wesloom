//! `wxsl-gltf`: device-free glTF 2.0 import
//! ([ADR 0080](../../../docs/adr/0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)).
//!
//! [`import`] reads a `.gltf` or `.glb` file into an [`ImportedScene`]: a
//! [`Scene`] document plus the geometry and image bytes it indexes. No
//! device and no renderer are involved, so the same import serves the
//! Rust facade (`wxsl::asset`, which uploads it), the editor, and C++
//! through the C ABI (`wxsl_import_gltf` in `wxsl-ffi`).
//!
//! - **Structure.** The default scene's nodes become the document's
//!   `nodes`, transforms kept, not baked. Each triangle primitive is one
//!   mesh, and each placement of it one instance under its node.
//! - **Materials.** Each glTF material becomes a generated graph named
//!   [`GLTF_PBR_GRAPH`], built from stdlib nodes found in the registry the
//!   caller passes: texture samples, the factors as parameters (editable
//!   without a recompile), alpha and emissive. Only the branches the
//!   material uses are generated.
//! - **Images** given by a file URI stay files, relative to the glTF file.
//!   Images in the binary chunk, and data URIs, are supplied still encoded
//!   as [`ImportedScene::images`].
//! - **Lights and cameras** (`KHR_lights_punctual`, perspective cameras)
//!   become the document's `environment` block. Intensities are taken 1:1
//!   as the renderer's units.
//!
//! What cannot be mapped is never dropped quietly: every extension the file
//! uses beyond [`MAPPED_EXTENSIONS`], and every ignored texture or setting,
//! is an [`ImportWarning`] naming it.
//!
//! [`geometry`] is the lower half on its own: triangle primitives as CPU
//! geometry, for a `file` mesh source.
//!
//! # Stability
//!
//! Pre-`0.1`, and every item is unstable (ADR 0078) until the import has
//! been used by the editor (plan6 E1).

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod geometry;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use geometry::{Geometry, Vertex, COLOR_ATTRIBUTE, UV1_ATTRIBUTE};
use gltf::material::AlphaMode;
use gltf::texture::{MagFilter, MinFilter, WrappingMode};
use serde_json::Value as Json;
use wxsl_core::graph::{AttributeDecl, Graph, Node, NodeId};
use wxsl_core::lighting::{LightingError, LightingSet, DEFAULT_MODELS, DEFAULT_MODEL_ID};
use wxsl_core::node::{NodeRegistry, Value, ValueType, SETTING_NAME};
use wxsl_core::scene::{
    ColorSpace, Filter, Instance, MaterialEntry, MeshEntry, MeshSource, SamplerEntry, Scene,
    SceneCamera, SceneEnvironment, SceneLight, SceneLightKind, SceneNode, Tags, TextureEntry,
    TextureSource, Wrap, TAG_OPAQUE, TAG_TRANSPARENT,
};

/// The name every generated material graph carries.
pub const GLTF_PBR_GRAPH: &str = "gltf_pbr";

/// Tag of a material the file marks double-sided, which also sets the
/// material's `double_sided` (ADR 0090). The tag is for a pass that
/// selects on it.
pub const TAG_DOUBLE_SIDED: &str = "double_sided";

/// Tag the refraction pass selects (ADR 0067), set on transmissive
/// materials.
pub const TAG_REFRACTIVE: &str = "refractive";

/// The extensions the importer maps. Any other one the file uses is a
/// warning.
pub const MAPPED_EXTENSIONS: &[&str] = &[
    "KHR_lights_punctual",
    "KHR_materials_clearcoat",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_iridescence",
    "KHR_materials_sheen",
    "KHR_materials_transmission",
    "KHR_materials_volume",
    "KHR_texture_transform",
];

/// What [`import`] read: a scene document and the data it indexes.
#[derive(Clone, Debug)]
pub struct ImportedScene {
    /// The document. Its `embedded` meshes index [`ImportedScene::meshes`]
    /// and its `embedded` textures [`ImportedScene::images`].
    pub scene: Scene,
    /// One per triangle primitive, in its mesh's own space.
    pub meshes: Vec<Geometry>,
    /// Images from the binary chunk or data URIs, still encoded (PNG or
    /// JPEG).
    pub images: Vec<Vec<u8>>,
    /// The directory the document's `file` textures are relative to: the
    /// glTF file's own. `None` for an import from bytes.
    pub directory: Option<PathBuf>,
    /// Everything the importer could not map, by name.
    pub warnings: Vec<ImportWarning>,
}

impl ImportedScene {
    /// Every model a material names, plus the default one if a material
    /// names none: the set a pipeline must enable to draw this scene.
    pub fn lighting(&self) -> Result<LightingSet, LightingError> {
        let mut names = Vec::new();
        for material in &self.scene.materials {
            names.push(match &material.config.model {
                Some(model) => model.as_str(),
                None => DEFAULT_MODELS[DEFAULT_MODEL_ID as usize].name,
            });
        }
        if names.is_empty() {
            names.push(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize].name);
        }
        LightingSet::new(
            DEFAULT_MODELS
                .iter()
                .copied()
                .filter(|model| names.contains(&model.name)),
        )
    }
}

/// Something in the file the importer did not map.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ImportWarning {
    /// The file uses an extension the importer does not map.
    UnsupportedExtension {
        /// The extension's name.
        name: String,
    },
    /// Part of a material was not mapped.
    Material {
        /// The material's name.
        material: String,
        /// What was ignored, and how the material differs as a result.
        message: String,
    },
    /// An image could not be used; the textures reading it were dropped.
    Image {
        /// The image's name.
        image: String,
        /// Why.
        message: String,
    },
    /// A primitive was skipped.
    Mesh {
        /// The mesh's name.
        mesh: String,
        /// Why.
        message: String,
    },
    /// A node's camera, or a scene other than the default, was skipped.
    Node {
        /// The node's or scene's name.
        node: String,
        /// Why.
        message: String,
    },
}

impl core::fmt::Display for ImportWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ImportWarning::UnsupportedExtension { name } => {
                write!(f, "extension `{name}` is not supported and was ignored")
            }
            ImportWarning::Material { material, message } => {
                write!(f, "material `{material}`: {message}")
            }
            ImportWarning::Image { image, message } => write!(f, "image `{image}`: {message}"),
            ImportWarning::Mesh { mesh, message } => write!(f, "mesh `{mesh}`: {message}"),
            ImportWarning::Node { node, message } => write!(f, "`{node}`: {message}"),
        }
    }
}

/// A file that could not be imported at all.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportError {
    /// The file, or `<bytes>` for an import from memory.
    pub path: String,
    /// Why.
    pub reason: String,
}

impl core::fmt::Display for ImportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cannot import `{}`: {}", self.path, self.reason)
    }
}

impl std::error::Error for ImportError {}

/// Import a `.gltf` or `.glb` file. Its buffers and file images are
/// resolved against its own directory; the generated graphs use
/// `registry`'s stdlib nodes.
pub fn import(
    path: impl AsRef<Path>,
    registry: &NodeRegistry,
) -> Result<ImportedScene, ImportError> {
    let path = path.as_ref();
    let failed = |reason: String| ImportError {
        path: path.display().to_string(),
        reason,
    };
    let gltf::Gltf { document, blob } =
        gltf::Gltf::open(path).map_err(|error| failed(error.to_string()))?;
    let directory = path.parent().map(Path::to_path_buf);
    let buffers = gltf::import_buffers(&document, directory.as_deref(), blob)
        .map_err(|error| failed(error.to_string()))?;
    Ok(Importer::new(&document, &buffers, registry, file_stem(path)).run(directory))
}

/// Import a GLB (or a self-contained `.gltf`) from memory, for hosts
/// with no file system. An external buffer is an error; an external image
/// stays a `file` texture with no directory to resolve it against.
pub fn import_slice(bytes: &[u8], registry: &NodeRegistry) -> Result<ImportedScene, ImportError> {
    let failed = |reason: String| ImportError {
        path: "<bytes>".to_string(),
        reason,
    };
    let gltf::Gltf { document, blob } =
        gltf::Gltf::from_slice(bytes).map_err(|error| failed(error.to_string()))?;
    let buffers =
        gltf::import_buffers(&document, None, blob).map_err(|error| failed(error.to_string()))?;
    Ok(Importer::new(&document, &buffers, registry, "gltf".to_string()).run(None))
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "gltf".to_string())
}

struct Importer<'a> {
    document: &'a gltf::Document,
    buffers: &'a [gltf::buffer::Data],
    registry: &'a NodeRegistry,
    scene: Scene,
    meshes: Vec<Geometry>,
    images: Vec<Vec<u8>>,
    warnings: Vec<ImportWarning>,
    /// Per glTF mesh: its primitives' scene meshes and glTF materials.
    mesh_primitives: BTreeMap<usize, Vec<(usize, gltf::Material<'a>)>>,
    /// Per glTF material (`None`: the default material): its entry.
    materials: BTreeMap<Option<usize>, usize>,
    /// Per (glTF texture, colour space): its entry, or `None` when its
    /// image is unusable.
    textures: BTreeMap<(usize, bool), Option<usize>>,
    /// Per glTF image: its source, or why it is unusable.
    sources: BTreeMap<usize, Result<TextureSource, String>>,
}

impl<'a> Importer<'a> {
    fn new(
        document: &'a gltf::Document,
        buffers: &'a [gltf::buffer::Data],
        registry: &'a NodeRegistry,
        name: String,
    ) -> Self {
        Importer {
            document,
            buffers,
            registry,
            scene: Scene::new(name),
            meshes: Vec::new(),
            images: Vec::new(),
            warnings: Vec::new(),
            mesh_primitives: BTreeMap::new(),
            materials: BTreeMap::new(),
            textures: BTreeMap::new(),
            sources: BTreeMap::new(),
        }
    }

    fn run(mut self, directory: Option<PathBuf>) -> ImportedScene {
        for name in self.document.extensions_used() {
            if !MAPPED_EXTENSIONS.contains(&name) {
                self.warnings.push(ImportWarning::UnsupportedExtension {
                    name: name.to_string(),
                });
            }
        }

        let mut environment = SceneEnvironment::default();
        let default = self
            .document
            .default_scene()
            .or_else(|| self.document.scenes().next());
        for other in self.document.scenes() {
            if Some(other.index()) != default.as_ref().map(gltf::Scene::index) {
                self.warnings.push(ImportWarning::Node {
                    node: other.name().unwrap_or("scene").to_string(),
                    message: "only the default scene is imported".to_string(),
                });
            }
        }
        if let Some(scene) = default {
            for node in scene.nodes() {
                self.visit(&node, None, &mut environment);
            }
        }
        if !environment.lights.is_empty() || !environment.cameras.is_empty() {
            self.scene.environment = Some(environment);
        }
        self.fit_lighting_budget();

        ImportedScene {
            scene: self.scene,
            meshes: self.meshes,
            images: self.images,
            directory,
            warnings: self.warnings,
        }
    }

    /// A layered model next to the default one can overflow the G-buffer
    /// budget (the sheen and iridescent models fill it alone, ADRs 0058 and
    /// 0059). When exactly one layered model is used and the pair does not
    /// fit, the plain materials take that model too: with its layer at zero
    /// it shades as the default model does.
    fn fit_lighting_budget(&mut self) {
        let named: std::collections::BTreeSet<&str> = self
            .scene
            .materials
            .iter()
            .filter_map(|material| material.config.model.as_deref())
            .collect();
        let plain = self
            .scene
            .materials
            .iter()
            .any(|material| material.config.model.is_none());
        let fits = |names: &[&str]| {
            LightingSet::new(
                DEFAULT_MODELS
                    .iter()
                    .copied()
                    .filter(|model| names.contains(&model.name)),
            )
            .is_ok_and(|set| {
                wxsl_frame::pipeline::gbuffer_bytes_per_sample(&set)
                    <= wxsl_frame::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE
            })
        };
        let default = DEFAULT_MODELS[DEFAULT_MODEL_ID as usize].name;
        let [layered] = named.iter().copied().collect::<Vec<_>>()[..] else {
            return;
        };
        if !plain || fits(&[default, layered]) || !fits(&[layered]) {
            return;
        }
        let layered = layered.to_string();
        for material in &mut self.scene.materials {
            if material.config.model.is_none() {
                material.config.model = Some(layered.clone());
                self.warnings.push(ImportWarning::Material {
                    material: material.name.clone(),
                    message: format!(
                        "shaded by `{layered}` with its layer off: `{default}` and `{layered}` \
                         together exceed the G-buffer budget"
                    ),
                });
            }
        }
    }

    /// Add `node` and its subtree, parents first.
    fn visit(
        &mut self,
        node: &gltf::Node<'a>,
        parent: Option<usize>,
        environment: &mut SceneEnvironment,
    ) {
        let name = node
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("node {}", node.index()));
        let mut entry =
            SceneNode::new(name.clone()).with_transform(flatten(node.transform().matrix()));
        if let Some(parent) = parent {
            entry = entry.with_parent(parent);
        }
        let index = self.scene.add_node(entry);

        if let Some(mesh) = node.mesh() {
            for (primitive, material) in self.primitives(&mesh) {
                let material = self.material(&material);
                self.scene.add_instance(
                    Instance::new(primitive, material)
                        .with_node(index)
                        .with_name(name.clone()),
                );
            }
        }
        if let Some(light) = node.light() {
            environment.lights.push(scene_light(&light, index, &name));
        }
        if let Some(camera) = node.camera() {
            match camera.projection() {
                gltf::camera::Projection::Perspective(perspective) => {
                    let mut entry =
                        SceneCamera::new(camera.name().map(str::to_string).unwrap_or(name.clone()))
                            .with_node(index);
                    entry.fov_y = perspective.yfov();
                    entry.near = perspective.znear();
                    if let Some(far) = perspective.zfar() {
                        entry.far = far;
                    }
                    environment.cameras.push(entry);
                }
                gltf::camera::Projection::Orthographic(_) => {
                    self.warnings.push(ImportWarning::Node {
                        node: name.clone(),
                        message: "orthographic cameras are not supported".to_string(),
                    });
                }
            }
        }
        for child in node.children() {
            self.visit(&child, Some(index), environment);
        }
    }

    /// The scene meshes of `mesh`'s triangle primitives, each with its glTF
    /// material, read on first use.
    fn primitives(&mut self, mesh: &gltf::Mesh<'a>) -> Vec<(usize, gltf::Material<'a>)> {
        if let Some(known) = self.mesh_primitives.get(&mesh.index()) {
            return known.clone();
        }
        let name = mesh
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("mesh {}", mesh.index()));
        let mut read = Vec::new();
        for primitive in mesh.primitives() {
            match geometry::read_primitive(&primitive, glam::Mat4::IDENTITY, self.buffers) {
                Some(data) => {
                    let index = self.meshes.len();
                    self.meshes.push(data);
                    let entry = self.scene.add_mesh(MeshEntry::new(
                        format!("{name}/{}", primitive.index()),
                        MeshSource::Embedded { index },
                    ));
                    read.push((entry, primitive.material()));
                }
                None => self.warnings.push(ImportWarning::Mesh {
                    mesh: name.clone(),
                    message: format!(
                        "primitive {} is not triangles with positions ({:?}); skipped",
                        primitive.index(),
                        primitive.mode()
                    ),
                }),
            }
        }
        self.mesh_primitives.insert(mesh.index(), read.clone());
        read
    }

    /// The material entry for `material`, built on first use. A primitive
    /// with none has the spec's default material (white, metallic 1,
    /// roughness 1), whose index is `None`.
    fn material(&mut self, material: &gltf::Material<'a>) -> usize {
        if let Some(&entry) = self.materials.get(&material.index()) {
            return entry;
        }
        let entry = self.build_material(material);
        let entry = self.scene.add_material(entry);
        self.materials.insert(material.index(), entry);
        entry
    }

    fn build_material(&mut self, material: &gltf::Material<'_>) -> MaterialEntry {
        let name = material
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| match material.index() {
                Some(index) => format!("material {index}"),
                None => "default".to_string(),
            });
        let warn = |warnings: &mut Vec<ImportWarning>, message: String| {
            warnings.push(ImportWarning::Material {
                material: name.clone(),
                message,
            })
        };
        let mut graph = PbrGraph::new(self.registry);
        let mut bindings: Vec<(String, usize)> = Vec::new();
        let mut tags = vec![TAG_OPAQUE];
        let mut model = None;

        let pbr = material.pbr_metallic_roughness();
        let mut base_factor = pbr.base_color_factor();

        // Transmission and volume: the refraction tier (ADR 0067).
        let mut refractive = None;
        if let Some(transmission) = material.transmission() {
            if transmission.transmission_factor() > 0.0 {
                if transmission.transmission_factor() < 1.0 {
                    warn(
                        &mut self.warnings,
                        format!(
                            "transmission factor {} is drawn as clear glass",
                            transmission.transmission_factor()
                        ),
                    );
                }
                if transmission.transmission_texture().is_some() {
                    warn(
                        &mut self.warnings,
                        "transmissionTexture ignored".to_string(),
                    );
                }
                let mut distance = 1e9;
                if let Some(volume) = material.volume() {
                    if volume.attenuation_distance().is_finite() {
                        distance = volume.attenuation_distance();
                    }
                    let tint = volume.attenuation_color();
                    for (channel, tint) in base_factor.iter_mut().zip(tint) {
                        *channel *= tint;
                    }
                    if volume.thickness_texture().is_some() {
                        warn(&mut self.warnings, "thicknessTexture ignored".to_string());
                    }
                }
                refractive = Some(distance);
                tags = vec![TAG_REFRACTIVE];
            }
        }

        // Base colour and alpha.
        let base = match self.texture_ref(pbr.base_color_texture().as_ref(), true) {
            Ok(Some((slot_texture, info))) => {
                bindings.push(("base_color_texture".to_string(), slot_texture));
                Some(graph.sample("base_color_texture", &info))
            }
            Ok(None) => None,
            Err(message) => {
                warn(&mut self.warnings, format!("baseColorTexture: {message}"));
                None
            }
        };
        let factor = graph.param("base_color_factor", Value::Vec4(base_factor));
        let color = match base {
            Some(sample) => graph.multiply((sample, "out"), (factor, "out"), ValueType::Vec4),
            None => (factor, "out"),
        };
        let rgba = graph.node("convert.split.vec4f");
        graph.wire(color, (rgba, "v"));
        let rgb = graph.node("convert.combine.vec3f");
        for channel in ["x", "y", "z"] {
            graph.wire((rgba, channel), (rgb, channel));
        }
        graph.output((rgb, "out"), "base_color");
        match material.alpha_mode() {
            AlphaMode::Opaque => {}
            AlphaMode::Mask => {
                let cutoff = graph.param(
                    "alpha_cutoff",
                    Value::F32(material.alpha_cutoff().unwrap_or(0.5)),
                );
                let less = graph.node("compare.less");
                graph.wire((rgba, "w"), (less, "a"));
                graph.wire((cutoff, "out"), (less, "b"));
                let discard = graph.node("output.discard");
                graph.wire((less, "out"), (discard, "discard"));
            }
            AlphaMode::Blend => {
                graph.output((rgba, "w"), "alpha");
                if refractive.is_none() {
                    tags = vec![TAG_TRANSPARENT];
                }
            }
        }

        // Metallic and roughness: blue and green of one texture.
        let metallic = graph.param("metallic_factor", Value::F32(pbr.metallic_factor()));
        let roughness = graph.param("roughness_factor", Value::F32(pbr.roughness_factor()));
        match self.texture_ref(pbr.metallic_roughness_texture().as_ref(), false) {
            Ok(Some((texture, info))) => {
                bindings.push(("metallic_roughness_texture".to_string(), texture));
                let sample = graph.sample("metallic_roughness_texture", &info);
                let channels = graph.node("convert.split.vec4f");
                graph.wire((sample, "out"), (channels, "v"));
                let m = graph.multiply((channels, "z"), (metallic, "out"), ValueType::F32);
                let r = graph.multiply((channels, "y"), (roughness, "out"), ValueType::F32);
                graph.output(m, "metallic");
                graph.output(r, "roughness");
            }
            other => {
                if let Err(message) = other {
                    warn(
                        &mut self.warnings,
                        format!("metallicRoughnessTexture: {message}"),
                    );
                }
                graph.output((metallic, "out"), "metallic");
                graph.output((roughness, "out"), "roughness");
            }
        }

        // Normal map: tangent space, scaled in x and y.
        if let Some(normal) = material.normal_texture() {
            let info = TexInfo::raw(
                normal.texture(),
                normal.tex_coord(),
                normal.extension_value("KHR_texture_transform"),
            );
            match self.texture_slot(&info, false) {
                Ok(texture) => {
                    bindings.push(("normal_texture".to_string(), texture));
                    let mapped = graph.normal_map("normal", &info, normal.scale());
                    graph.output(mapped, "normal");
                }
                Err(message) => warn(&mut self.warnings, format!("normalTexture: {message}")),
            }
        }

        // Occlusion: red, blended toward 1 by strength.
        if let Some(occlusion) = material.occlusion_texture() {
            let info = TexInfo::raw(
                occlusion.texture(),
                occlusion.tex_coord(),
                occlusion.extension_value("KHR_texture_transform"),
            );
            match self.texture_slot(&info, false) {
                Ok(texture) => {
                    bindings.push(("occlusion_texture".to_string(), texture));
                    let sample = graph.sample("occlusion_texture", &info);
                    let channels = graph.node("convert.split.vec4f");
                    graph.wire((sample, "out"), (channels, "v"));
                    let strength =
                        graph.param("occlusion_strength", Value::F32(occlusion.strength()));
                    let mix = graph.node("math.mix");
                    graph.generic(mix, "T", ValueType::F32);
                    graph.set(mix, "a", Value::F32(1.0));
                    graph.wire((channels, "x"), (mix, "b"));
                    graph.wire((strength, "out"), (mix, "t"));
                    graph.output((mix, "out"), "occlusion");
                }
                Err(message) => warn(&mut self.warnings, format!("occlusionTexture: {message}")),
            }
        }

        // Emissive, with KHR_materials_emissive_strength folded into the
        // factor.
        let strength = material.emissive_strength().unwrap_or(1.0);
        let emissive = material.emissive_factor().map(|channel| channel * strength);
        let emissive_texture = match self.texture_ref(material.emissive_texture().as_ref(), true) {
            Ok(found) => found,
            Err(message) => {
                warn(&mut self.warnings, format!("emissiveTexture: {message}"));
                None
            }
        };
        if emissive_texture.is_some() || emissive != [0.0; 3] {
            let factor = graph.param("emissive_factor", Value::Vec3(emissive));
            let radiance = match emissive_texture {
                Some((texture, info)) => {
                    bindings.push(("emissive_texture".to_string(), texture));
                    let sample = graph.sample("emissive_texture", &info);
                    let channels = graph.node("convert.split.vec4f");
                    graph.wire((sample, "out"), (channels, "v"));
                    let rgb = graph.node("convert.combine.vec3f");
                    for channel in ["x", "y", "z"] {
                        graph.wire((channels, channel), (rgb, channel));
                    }
                    graph.multiply((rgb, "out"), (factor, "out"), ValueType::Vec3)
                }
                None => (factor, "out"),
            };
            graph.output(radiance, "emissive");
        }

        if let Some(ior) = material.ior() {
            let ior = graph.param("ior", Value::F32(ior));
            graph.output((ior, "out"), "ior");
        }
        if let Some(distance) = refractive {
            let distance = graph.param("attenuation_distance", Value::F32(distance));
            graph.output((distance, "out"), "attenuation_distance");
        }

        // The layered extensions each select a lighting model; a material
        // has one model, so the first one present wins.
        let choose = |model: &mut Option<&'static str>,
                      warnings: &mut Vec<ImportWarning>,
                      extension: &str,
                      wanted: &'static str| {
            match model {
                None => {
                    *model = Some(wanted);
                    true
                }
                Some(chosen) => {
                    warnings.push(ImportWarning::Material {
                        material: name.clone(),
                        message: format!("{extension} ignored: the material is already `{chosen}`"),
                    });
                    false
                }
            }
        };
        if let Some(iridescence) = material.extension_value("KHR_materials_iridescence") {
            let factor = number(iridescence, "iridescenceFactor", 0.0);
            if factor > 0.0
                && choose(
                    &mut model,
                    &mut self.warnings,
                    "KHR_materials_iridescence",
                    "wxsl.iridescent",
                )
            {
                for (slot, param, value) in [
                    ("iridescence_strength", "iridescence_factor", factor),
                    (
                        "iridescence_ior",
                        "iridescence_ior",
                        number(iridescence, "iridescenceIor", 1.3),
                    ),
                    (
                        "iridescence_thickness",
                        "iridescence_thickness",
                        number(iridescence, "iridescenceThicknessMaximum", 400.0),
                    ),
                ] {
                    let node = graph.param(param, Value::F32(value));
                    graph.output((node, "out"), slot);
                }
                for texture in ["iridescenceTexture", "iridescenceThicknessTexture"] {
                    if iridescence.get(texture).is_some() {
                        warn(&mut self.warnings, format!("{texture} ignored"));
                    }
                }
            }
        }
        if let Some(clearcoat) = material.extension_value("KHR_materials_clearcoat") {
            let factor = number(clearcoat, "clearcoatFactor", 0.0);
            if factor > 0.0
                && choose(
                    &mut model,
                    &mut self.warnings,
                    "KHR_materials_clearcoat",
                    "wxsl.gltf_clearcoat",
                )
            {
                // Strength in the red of its texture, roughness in the
                // green of its own; each times its factor.
                for (field, value, key, channel) in [
                    ("clearcoat", factor, "clearcoatTexture", "x"),
                    (
                        "clearcoat_roughness",
                        number(clearcoat, "clearcoatRoughnessFactor", 0.0),
                        "clearcoatRoughnessTexture",
                        "y",
                    ),
                ] {
                    let factor = graph.param(&format!("{field}_factor"), Value::F32(value));
                    let out = match self.extension_texture(clearcoat.get(key)) {
                        Ok(Some((texture, info))) => {
                            let slot = format!("{field}_texture");
                            bindings.push((slot.clone(), texture));
                            let sample = graph.sample(&slot, &info);
                            let channels = graph.node("convert.split.vec4f");
                            graph.wire((sample, "out"), (channels, "v"));
                            graph.multiply((channels, channel), (factor, "out"), ValueType::F32)
                        }
                        Ok(None) => (factor, "out"),
                        Err(message) => {
                            warn(&mut self.warnings, format!("{key}: {message}"));
                            (factor, "out")
                        }
                    };
                    graph.output(out, field);
                }
                let normal = clearcoat.get("clearcoatNormalTexture");
                match self.extension_texture(normal) {
                    Ok(Some((texture, info))) => {
                        bindings.push(("clearcoat_normal_texture".to_string(), texture));
                        let scale = normal.map_or(1.0, |normal| number(normal, "scale", 1.0));
                        let mapped = graph.normal_map("clearcoat_normal", &info, scale);
                        graph.output(mapped, "clearcoat_normal");
                    }
                    Ok(None) => {}
                    Err(message) => warn(
                        &mut self.warnings,
                        format!("clearcoatNormalTexture: {message}"),
                    ),
                }
            }
        }
        if let Some(sheen) = material.extension_value("KHR_materials_sheen") {
            let color = vec3(sheen, "sheenColorFactor", [0.0; 3]);
            if color != [0.0; 3]
                && choose(
                    &mut model,
                    &mut self.warnings,
                    "KHR_materials_sheen",
                    "wxsl.sheen",
                )
            {
                let color = graph.param("sheen_color_factor", Value::Vec3(color));
                graph.output((color, "out"), "sheen_color");
                let roughness = graph.param(
                    "sheen_roughness_factor",
                    Value::F32(number(sheen, "sheenRoughnessFactor", 0.0)),
                );
                graph.output((roughness, "out"), "sheen_roughness");
                for texture in ["sheenColorTexture", "sheenRoughnessTexture"] {
                    if sheen.get(texture).is_some() {
                        warn(&mut self.warnings, format!("{texture} ignored"));
                    }
                }
            }
        }

        if material.double_sided() {
            tags.push(TAG_DOUBLE_SIDED);
        }

        let mut entry =
            MaterialEntry::new(name.clone(), graph.finish()).with_tags(Tags::from_iter(tags));
        entry.config.double_sided = material.double_sided();
        if let Some(model) = model {
            entry = entry.with_lighting(model);
        }
        for (slot, texture) in bindings {
            entry = entry
                .with_texture(format!("{slot}_sampler"), texture)
                .with_texture(slot, texture);
        }
        entry
    }

    /// A core texture reference: its scene texture and how it is read.
    fn texture_ref(
        &mut self,
        info: Option<&gltf::texture::Info<'_>>,
        srgb: bool,
    ) -> Result<Option<(usize, TexInfo)>, String> {
        let Some(info) = info else { return Ok(None) };
        let transform = info.texture_transform().map(|transform| UvTransform {
            offset: transform.offset(),
            rotation: transform.rotation(),
            scale: transform.scale(),
            tex_coord: transform.tex_coord(),
        });
        let info = TexInfo {
            texture: info.texture().index(),
            tex_coord: transform
                .and_then(|transform| transform.tex_coord)
                .unwrap_or(info.tex_coord()),
            transform,
        };
        let texture = self.texture_slot(&info, srgb)?;
        Ok(Some((texture, info)))
    }

    /// An extension's texture reference, which exists only as JSON: its
    /// scene texture (linear) and how it is read.
    fn extension_texture(
        &mut self,
        json: Option<&Json>,
    ) -> Result<Option<(usize, TexInfo)>, String> {
        let Some(json) = json else { return Ok(None) };
        let index = json
            .get("index")
            .and_then(Json::as_u64)
            .ok_or("no texture index")? as usize;
        let texture = self
            .document
            .textures()
            .nth(index)
            .ok_or_else(|| format!("no texture {index}"))?;
        let info = TexInfo::raw(
            texture,
            json.get("texCoord").and_then(Json::as_u64).unwrap_or(0) as u32,
            json.get("extensions")
                .and_then(|extensions| extensions.get("KHR_texture_transform")),
        );
        let texture = self.texture_slot(&info, false)?;
        Ok(Some((texture, info)))
    }

    /// The scene texture for `info` in the given colour space, created on
    /// first use.
    fn texture_slot(&mut self, info: &TexInfo, srgb: bool) -> Result<usize, String> {
        if info.tex_coord > 1 {
            return Err(format!(
                "TEXCOORD_{} is not imported; only sets 0 and 1 are",
                info.tex_coord
            ));
        }
        if let Some(known) = self.textures.get(&(info.texture, srgb)) {
            return known.ok_or_else(|| "its image is unusable".to_string());
        }
        let texture = self
            .document
            .textures()
            .nth(info.texture)
            .ok_or_else(|| format!("no texture {}", info.texture))?;
        let image = texture.source();
        let source = match self.sources.get(&image.index()) {
            Some(source) => source.clone(),
            None => {
                let source = self.image_source(&image);
                if let Err(message) = &source {
                    self.warnings.push(ImportWarning::Image {
                        image: image
                            .name()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("image {}", image.index())),
                        message: message.clone(),
                    });
                }
                self.sources.insert(image.index(), source.clone());
                source
            }
        };
        let Ok(source) = source else {
            self.textures.insert((info.texture, srgb), None);
            return Err("its image is unusable".to_string());
        };
        let name = texture
            .name()
            .or_else(|| image.name())
            .map(str::to_string)
            .unwrap_or_else(|| format!("texture {}", texture.index()));
        let mut entry = TextureEntry::new(name, source).with_color_space(if srgb {
            ColorSpace::Srgb
        } else {
            ColorSpace::Linear
        });
        entry.sampler = sampler(&texture.sampler());
        let index = self.scene.add_texture(entry);
        self.textures.insert((info.texture, srgb), Some(index));
        Ok(index)
    }

    /// Where `image`'s bytes are, or why they cannot be used.
    fn image_source(&mut self, image: &gltf::Image<'_>) -> Result<TextureSource, String> {
        match image.source() {
            gltf::image::Source::View { view, mime_type } => {
                supported(mime_type)?;
                let buffer = &self.buffers[view.buffer().index()];
                let bytes = buffer
                    .get(view.offset()..view.offset() + view.length())
                    .ok_or("its buffer view is out of range")?;
                Ok(self.embed(bytes.to_vec()))
            }
            gltf::image::Source::Uri { uri, mime_type } => {
                if let Some(data) = uri.strip_prefix("data:") {
                    let (header, payload) = data.split_once(',').ok_or("malformed data URI")?;
                    supported(header.split(';').next().unwrap_or(""))?;
                    if !header.ends_with(";base64") {
                        return Err("a data URI that is not base64".to_string());
                    }
                    Ok(self.embed(base64(payload)?))
                } else {
                    let path = percent_decode(uri);
                    let mime = mime_type.unwrap_or(
                        match Path::new(&path).extension().and_then(|ext| ext.to_str()) {
                            Some("png") => "image/png",
                            Some("jpg" | "jpeg") => "image/jpeg",
                            _ => "",
                        },
                    );
                    supported(mime)?;
                    Ok(TextureSource::File { path })
                }
            }
        }
    }

    fn embed(&mut self, bytes: Vec<u8>) -> TextureSource {
        self.images.push(bytes);
        TextureSource::Embedded {
            index: self.images.len() - 1,
        }
    }
}

fn supported(mime: &str) -> Result<(), String> {
    match mime {
        "image/png" | "image/jpeg" => Ok(()),
        "" => Err("its format is unknown".to_string()),
        other => Err(format!("`{other}` is not supported; only PNG and JPEG are")),
    }
}

fn sampler(sampler: &gltf::texture::Sampler<'_>) -> SamplerEntry {
    let wrap = |mode| match mode {
        WrappingMode::ClampToEdge => Wrap::ClampToEdge,
        WrappingMode::MirroredRepeat => Wrap::MirroredRepeat,
        WrappingMode::Repeat => Wrap::Repeat,
    };
    let (min, mipmap) = match sampler.min_filter() {
        Some(MinFilter::Nearest | MinFilter::NearestMipmapNearest) => {
            (Filter::Nearest, Filter::Nearest)
        }
        Some(MinFilter::NearestMipmapLinear) => (Filter::Nearest, Filter::Linear),
        Some(MinFilter::Linear | MinFilter::LinearMipmapNearest) => {
            (Filter::Linear, Filter::Nearest)
        }
        Some(MinFilter::LinearMipmapLinear) | None => (Filter::Linear, Filter::Linear),
    };
    SamplerEntry {
        mag: match sampler.mag_filter() {
            Some(MagFilter::Nearest) => Filter::Nearest,
            Some(MagFilter::Linear) | None => Filter::Linear,
        },
        min,
        mipmap,
        wrap_u: wrap(sampler.wrap_s()),
        wrap_v: wrap(sampler.wrap_t()),
    }
}

fn scene_light(
    light: &gltf::khr_lights_punctual::Light<'_>,
    node: usize,
    name: &str,
) -> SceneLight {
    use gltf::khr_lights_punctual::Kind;
    let kind = match light.kind() {
        Kind::Directional => SceneLightKind::Directional,
        Kind::Point => SceneLightKind::Point,
        Kind::Spot {
            inner_cone_angle,
            outer_cone_angle,
        } => SceneLightKind::Spot {
            inner_angle: inner_cone_angle,
            outer_angle: outer_cone_angle,
        },
    };
    let mut entry = SceneLight::new(light.name().unwrap_or(name), kind).with_node(node);
    entry.color = light.color();
    entry.intensity = light.intensity();
    entry.range = light.range().unwrap_or(0.0);
    entry
}

/// A texture read: which texture, which UV set, and its transform.
#[derive(Clone, Copy, Debug)]
struct TexInfo {
    texture: usize,
    tex_coord: u32,
    transform: Option<UvTransform>,
}

impl TexInfo {
    /// From a reference whose `KHR_texture_transform` only exists as JSON
    /// (normal and occlusion textures).
    fn raw(texture: gltf::Texture<'_>, tex_coord: u32, transform: Option<&Json>) -> Self {
        let transform = transform.map(|json| UvTransform {
            offset: vec2(json, "offset", [0.0, 0.0]),
            rotation: number(json, "rotation", 0.0),
            scale: vec2(json, "scale", [1.0, 1.0]),
            tex_coord: json
                .get("texCoord")
                .and_then(Json::as_u64)
                .map(|set| set as u32),
        });
        TexInfo {
            texture: texture.index(),
            tex_coord: transform
                .and_then(|transform| transform.tex_coord)
                .unwrap_or(tex_coord),
            transform,
        }
    }
}

/// `KHR_texture_transform`: `uv' = offset + rotation * (scale * uv)`.
#[derive(Clone, Copy, Debug)]
struct UvTransform {
    offset: [f32; 2],
    rotation: f32,
    scale: [f32; 2],
    tex_coord: Option<u32>,
}

impl UvTransform {
    fn is_identity(&self) -> bool {
        self.offset == [0.0, 0.0] && self.rotation == 0.0 && self.scale == [1.0, 1.0]
    }
}

/// The `gltf_pbr` graph under construction.
struct PbrGraph<'r> {
    graph: Graph,
    registry: &'r NodeRegistry,
    surface: NodeId,
    uv: [Option<NodeId>; 2],
}

impl<'r> PbrGraph<'r> {
    fn new(registry: &'r NodeRegistry) -> Self {
        let mut graph = Graph::new(GLTF_PBR_GRAPH);
        let surface = graph.add_resolved(registry, Node::new("output.surface"));
        PbrGraph {
            graph,
            registry,
            surface,
            uv: [None, None],
        }
    }

    fn finish(self) -> Graph {
        self.graph
    }

    fn node(&mut self, def: &str) -> NodeId {
        self.graph.add_resolved(self.registry, Node::new(def))
    }

    /// Every edge here joins sockets whose types this module chose, so a
    /// refusal is a bug in the importer, caught by its tests.
    fn wire(&mut self, from: (NodeId, &str), to: (NodeId, &str)) {
        if let Err(error) = self.graph.wire(self.registry, from, to) {
            panic!("gltf_pbr wiring {from:?} -> {to:?}: {error}");
        }
    }

    fn set(&mut self, node: NodeId, socket: &str, value: Value) {
        self.graph.set_param(node, socket, value);
    }

    fn generic(&mut self, node: NodeId, param: &str, ty: ValueType) {
        if let Err(error) = self.graph.set_generic(self.registry, node, param, ty) {
            panic!("gltf_pbr generic {param} = {ty:?}: {error}");
        }
    }

    fn generics(&mut self, node: NodeId, a: ValueType, b: ValueType) {
        self.generic(node, "A", a);
        self.generic(node, "B", b);
    }

    fn output(&mut self, from: (NodeId, &str), field: &str) {
        let surface = self.surface;
        self.wire(from, (surface, field));
    }

    /// A parameter named `name`, starting at `value`.
    fn param(&mut self, name: &str, value: Value) -> NodeId {
        let node = self.graph.add_resolved(
            self.registry,
            Node::new("param.value").with_setting(SETTING_NAME, name),
        );
        self.generic(node, "T", value.ty());
        self.set(node, "value", value);
        node
    }

    /// `a * b`, typed `ty` on both sides.
    fn multiply(
        &mut self,
        a: (NodeId, &str),
        b: (NodeId, &str),
        ty: ValueType,
    ) -> (NodeId, &'static str) {
        let node = self.node("math.multiply");
        self.generics(node, ty, ty);
        self.wire(a, (node, "a"));
        self.wire(b, (node, "b"));
        (node, "out")
    }

    /// A tangent-space normal map over the geometric frame, sampled from
    /// slot `<name>_texture` and scaled in x and y by param `<name>_scale`.
    fn normal_map(&mut self, name: &str, info: &TexInfo, scale: f32) -> (NodeId, &'static str) {
        let sample = self.sample(&format!("{name}_texture"), info);
        let channels = self.node("convert.split.vec4f");
        self.wire((sample, "out"), (channels, "v"));
        let encoded = self.node("convert.combine.vec3f");
        for channel in ["x", "y", "z"] {
            self.wire((channels, channel), (encoded, channel));
        }
        let doubled = self.node("math.multiply");
        self.generics(doubled, ValueType::Vec3, ValueType::F32);
        self.wire((encoded, "out"), (doubled, "a"));
        self.set(doubled, "b", Value::F32(2.0));
        let decoded = self.node("math.subtract");
        self.generics(decoded, ValueType::Vec3, ValueType::F32);
        self.wire((doubled, "out"), (decoded, "a"));
        self.set(decoded, "b", Value::F32(1.0));
        let scale = self.param(&format!("{name}_scale"), Value::F32(scale));
        let mapped = self.node("space.apply_normal_map");
        self.wire((decoded, "out"), (mapped, "tangent_normal"));
        for (input, socket) in [
            ("input.world_normal", "normal"),
            ("input.world_tangent", "tangent"),
            ("input.world_bitangent", "bitangent"),
        ] {
            let read = self.node(input);
            self.wire((read, "out"), (mapped, socket));
        }
        self.wire((scale, "out"), (mapped, "strength"));
        (mapped, "out")
    }

    /// Sample texture slot `slot` (and its sampler, `<slot>_sampler`)
    /// where `info` says.
    fn sample(&mut self, slot: &str, info: &TexInfo) -> NodeId {
        let texture = self.graph.add_resolved(
            self.registry,
            Node::new("texture.texture_2d").with_setting(SETTING_NAME, slot),
        );
        let sampler = self.graph.add_resolved(
            self.registry,
            Node::new("texture.sampler").with_setting(SETTING_NAME, format!("{slot}_sampler")),
        );
        let uv = self.uv(info);
        let sample = self.node("sample.texture_2d");
        self.wire((texture, "out"), (sample, "tex"));
        self.wire((sampler, "out"), (sample, "samp"));
        self.wire(uv, (sample, "uv"));
        sample
    }

    /// The UV set `info` reads, transformed if it says so.
    fn uv(&mut self, info: &TexInfo) -> (NodeId, &'static str) {
        let set = info.tex_coord.min(1) as usize;
        let base = match self.uv[set] {
            Some(node) => node,
            None => {
                let node = if set == 0 {
                    self.node("input.uv")
                } else {
                    self.graph
                        .declare_attribute(AttributeDecl::vertex(UV1_ATTRIBUTE, ValueType::Vec2));
                    let node = self.graph.add_resolved(
                        self.registry,
                        Node::new("input.attribute").with_setting(SETTING_NAME, UV1_ATTRIBUTE),
                    );
                    self.generic(node, "T", ValueType::Vec2);
                    node
                };
                self.uv[set] = Some(node);
                node
            }
        };
        let Some(transform) = info.transform.filter(|transform| !transform.is_identity()) else {
            return (base, "out");
        };
        // Rows of rotation * scale, each dotted with the UV.
        let (sin, cos) = transform.rotation.sin_cos();
        let [sx, sy] = transform.scale;
        let combined = self.node("convert.combine.vec2f");
        for (row, component) in [([cos * sx, -sin * sy], "x"), ([sin * sx, cos * sy], "y")] {
            let dot = self.node("vector.dot");
            self.generic(dot, "T", ValueType::Vec2);
            self.wire((base, "out"), (dot, "a"));
            self.set(dot, "b", Value::Vec2(row));
            self.wire((dot, "out"), (combined, component));
        }
        let offset = self.node("math.add");
        self.generics(offset, ValueType::Vec2, ValueType::Vec2);
        self.wire((combined, "out"), (offset, "a"));
        self.set(offset, "b", Value::Vec2(transform.offset));
        (offset, "out")
    }
}

/// A glTF column-major 4×4 as the document's flat column-major array.
fn flatten(matrix: [[f32; 4]; 4]) -> [f32; 16] {
    let mut flat = [0.0; 16];
    for (column, values) in matrix.iter().enumerate() {
        flat[column * 4..column * 4 + 4].copy_from_slice(values);
    }
    flat
}

fn number(json: &Json, key: &str, default: f32) -> f32 {
    json.get(key)
        .and_then(Json::as_f64)
        .map_or(default, |value| value as f32)
}

fn vec2(json: &Json, key: &str, default: [f32; 2]) -> [f32; 2] {
    let mut out = default;
    if let Some(values) = json.get(key).and_then(Json::as_array) {
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value.as_f64().map_or(*slot, |value| value as f32);
        }
    }
    out
}

fn vec3(json: &Json, key: &str, default: [f32; 3]) -> [f32; 3] {
    let mut out = default;
    if let Some(values) = json.get(key).and_then(Json::as_array) {
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value.as_f64().map_or(*slot, |value| value as f32);
        }
    }
    out
}

/// `%XX` escapes in a relative URI.
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| uri.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64, padding optional.
fn base64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for byte in text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
    {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err("invalid base64 in a data URI".to_string()),
        };
        bits = bits << 6 | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_percent_escapes_decode() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8").unwrap(), b"hello");
        assert_eq!(percent_decode("my%20file.png"), "my file.png");
        assert_eq!(percent_decode("100%.png"), "100%.png");
    }
}
