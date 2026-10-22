//! Safe request handlers. All planning, layouts and compilation delegate to shared crates.

use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::codegen::{self, CodegenOptions};
use wxsl_core::graph::Graph;
use wxsl_core::identity;
use wxsl_core::lighting::{self, LightingSet};
use wxsl_core::macros::MacroSet;
use wxsl_core::material::MaterialConfig;
use wxsl_core::node::NodeRegistry;
use wxsl_core::resources::BufferLayout;
use wxsl_core::scene::Scene;
use wxsl_frame::effect::{Effect, EffectRegistry};
use wxsl_frame::pass::PassKind;
use wxsl_frame::pipeline::{PipelineConfig, TargetDesc};
use wxsl_frame::profile::Profile;
use wxsl_frame::setup::RenderSetup;
use wxsl_frame::types::TextureFormat;

use crate::ffi::WxslStatus;

/// An owned row of a computed buffer layout. No shader-buffer structs cross the ABI.
pub struct ComputedField {
    /// Which buffer: 0 material params, 1 user block, 2 instance attributes, 3 effect params.
    pub buffer: u32,
    /// Bind group of the enclosing buffer.
    pub group: u32,
    /// Binding of the enclosing buffer within its group.
    pub binding: u32,
    /// Field name.
    pub name: String,
    /// Host storage type (`bool` is stored as `u32`).
    pub ty: &'static str,
    /// Byte offset in the buffer.
    pub offset: u32,
    /// Byte size, WGSL padding included.
    pub size: u32,
    /// Alignment in bytes.
    pub align: u32,
    /// Byte size of the enclosing buffer.
    pub buffer_size: u32,
    /// Alignment of the enclosing buffer.
    pub buffer_align: u32,
}

/// Owned operation output; the foreign boundary exposes immutable views of this data.
pub struct Response {
    /// Outcome of the call.
    pub status: WxslStatus,
    /// The JSON envelope: `{version, abi, status, data}`.
    pub json: Vec<u8>,
    /// UTF-8 WGSL, empty unless the call compiled a shader.
    pub wgsl: Vec<u8>,
    /// Initial parameter bytes, already padded to the computed layout.
    pub params: Vec<u8>,
    /// Computed layout rows.
    pub fields: Vec<ComputedField>,
    /// A glTF import's binary outputs; `None` for every other call.
    pub import: Option<Box<ImportOutput>>,
}

/// What a glTF import returns beside its JSON (ADR 0080).
pub struct ImportOutput {
    /// One per scene mesh.
    pub meshes: Vec<ImportedMesh>,
    /// The embedded images, still encoded (PNG or JPEG).
    pub images: Vec<Vec<u8>>,
}

/// One imported primitive as native-endian bytes.
pub struct ImportedMesh {
    /// `WxslVertex` rows (`wxsl_host.h`), 48 bytes each.
    pub vertices: Vec<u8>,
    /// `uint32_t` triangle indices.
    pub indices: Vec<u8>,
    /// `float[4]` per vertex (`COLOR_0`), or empty.
    pub colors: Vec<u8>,
    /// `float[2]` per vertex (`TEXCOORD_1`), or empty.
    pub uv1: Vec<u8>,
}

impl Response {
    /// A response whose envelope carries `payload` as `data`.
    pub fn new(status: WxslStatus, payload: Value) -> Self {
        let json = serde_json::to_vec(&json!({
            "version": crate::ABI_VERSION,
            "abi": abi::REVISION,
            "status": status,
            "data": payload,
        }))
        .expect("response values are JSON serializable");
        Self {
            status,
            json,
            wgsl: Vec::new(),
            params: Vec::new(),
            fields: Vec::new(),
            import: None,
        }
    }

    /// A failure whose `data.message` is `message`.
    pub fn error(status: WxslStatus, message: impl ToString) -> Self {
        Self::new(status, json!({"message": message.to_string()}))
    }

    fn add_layout(&mut self, buffer: u32, group: u32, binding: u32, layout: &BufferLayout) {
        self.fields.extend(layout.fields().iter().map(|field| {
            ComputedField {
                buffer,
                group,
                binding,
                name: field.name.to_string(),
                ty: field
                    .ty
                    .buffer_type()
                    .expect("a computed field is host-shared"),
                offset: field.offset,
                size: field.size,
                align: field
                    .ty
                    .buffer_align()
                    .expect("a computed field is host-shared"),
                buffer_size: layout.size(),
                buffer_align: layout.align(),
            }
        }));
    }
}

type Result<T> = std::result::Result<T, Response>;

fn invalid(message: impl ToString) -> Response {
    Response::error(WxslStatus::InvalidDocument, message)
}

fn compile_error(message: impl ToString) -> Response {
    Response::error(WxslStatus::CompileError, message)
}

/// The envelope shares the documents' version/ABI defaults and refusal rule.
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    #[derive(Deserialize)]
    struct Version {
        #[serde(default = "request_version")]
        version: u32,
        #[serde(default = "shader_abi")]
        abi: u32,
    }
    fn request_version() -> u32 {
        crate::ABI_VERSION
    }
    fn shader_abi() -> u32 {
        abi::REVISION
    }
    let version: Version = serde_json::from_slice(bytes).map_err(invalid)?;
    if version.version > crate::ABI_VERSION {
        return Err(invalid(format!(
            "request version {} is newer than supported version {}",
            version.version,
            crate::ABI_VERSION
        )));
    }
    if version.abi != abi::REVISION {
        return Err(invalid(format!(
            "request abi {} does not match shader ABI {}",
            version.abi,
            abi::REVISION
        )));
    }
    serde_json::from_slice(bytes).map_err(invalid)
}

#[derive(Deserialize)]
#[serde(default)]
struct Config {
    target: TargetDesc,
    lighting_models: Vec<String>,
    features: Vec<String>,
    macros: MacroSet,
    /// What the host's device can run (ADR 0075); absent is the full
    /// profile, so every request written before it reads as it did.
    profile: Profile,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target: TargetDesc::new(800, 600, TextureFormat::Rgba8Unorm),
            lighting_models: vec!["wxsl.pbr".into()],
            features: Vec::new(),
            macros: MacroSet::new(),
            profile: Profile::FULL,
        }
    }
}

impl Config {
    fn resolve(self) -> Result<PipelineConfig> {
        if self.target.width == 0 || self.target.height == 0 {
            return Err(invalid("target width and height must be nonzero"));
        }
        let models = self
            .lighting_models
            .iter()
            .map(|name| {
                lighting::DEFAULT_MODELS
                    .iter()
                    .copied()
                    .find(|model| model.name == identity::resolve(name))
                    .ok_or_else(|| invalid(format!("unknown lighting model `{name}`")))
            })
            .collect::<Result<Vec<_>>>()?;
        let lighting = LightingSet::new(models).map_err(invalid)?;
        let names = self.features.iter().map(String::as_str).collect::<Vec<_>>();
        let features = lighting::feature_requests(&names).map_err(invalid)?;
        let config = PipelineConfig {
            target: self.target,
            lighting,
            features,
            macros: self.macros,
            profile: self.profile,
            // The C ABI's atlas is the header's `SHADOW_ATLAS_SIZE`.
            shadow_atlas_size: abi::SHADOW_ATLAS_SIZE,
        };
        config.plan().map_err(invalid)?;
        Ok(config)
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Library {
    /// Overlays the shipped module map (also supports replacing ABI sources).
    modules: BTreeMap<String, String>,
    /// Paths whose source is also derived as a function node, per ADR 0020.
    node_modules: Vec<String>,
}

impl Library {
    fn modules(&self) -> Result<wxsl_lang::Modules> {
        let mut modules = wxsl_lang::Modules::new();
        for (path, source) in wxsl_stdlib::MODULES {
            modules.insert(*path, *source);
        }
        for (path, source) in &self.modules {
            if wxsl_lang::ModulePath::parse(path).is_none() {
                return Err(invalid(format!("invalid module path `{path}`")));
            }
            modules.insert(path.as_str(), source.as_str());
        }
        Ok(modules)
    }

    fn registry(&self) -> Result<NodeRegistry> {
        let mut registry = wxsl_stdlib::registry();
        let modules = self.modules()?;
        for path in &self.node_modules {
            let source = modules
                .get(path)
                .ok_or_else(|| invalid(format!("node module `{path}` has no source")))?;
            let node = wxsl_lang::node_from_source(source, path).map_err(|diagnostics| {
                invalid(diagnostics.render(&|module| modules.get(module).map(str::to_string)))
            })?;
            registry.register(node);
        }
        Ok(registry)
    }
}

#[derive(Deserialize)]
struct PipelineRequest {
    pipeline: Graph,
    #[serde(default)]
    config: Config,
}

fn effect_metadata(effect: &Effect) -> Value {
    json!({
        "id": effect.id,
        "kind": effect.kind,
        "inputs": effect.inputs,
        "outputs": effect.outputs,
        "parameters": effect.parameters,
        "param_layout": effect.param_layout(),
    })
}

/// Compile a document and return the actual shared graph and schedule as JSON.
pub fn pipeline(bytes: &[u8]) -> Response {
    pipeline_impl(bytes).unwrap_or_else(|error| error)
}

fn pipeline_impl(bytes: &[u8]) -> Result<Response> {
    let request: PipelineRequest = parse(bytes)?;
    let config = request.config.resolve()?;
    let effects = EffectRegistry::shipped();
    let graph = wxsl_frame::pipeline_doc::compile(
        &request.pipeline,
        &wxsl_frame::pipeline_doc::document_registry(&effects),
        &effects,
        &config,
    )
    .map_err(compile_error)?;
    let schedule = graph.schedule().map_err(compile_error)?;
    let mut used = BTreeMap::new();
    for pass in graph.passes() {
        if let PassKind::Screen { effect: id } | PassKind::Compute { effect: id } = &pass.kind {
            // As the device runs it: a lowered compute effect is the
            // screen effect its pass now is.
            let effect = effects
                .get_on(config.profile, id)
                .ok_or_else(|| compile_error(format!("unknown effect `{id}`")))?;
            used.insert(effect.id, effect_metadata(&effect));
        }
    }
    Ok(Response::new(
        WxslStatus::Success,
        json!({
            "target": config.target,
            "graph": graph,
            "schedule": schedule,
            "effects": used.into_values().collect::<Vec<_>>(),
        }),
    ))
}

#[derive(Deserialize)]
struct ShaderRequest {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    effect: Option<String>,
    #[serde(default)]
    macros: MacroSet,
    #[serde(default)]
    library: Library,
    #[serde(default)]
    config: Config,
}

/// Compile raw WXSL or a shipped effect, using the same entry point as `variants`.
pub fn shader(bytes: &[u8]) -> Response {
    shader_impl(bytes).unwrap_or_else(|error| error)
}

fn shader_impl(bytes: &[u8]) -> Result<Response> {
    let request: ShaderRequest = parse(bytes)?;
    let config = request.config.resolve()?;
    let mut modules = request.library.modules()?;
    let effects = EffectRegistry::shipped();
    let mut response;
    let (root, macros) = match (&request.root, &request.effect) {
        (Some(root), None) => {
            response = Response::new(WxslStatus::Success, json!({"root": root}));
            (root.as_str(), request.macros)
        }
        (None, Some(id)) => {
            let effect = effects
                .get_on(config.profile, id)
                .ok_or_else(|| invalid(format!("unknown effect `{id}`")))?;
            let (path, source) = effect.module_source(&config.lighting, &config.features);
            modules.insert(path, source.as_ref());
            response = Response::new(WxslStatus::Success, effect_metadata(&effect));
            let layout = effect.param_layout();
            response.add_layout(
                3,
                abi::GROUP_PASS,
                (effect.inputs.len() + effect.outputs.len()) as u32,
                &layout,
            );
            let defaults = effect
                .parameters
                .iter()
                .map(|param| (param.name.to_string(), param.default))
                .collect();
            response.params = layout.filled(&defaults);
            let mut macros = effect.shader_macros(&request.macros);
            macros.overlay(&config.profile.macros());
            (path, macros)
        }
        _ => {
            return Err(invalid(
                "shader request must name exactly one of `root` or `effect`",
            ))
        }
    };
    response.wgsl = wxsl_lang::compile_with_macros(&modules, root, &macros)
        .map_err(compile_error)?
        .into_bytes();
    Ok(response)
}

#[derive(Deserialize)]
struct MaterialRequest {
    graph: Graph,
    stage: String,
    #[serde(default)]
    material: MaterialConfig,
    #[serde(default)]
    config: Config,
    #[serde(default)]
    library: Library,
}

/// Compile a material stage and expose its computed interface, fields and defaults.
pub fn material(bytes: &[u8]) -> Response {
    material_impl(bytes).unwrap_or_else(|error| error)
}

fn material_impl(bytes: &[u8]) -> Result<Response> {
    let request: MaterialRequest = parse(bytes)?;
    let config = request.config.resolve()?;
    let stage = MaterialStage::parse(&request.stage)
        .ok_or_else(|| invalid(format!("unknown material stage `{}`", request.stage)))?;
    let registry = request.library.registry()?;
    let mut authored = request.material;
    authored.features = config.features;
    let resolved = authored.resolve(&config.lighting).map_err(compile_error)?;
    let mut shader = codegen::generate(
        &request.graph,
        &registry,
        &CodegenOptions {
            stage,
            material: resolved.clone(),
            ..CodegenOptions::default()
        },
    )
    .map_err(compile_error)?;
    // The device profile's pins, over anything the graph says (ADR 0075);
    // folded into the variant key with the rest of the macros.
    shader.macros.overlay(&config.profile.macros());
    resolved
        .check_feature_demands(&shader.macros)
        .map_err(compile_error)?;
    let mut modules = request.library.modules()?;
    modules.insert(codegen::MATERIAL_MODULE, &shader.source);
    let wgsl = wxsl_lang::compile_with_macros(&modules, codegen::MATERIAL_MODULE, &shader.macros)
        .map_err(compile_error)?;
    let interface = &shader.interface;
    let mut response = Response::new(
        WxslStatus::Success,
        json!({
            "stage": stage,
            "vertex_entry": abi::VERTEX_ENTRY,
            "fragment_entry": shader.fragment_entry,
            "variant_key": shader.variant_key().to_string(),
            "interface": interface,
            "signature": interface.signature(),
        }),
    );
    response.add_layout(
        0,
        abi::GROUP_MATERIAL,
        abi::BINDING_MATERIAL_PARAMS,
        &interface.params,
    );
    if let Some(user) = &interface.user {
        response.add_layout(1, abi::GROUP_USER, abi::BINDING_USER_BLOCK, &user.layout);
    }
    response.add_layout(
        2,
        abi::GROUP_FRAME,
        abi::BINDING_INSTANCE_ATTRIBUTES,
        interface.geometry.instance(),
    );
    response.params = interface.params.filled(&interface.defaults);
    response.wgsl = wgsl.into_bytes();
    Ok(response)
}

#[derive(Deserialize)]
struct CheckRequest {
    pipeline: Graph,
    scene: Scene,
    #[serde(default)]
    config: Config,
    #[serde(default)]
    library: Library,
}

/// Run the shared load-time capability check and report all named incompatibilities.
pub fn check(bytes: &[u8]) -> Response {
    check_impl(bytes).unwrap_or_else(|error| error)
}

fn check_impl(bytes: &[u8]) -> Result<Response> {
    let request: CheckRequest = parse(bytes)?;
    let config = request.config.resolve()?;
    let registry = request.library.registry()?;
    let effects = EffectRegistry::shipped();
    let pipeline_error = wxsl_frame::pipeline_doc::compile(
        &request.pipeline,
        &wxsl_frame::pipeline_doc::document_registry(&effects),
        &effects,
        &config,
    )
    .map_err(|error| error.to_string())
    .and_then(|graph| graph.schedule().map_err(|error| error.to_string()))
    .err();
    let setup = RenderSetup::new(request.pipeline, effects, config);
    let mut errors = request
        .scene
        .validate()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    errors.extend(
        setup
            .check(&request.scene, &registry)
            .iter()
            .map(ToString::to_string),
    );
    if let Some(error) = pipeline_error {
        if !errors.iter().any(|existing| existing == &error) {
            errors.push(error);
        }
    }
    let status = if errors.is_empty() {
        WxslStatus::Success
    } else {
        WxslStatus::Incompatible
    };
    let capabilities = setup.capabilities().ok().map(|caps| json!({
        "stages": caps.stages,
        "effects": caps.effects,
        "lighting_models": caps.lighting.models().iter().map(|model| model.name).collect::<Vec<_>>(),
        "gbuffer_layout": caps.plan.layout(),
        "plan_signature": caps.plan.signature(),
    }));
    Ok(Response::new(
        status,
        json!({"incompatibilities": errors, "capabilities": capabilities}),
    ))
}

#[derive(Deserialize)]
struct ImportRequest {
    path: String,
}

/// Import a glTF or GLB file named by the request's `path` as a scene
/// document, its geometry and its embedded images (ADR 0080).
pub fn import_gltf(bytes: &[u8]) -> Response {
    match parse::<ImportRequest>(bytes) {
        Ok(request) => imported(wxsl_gltf::import(&request.path, &wxsl_stdlib::registry())),
        Err(error) => error,
    }
}

/// Import a GLB from its bytes; external buffers are an error.
pub fn import_glb(bytes: &[u8]) -> Response {
    imported(wxsl_gltf::import_slice(bytes, &wxsl_stdlib::registry()))
}

fn imported(
    result: std::result::Result<wxsl_gltf::ImportedScene, wxsl_gltf::ImportError>,
) -> Response {
    let imported = match result {
        Ok(imported) => imported,
        Err(error) => return invalid(error),
    };
    let lighting = match imported.lighting() {
        Ok(set) => set,
        Err(error) => return compile_error(error),
    };
    let scene = match serde_json::to_value(&imported.scene) {
        Ok(scene) => scene,
        Err(error) => return invalid(error),
    };
    let mut response = Response::new(
        WxslStatus::Success,
        json!({
            "scene": scene,
            "directory": imported.directory.as_ref().map(|path| path.display().to_string()),
            "lighting_models": lighting.models().iter().map(|model| model.name).collect::<Vec<_>>(),
            "warnings": imported.warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }),
    );
    let floats = |values: &mut dyn Iterator<Item = f32>| -> Vec<u8> {
        values.flat_map(f32::to_ne_bytes).collect()
    };
    let meshes = imported
        .meshes
        .iter()
        .map(|geometry| ImportedMesh {
            vertices: floats(&mut geometry.vertices.iter().flat_map(|vertex| {
                vertex
                    .position
                    .into_iter()
                    .chain(vertex.normal)
                    .chain(vertex.tangent)
                    .chain(vertex.uv)
            })),
            indices: geometry
                .indices
                .iter()
                .flat_map(|index| index.to_ne_bytes())
                .collect(),
            colors: floats(&mut geometry.colors.iter().flatten().flatten().copied()),
            uv1: floats(&mut geometry.uv1.iter().flatten().flatten().copied()),
        })
        .collect();
    response.import = Some(Box::new(ImportOutput {
        meshes,
        images: imported.images,
    }));
    response
}
