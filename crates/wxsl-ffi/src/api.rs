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
use wxsl_frame::pipeline::{PipelineConfig, TargetConfig};
use wxsl_frame::setup::RenderSetup;
use wxsl_frame::types::TextureFormat;

use crate::ffi::WxslStatus;

/// An owned row of a computed buffer layout. No shader-buffer structs cross the ABI.
pub struct ComputedField {
    pub buffer: u32,
    pub group: u32,
    pub binding: u32,
    pub name: String,
    pub ty: &'static str,
    pub offset: u32,
    pub size: u32,
    pub align: u32,
    pub buffer_size: u32,
    pub buffer_align: u32,
}

/// Owned operation output; the foreign boundary exposes immutable views of this data.
pub struct Response {
    pub status: WxslStatus,
    pub json: Vec<u8>,
    pub wgsl: Vec<u8>,
    pub params: Vec<u8>,
    pub fields: Vec<ComputedField>,
}

impl Response {
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
        }
    }

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
    target: TargetConfig,
    lighting_models: Vec<String>,
    features: Vec<String>,
    macros: MacroSet,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target: TargetConfig::new(800, 600, TextureFormat::Rgba8Unorm),
            lighting_models: vec!["wxsl.pbr".into()],
            features: Vec::new(),
            macros: MacroSet::new(),
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
            let effect = effects
                .get(id)
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
                .get(id)
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
            (path, effect.shader_macros(&request.macros))
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
    let shader = codegen::generate(
        &request.graph,
        &registry,
        &CodegenOptions {
            stage,
            material: resolved.clone(),
            ..CodegenOptions::default()
        },
    )
    .map_err(compile_error)?;
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
