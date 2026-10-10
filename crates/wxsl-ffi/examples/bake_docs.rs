//! Export validated documents, shared plans, WGSL and CPU upload data for an offline backend.

#[path = "../../wxsl/tests/probe/cube_mips.rs"]
mod cube_mips;
#[path = "../../wxsl/tests/probe/ibl.rs"]
mod ibl_probe;

use glam::{Mat4, Vec3};
use serde_json::{json, Value as Json};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
};
use wxsl_core::{
    abi::{self, MaterialStage},
    graph::{Graph, Node},
    lighting::{self, LightingSet},
    macros::MacroValue,
    node::{Value, ValueType},
    scene::{Instance, MaterialEntry, MeshEntry, MeshSource, Scene, TagExpr},
};
use wxsl_frame::{
    effect::{
        Effect, EffectKind, EffectOutput, EffectOutputShape, EffectParameter, EffectRegistry,
        EffectShader, BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW,
    },
    environment::{Camera, Environment, InstanceTransform, Light},
    graph::SlotShape,
    pass::{
        Attachment, DepthAttachment, DrawSource, PassDesc, PassKind, Policy, Read, ResourceDesc,
    },
    pipeline::{PipelineConfig, StockPipeline, TargetConfig},
    types::{Color, TextureFormat},
};
use wxsl_render::{
    bindings::MaterialBindings,
    draw::{DrawItem, InstanceAttributes},
    gpu::{GpuContext, OffscreenTarget},
    graph::{PassBinding, RenderGraph},
    library::ShaderLibrary,
    material::Material,
    mesh::{self, Mesh, MeshData},
    types::WgpuType,
    variants::EffectRequest,
    wgpu, RenderRequest, Renderer,
};

fn save(root: &Path, name: &str, data: impl AsRef<[u8]>) -> Result<String, Box<dyn Error>> {
    std::fs::write(root.join(name), data)?;
    Ok(name.to_string())
}

fn response(response: wxsl_ffi::api::Response) -> Result<wxsl_ffi::api::Response, Box<dyn Error>> {
    if response.status != wxsl_ffi::ffi::WxslStatus::Success {
        return Err(String::from_utf8(response.json)?.into());
    }
    Ok(response)
}

fn checker() -> Vec<u8> {
    let mut bytes = Vec::new();
    for y in 0..64 {
        for x in 0..64 {
            let grain = (((x * 7 + y * 13) % 11) as f32 / 11.0 - 0.5) * 0.06;
            let base = if ((x / 8) + (y / 8)) % 2 == 0 {
                0.82
            } else {
                0.30
            };
            let level = ((base + grain).clamp(0.0, 1.0) * 255.0) as u8;
            bytes.extend_from_slice(&[
                level,
                (level as f32 * 0.94) as u8,
                (level as f32 * 0.86) as u8,
                255,
            ]);
        }
    }
    bytes
}

fn environment(aspect: f32) -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(2.4, 1.9, 3.2),
            target: Vec3::ZERO,
            aspect,
            ..Camera::default()
        },
        lights: vec![
            Light::point(Vec3::new(2.6, 3.0, 2.2), Vec3::new(1.0, 0.86, 0.72), 42.0),
            Light::point(Vec3::new(-3.0, 1.2, -1.6), Vec3::new(0.5, 0.65, 1.0), 18.0),
            Light::directional(Vec3::new(-0.4, 0.7, -1.0), Vec3::new(0.7, 0.75, 0.9), 1.1),
        ],
        ambient_sky: Vec3::new(0.14, 0.19, 0.28),
        ambient_ground: Vec3::new(0.05, 0.04, 0.035),
        exposure: 1.0,
        time: 1.0,
        previous_time: 1.0,
        previous_camera: None,
    }
}

fn dimension(dimension: wgpu::TextureViewDimension) -> &'static str {
    match dimension {
        wgpu::TextureViewDimension::D2 => "d2",
        wgpu::TextureViewDimension::D2Array => "d2_array",
        wgpu::TextureViewDimension::D3 => "d3",
        wgpu::TextureViewDimension::Cube => "cube",
        _ => panic!("the neutral plan does not name this dimension"),
    }
}

fn binding(kind: PassBinding) -> Json {
    match kind {
        PassBinding::Texture {
            sample_type,
            view_dimension,
        } => {
            json!({"kind": "texture", "dimension": dimension(view_dimension), "sample_type": match sample_type {
                wgpu::TextureSampleType::Float { filterable: true } => "float",
                wgpu::TextureSampleType::Float { filterable: false } => "unfilterable_float",
                wgpu::TextureSampleType::Depth => "depth", wgpu::TextureSampleType::Sint => "sint", wgpu::TextureSampleType::Uint => "uint",
            }})
        }
        PassBinding::StorageTexture {
            format,
            view_dimension,
        } => {
            json!({"kind": "storage_texture", "dimension": dimension(view_dimension), "format": TextureFormat::from_wgpu(format)})
        }
        PassBinding::Buffer { read_only, size } => {
            json!({"kind": "buffer", "read_only": read_only, "size": size})
        }
        PassBinding::Uniform { size } => json!({"kind": "uniform", "size": size}),
    }
}

fn slot(shape: SlotShape, target: TargetConfig) -> Json {
    let mut value = serde_json::to_value(shape).unwrap();
    if let SlotShape::Texture { extent, layers, .. } = shape {
        let (w, h) = extent.resolve(target.width, target.height);
        value["texture"]["size"] = json!([w, h, layers]);
    }
    value
}

fn compile_effect(
    root: &Path,
    effect: Effect,
    library: &ShaderLibrary,
    set: &LightingSet,
    config: &PipelineConfig,
) -> Result<Json, Box<dyn Error>> {
    let request = EffectRequest::new(effect.clone(), &config.macros, set, &config.features);
    let wgsl = request.compile(library).1?;
    let (path, source) = effect.module_source(set, &config.features);
    let runtime_request = json!({"root": path, "macros": effect.shader_macros(&config.macros),
        "library": {"modules": {path: source}}});
    let defaults = effect
        .parameters
        .iter()
        .map(|p| (p.name.to_string(), p.default))
        .collect();
    Ok(
        json!({"kind": effect.kind, "wgsl": save(root, &format!("{}.wgsl", effect.id), wgsl)?,
        "params": save(root, &format!("{}.params.bin", effect.id), effect.param_layout().filled(&defaults))?,
        "param_size": effect.param_layout().size(), "request": runtime_request}),
    )
}

const INDIRECT: Effect = Effect {
    id: "test.indirect_arguments",
    label: "Indirect arguments",
    description: "One indexed draw record",
    inputs: &[],
    outputs: &[EffectOutput {
        name: "arguments",
        shape: EffectOutputShape::Buffer,
        description: "Indexed draw record",
    }],
    parameters: &[EffectParameter {
        name: "index_count",
        default: Value::U32(36),
    }],
    kind: EffectKind::Compute {
        entry: "fill_arguments",
        workgroups: [1, 1, 1],
    },
    shader: EffectShader::Source {
        path: "package::test::indirect_arguments",
        wxsl: include_str!("../../wxsl/tests/probe/indirect.wxsl"),
    },
};

fn device_probes(format: TextureFormat) -> Vec<(String, wxsl_frame::graph::RenderGraph)> {
    use wxsl_frame::graph::RenderGraph as Plan;
    let mut buffer = Plan::new(format);
    let ramp = buffer.resource(ResourceDesc::buffer("ramp", 1024).persistent(0));
    buffer.pass(
        PassDesc::compute("fill ramp", RAMP_FILL.id)
            .with_write(ramp)
            .with_policy(Policy::Once),
    );
    buffer.pass(
        PassDesc::screen("show ramp", RAMP_VIEW.id)
            .with_reads([Read::current(ramp)])
            .with_color(Attachment::clear(Plan::TARGET, Color::BLACK)),
    );

    let mut history = Plan::new(format);
    let ring = history.resource(ResourceDesc::color("history", format).persistent(2));
    history.pass(
        PassDesc::geometry(
            "write history",
            DrawSource::Scene(TagExpr::Never),
            MaterialStage::FORWARD_LIT,
        )
        .with_state(wxsl_frame::pass::PassState::FULLSCREEN)
        .with_color(Attachment::clear(
            ring,
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        )),
    );
    history.pass(
        PassDesc::screen("read history", LUT_VIEW.id)
            .with_reads([Read::previous(ring, 1)])
            .with_color(Attachment::clear(Plan::TARGET, Color::BLACK)),
    );

    let mut indirect = Plan::new(format);
    let args = indirect.resource(ResourceDesc::buffer("arguments", 20));
    let color = indirect.resource(ResourceDesc::color("linear", TextureFormat::Rgba16Float));
    let depth = indirect.resource(ResourceDesc::color("depth", TextureFormat::Depth32Float));
    // Declared before the writer: ordering must come from the shared scheduler.
    indirect.pass(
        PassDesc::geometry(
            "draw indirect",
            DrawSource::Indirect {
                buffer: args,
                offset: 0,
                count: 1,
                draw: 0,
            },
            MaterialStage::FORWARD_LIT,
        )
        .with_color(Attachment::clear(color, Color::BLACK))
        .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    indirect.pass(PassDesc::compute("fill arguments", INDIRECT.id).with_write(args));
    indirect.pass(
        PassDesc::screen("tonemap", "wxsl.tonemap")
            .with_reads([Read::current(color)])
            .with_color(Attachment::clear(Plan::TARGET, Color::BLACK)),
    );
    vec![
        ("buffer_probe".into(), buffer),
        ("history_probe".into(), history),
        ("indirect_probe".into(), indirect),
        (
            "cube_mips_probe".into(),
            cube_mips::plan(format.to_wgpu()).into(),
        ),
        (
            "ibl_constant_probe".into(),
            ibl_probe::plan(format.to_wgpu(), false).into(),
        ),
        (
            "ibl_directional_probe".into(),
            ibl_probe::plan(format.to_wgpu(), true).into(),
        ),
        (
            "ibl_hotspot_probe".into(),
            ibl_probe::hotspot_plan(format.to_wgpu()).into(),
        ),
    ]
}

fn category_sample(category: &str) -> Result<Graph, Box<dyn Error>> {
    if matches!(category, "sample" | "filter") {
        return Ok(serde_json::from_str(include_str!(
            "../../wxsl/assets/pbr_cube.wxsl.json"
        ))?);
    }
    let (id, input, ty) = match category {
        "math" => ("math.sine", "a", ValueType::Vec3),
        "color" => ("color.hsv_to_rgb", "hsv", ValueType::Vec3),
        "space" => ("space.rotate_uv", "uv", ValueType::Vec2),
        "lighting" => ("lighting.distribution_ggx", "n_dot_h", ValueType::F32),
        "generative" => ("generative.value_noise3", "p", ValueType::Vec3),
        "sdf" => ("sdf.sphere", "p", ValueType::Vec3),
        "animation" => ("animation.pulse", "time", ValueType::F32),
        "distort" => ("distort.swirl_uv", "uv", ValueType::Vec2),
        _ => return Err(format!("unknown stdlib category `{category}`").into()),
    };
    let registry = wxsl_stdlib::registry();
    let mut graph = Graph::new(format!("parity {category}"));
    let uv = graph.add_node(abi::context_node_id("uv"));
    let split = graph.add_node("convert.split.vec2f");
    graph.wire(&registry, (uv, "out"), (split, "v"))?;
    let coords = graph.add(Node::new("convert.combine.vec3f").with_param("z", Value::F32(0.37)));
    graph.wire(&registry, (split, "x"), (coords, "x"))?;
    graph.wire(&registry, (split, "y"), (coords, "y"))?;
    let sample = graph.add(Node::new(id).with_stage(wxsl_core::graph::StageConstraint::Fragment));
    if id == "math.sine" {
        graph.set_generic(&registry, sample, "T", ty)?;
    }
    let from = match ty {
        ValueType::Vec3 => (coords, "out"),
        ValueType::Vec2 => (uv, "out"),
        _ => (split, "x"),
    };
    graph.wire(&registry, from, (sample, input))?;
    let output_ty = if id == "math.sine" {
        ty
    } else {
        registry.get(id).ok_or("sample node missing")?.outputs[0].ty
    };
    let (color, socket) = match output_ty {
        ValueType::Vec3 => (sample, "out"),
        ValueType::Vec2 => {
            let channels = graph.add_node("convert.split.vec2f");
            graph.wire(&registry, (sample, "out"), (channels, "v"))?;
            let rgb =
                graph.add(Node::new("convert.combine.vec3f").with_param("z", Value::F32(0.2)));
            graph.wire(&registry, (channels, "x"), (rgb, "x"))?;
            graph.wire(&registry, (channels, "y"), (rgb, "y"))?;
            (rgb, "out")
        }
        ValueType::F32 => {
            let rgb = graph.add_node("convert.splat");
            graph.set_generic(&registry, rgb, "T", ValueType::Vec3)?;
            graph.wire(&registry, (sample, "out"), (rgb, "value"))?;
            (rgb, "out")
        }
        _ => return Err("sample output is not a colour".into()),
    };
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.wire(&registry, (color, socket), (output, "emissive"))?;
    graph.validate(&registry)?;
    Ok(graph)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut root = PathBuf::from("target/dawn-assets/pbr");
    let mut scene_path = None;
    let mut graph_path = None;
    let mut reference = false;
    let mut probes = false;
    let mut sample = None;
    let mut pipeline_path = None;
    let mut hdri_path = None;
    let mut width = 800;
    let mut height = 600;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => root = PathBuf::from(args.next().ok_or("--out needs a path")?),
            "--scene" => {
                scene_path = Some(PathBuf::from(args.next().ok_or("--scene needs a path")?))
            }
            "--graph" => {
                graph_path = Some(PathBuf::from(args.next().ok_or("--graph needs a path")?))
            }
            "--reference" => reference = true,
            "--hdri" => {
                hdri_path = Some(PathBuf::from(
                    args.next().ok_or("--hdri needs a Radiance HDR path")?,
                ))
            }
            "--probes" => probes = true,
            "--sample" => sample = Some(args.next().ok_or("--sample needs a category")?),
            "--pipeline" => {
                pipeline_path = Some(PathBuf::from(
                    args.next().ok_or("--pipeline needs a document")?,
                ))
            }
            "--width" => width = args.next().ok_or("--width needs a value")?.parse()?,
            "--height" => height = args.next().ok_or("--height needs a value")?.parse()?,
            _ => return Err(format!("unknown argument `{arg}`").into()),
        }
    }
    if width == 0 || height == 0 {
        return Err("target dimensions must be nonzero".into());
    }
    let hdri = if let Some(path) = hdri_path {
        let reader = image::ImageReader::open(path)?.with_guessed_format()?;
        if reader.format() != Some(image::ImageFormat::Hdr) {
            return Err("--hdri expects a Radiance HDR file".into());
        }
        let image = reader.decode()?.into_rgb32f();
        Some((
            image.width(),
            image.height(),
            image.pixels().map(|p| p.0).collect::<Vec<_>>(),
        ))
    } else {
        None
    };
    let scene: Scene = if let Some(path) = scene_path {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        let graph: Graph = if let Some(category) = &sample {
            category_sample(category)?
        } else if let Some(path) = graph_path {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            serde_json::from_str(include_str!("../../wxsl/assets/pbr_cube.wxsl.json"))?
        };
        let mut scene = Scene::new("pbr cube");
        scene.add_mesh(MeshEntry {
            name: "cube".into(),
            source: MeshSource::Cube { size: 1.6 },
        });
        scene.add_material(MaterialEntry::new("pbr cube", graph));
        scene.add_instance(Instance::new(0, 0).with_transform(
            (Mat4::from_rotation_y(0.6 * 0.45) * Mat4::from_rotation_x(0.6 * 0.21)).to_cols_array(),
        ));
        scene
    };
    let problems = scene.validate();
    if !problems.is_empty() {
        return Err(format!("invalid scene: {problems:?}").into());
    }
    let features = lighting::FEATURES
        .iter()
        .filter(|feature| {
            scene.materials.iter().any(|material| {
                material.graph.macros().get(feature.macro_name) == Some(MacroValue::Flag(true))
                    || material.config.macros.get(feature.macro_name)
                        == Some(MacroValue::Flag(true))
            })
        })
        .map(|feature| feature.name)
        .collect::<Vec<_>>();
    let mut config =
        PipelineConfig::new(TargetConfig::new(width, height, TextureFormat::Rgba8Unorm));
    config.features = lighting::feature_requests(&features)?;
    let mut model_names = scene
        .materials
        .iter()
        .map(|material| material.config.model.as_deref().unwrap_or("wxsl.pbr"))
        .map(|name| wxsl_core::identity::resolve(name).into_owned())
        .collect::<Vec<_>>();
    model_names.sort();
    model_names.dedup();
    config.lighting = LightingSet::new(
        model_names
            .iter()
            .map(|name| {
                lighting::DEFAULT_MODELS
                    .iter()
                    .copied()
                    .find(|model| model.name == name)
                    .ok_or_else(|| format!("unknown lighting model `{name}`"))
            })
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let wire_config =
        json!({"target": config.target, "features": features, "lighting_models": model_names});
    let registry = wxsl_stdlib::registry();
    let mut library = ShaderLibrary::new();
    library.insert_all(wxsl_stdlib::MODULES.iter().copied());
    for stock in StockPipeline::ALL {
        response(wxsl_ffi::api::check(&serde_json::to_vec(
            &json!({"pipeline": stock.document(), "scene": scene, "config": wire_config}),
        )?))?;
    }
    std::fs::create_dir_all(&root)?;
    std::fs::write(root.join("wxsl_host.h"), wxsl_frame::HOST_HEADER)?;
    save(
        &root,
        "scene.source.json",
        serde_json::to_vec_pretty(&scene)?,
    )?;
    let env = environment(width as f32 / height as f32);
    let transforms = scene
        .instances
        .iter()
        .map(|instance| InstanceTransform::new(Mat4::from_cols_array(&instance.transform)))
        .collect::<Vec<_>>();
    let checker = checker();
    let mut manifest = json!({"version": 1, "abi": abi::REVISION, "host_layout": wxsl_core::host::schema_id().to_string(),
        "width": width, "height": height, "name": scene.name,
        "frame": {"views": save(&root, "views.bin", bytemuck::cast_slice(&env.views()))?,
            "scene": save(&root, "scene.bin", bytemuck::bytes_of(&env.uniform()))?,
            "instances": save(&root, "instances.bin", bytemuck::cast_slice(&transforms))?,
            "light_count": abi::MAX_LIGHTS, "lut_size": abi::ENVIRONMENT_LUT_SIZE},
        "checker": save(&root, "checker.bin", &checker)?, "checker_size": [64, 64],
        "draws": scene.instances.iter().map(|i| json!({"mesh": i.mesh, "material": i.material})).collect::<Vec<_>>(),
    });
    let mut meshes = Vec::new();
    let mut cpu_meshes = Vec::new();
    for (index, entry) in scene.meshes.iter().enumerate() {
        let (vertices, indices) = match &entry.source {
            MeshSource::Cube { size } => mesh::cube_geometry(*size),
            MeshSource::Sphere { radius } => mesh::sphere_geometry(*radius, 48, 32),
            MeshSource::Plane { size } => mesh::plane_geometry(*size, 32),
            MeshSource::Torus {
                radius,
                tube_radius,
            } => mesh::torus_geometry(*radius, *tube_radius, 64, 24),
            MeshSource::File { path, .. } => {
                return Err(format!(
                    "offline demo does not provide a file mesh loader for `{path}`"
                )
                .into())
            }
        };
        meshes.push(json!({"vertices": save(&root, &format!("mesh{index}.vertices.bin"), bytemuck::cast_slice(&vertices))?,
            "indices": save(&root, &format!("mesh{index}.indices.bin"), bytemuck::cast_slice(&indices))?, "index_count": indices.len()}));
        cpu_meshes.push(MeshData::new(vertices, indices));
    }
    manifest["meshes"] = json!(meshes);
    let vertex = wxsl_core::host::structures()
        .into_iter()
        .find(|s| s.name == "Vertex")
        .unwrap();
    manifest["vertex_layout"] = json!({"stride": vertex.size(), "attributes": vertex.offsets().iter().enumerate().map(|(i, offset)| json!({"location": i, "offset": offset, "format": vertex.vertex_format(i)})).collect::<Vec<_>>()});
    let mut materials = Vec::new();
    let mut native_materials = Vec::new();
    let mut attributes = Vec::new();
    for (index, entry) in scene.materials.iter().enumerate() {
        let material = Material::with_lighting(
            &entry.graph,
            &registry,
            &entry.config.clone().with_features(config.features.clone()),
            &config.lighting,
        )?;
        let interface = material.interface();
        if !interface.geometry.vertex().is_empty() {
            return Err(format!("mesh cannot supply declared streams for `{}`", entry.name).into());
        }
        if interface.user.is_some() {
            return Err(format!(
                "offline demo has no application user block for `{}`",
                entry.name
            )
            .into());
        }
        let layout = interface.geometry.instance();
        let mut rows = vec![0; layout.size() as usize * scene.instances.len()];
        let mut defaults = InstanceAttributes::new();
        for field in layout.fields() {
            let value = if field.name.as_str() == "instance_tint" && field.ty == ValueType::Vec3 {
                Value::Vec3([1.0; 3])
            } else {
                field.ty.zero().ok_or("not a host-shared attribute")?
            };
            defaults.set(field.name.as_str(), value);
            for row in rows.chunks_mut(layout.size() as usize) {
                layout.write(row, field.name.as_str(), value)?;
            }
        }
        attributes.push(defaults);
        let mut stages = BTreeMap::new();
        for stage in MaterialStage::ALL {
            let request = json!({"graph": entry.graph, "stage": stage.name(), "material": entry.config, "config": wire_config});
            let result = response(wxsl_ffi::api::material(&serde_json::to_vec(&request)?))?;
            let metadata: Json = serde_json::from_slice(&result.json)?;
            stages.insert(stage.name(), json!({"wgsl": save(&root, &format!("material{index}.{}.wgsl", stage.name()), &result.wgsl)?, "fragment_entry": metadata["data"]["fragment_entry"], "request": request, "signature": metadata["data"]["signature"], "variant_key": metadata["data"]["variant_key"]}));
        }
        materials.push(json!({"name": entry.name, "interface": interface,
            "params": save(&root, &format!("material{index}.params.bin"), interface.params.filled(&interface.defaults))?,
            "attributes": save(&root, &format!("material{index}.attributes.bin"), &rows)?, "stages": stages}));
        native_materials.push(material);
    }
    manifest["materials"] = json!(materials);
    let mut effects = EffectRegistry::shipped()
        .with(BRDF_LUT)
        .with(LUT_VIEW)
        .with(RAMP_FILL)
        .with(RAMP_VIEW)
        .with(INDIRECT)
        .with(cube_mips::VIEW);
    for effect in ibl_probe::effects() {
        effects.add(effect);
    }
    if sample.as_deref() == Some("filter") {
        effects.add(wxsl::effects::fxaa(&registry)?);
    }
    let mut compiled_effects = BTreeMap::new();
    for effect in effects.iter() {
        compiled_effects.insert(
            effect.id,
            compile_effect(&root, effect.clone(), &library, &config.lighting, &config)?,
        );
    }
    manifest["effects"] = json!(compiled_effects);
    let mut pipelines = BTreeMap::new();
    let mut graphs = Vec::new();
    for stock in StockPipeline::ALL {
        let output = response(wxsl_ffi::api::pipeline(&serde_json::to_vec(
            &json!({"pipeline": stock.document(), "config": wire_config}),
        )?))?;
        let data: Json = serde_json::from_slice(&output.json)?;
        let mut pipeline = data["data"].clone();
        pipeline["request"] = json!({"pipeline": stock.document(), "config": wire_config});
        pipeline["check_request"] =
            json!({"pipeline": stock.document(), "scene": scene, "config": wire_config});
        pipelines.insert(stock.name().to_string(), pipeline);
        graphs.push((stock.name().to_string(), stock.graph(&config)));
    }
    if probes {
        if scene.instances.is_empty() || cpu_meshes[scene.instances[0].mesh].indices.len() < 36 {
            return Err("device probes require a first draw with at least 36 indices".into());
        }
        graphs.extend(device_probes(config.target.format));
        for stock in StockPipeline::ALL {
            let mut graph: wxsl_render::graph::RenderGraph = stock.graph(&config).into();
            ibl_probe::append_lighting_bake(&mut graph, true);
            graphs.push((format!("ibl_{}_probe", stock.name()), graph.into()));
            let document = ibl_probe::hdr_document(*stock);
            save(
                &root,
                &format!("hdr.{}.pipeline.json", stock.name()),
                serde_json::to_vec_pretty(&document)?,
            )?;
            let request = json!({"pipeline": document, "config": wire_config});
            let output = response(wxsl_ffi::api::pipeline(&serde_json::to_vec(&request)?))?;
            let exported: Json = serde_json::from_slice(&output.json)?;
            let mut exported = exported["data"].clone();
            exported["request"] = request;
            exported["check_request"] =
                json!({"pipeline": document, "scene": scene, "config": wire_config});
            response(wxsl_ffi::api::check(&serde_json::to_vec(
                &exported["check_request"],
            )?))?;
            let graph = ibl_probe::hdr_plan(*stock, &config);
            assert_eq!(exported["graph"], serde_json::to_value(&*graph)?);
            let name = format!("hdr_{}_probe", stock.name());
            pipelines.insert(name.clone(), exported);
            graphs.push((name, graph.into()));
        }
    }
    if let Some(path) = pipeline_path {
        let document: Graph = serde_json::from_slice(&std::fs::read(path)?)?;
        let request = json!({"pipeline": document, "config": wire_config});
        let output = response(wxsl_ffi::api::pipeline(&serde_json::to_vec(&request)?))?;
        let data: Json = serde_json::from_slice(&output.json)?;
        let graph = wxsl_frame::pipeline_doc::compile(
            &document,
            &wxsl_frame::pipeline_doc::document_registry(&effects),
            &effects,
            &config,
        )?;
        let mut exported = data["data"].clone();
        exported["request"] = request;
        exported["check_request"] =
            json!({"pipeline": document, "scene": scene, "config": wire_config});
        response(wxsl_ffi::api::check(&serde_json::to_vec(
            &exported["check_request"],
        )?))?;
        pipelines.insert("gallery".into(), exported);
        graphs.push(("gallery".into(), graph));
    }
    if sample.as_deref() == Some("filter") {
        let mut graph = wxsl_frame::graph::RenderGraph::new(config.target.format);
        let linear = graph.resource(ResourceDesc::color("linear", TextureFormat::Rgba16Float));
        let encoded = graph.resource(ResourceDesc::color("encoded", TextureFormat::Rgba16Float));
        let depth = graph.resource(ResourceDesc::color("depth", TextureFormat::Depth32Float));
        graph.pass(
            PassDesc::geometry(
                "lit",
                DrawSource::Scene(TagExpr::Always),
                MaterialStage::FORWARD_LIT,
            )
            .with_color(Attachment::clear(linear, config.target.clear_color))
            .with_depth(DepthAttachment::clear(depth, 1.0)),
        );
        graph.pass(
            PassDesc::screen("tonemap", "wxsl.tonemap")
                .with_reads([Read::current(linear)])
                .with_color(Attachment::clear(encoded, Color::BLACK)),
        );
        graph.pass(
            PassDesc::screen("fxaa", "wxsl.fxaa")
                .with_reads([Read::current(encoded)])
                .with_color(Attachment::clear(
                    wxsl_frame::graph::RenderGraph::TARGET,
                    Color::BLACK,
                )),
        );
        graphs.push(("filter".into(), graph));
    }
    for (name, graph) in &graphs {
        let data = pipelines
            .entry(name.clone())
            .or_insert_with(|| json!({"graph": graph}));
        let native: RenderGraph = graph.clone().into();
        let schedule = graph.schedule()?;
        data["schedule"] = serde_json::to_value(&schedule)?;
        data["requirements"] = json!({
            "max_color_attachments": graph.passes().iter().map(|p| p.color.len()).max().unwrap_or(0),
            "max_color_attachment_bytes_per_sample": if graph.passes().iter().any(|p| matches!(&p.kind, PassKind::Geometry { stage, .. } if *stage == MaterialStage::GBUFFER)) { wxsl_frame::pipeline::gbuffer_layout_bytes_per_sample(graph.gbuffer_layout()) } else { 0 },
            "float32_blendable": graph.passes().iter().any(|p| p.color.iter().any(|a| {
                (a.blend.is_some() || p.state.blend.is_some()) && matches!(graph.resource_desc(a.resource).unwrap().shape,
                    wxsl_frame::pass::ResourceShape::Texture { format: TextureFormat::R32Float | TextureFormat::Rg32Float | TextureFormat::Rgba32Float, .. })
            })),
        });
        data["reference_frames"] = json!(if name.ends_with("_probe") { 3 } else { 1 });
        data["physical_slots"] = json!(schedule
            .slots()
            .iter()
            .map(|desc| slot(desc.shape, config.target))
            .collect::<Vec<_>>());
        let mut passes = Vec::new();
        for pass in graph.passes() {
            let effect = match &pass.kind {
                PassKind::Screen { effect } | PassKind::Compute { effect } => effects.get(effect),
                _ => None,
            };
            let params = effect
                .as_ref()
                .filter(|e| !e.parameters.is_empty())
                .map(|e| e.param_layout().size());
            let draws = match &pass.kind {
                PassKind::Geometry {
                    source: DrawSource::Scene(expression),
                    stage,
                } => scene
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| {
                        expression.matches(scene.instance_tags(i))
                            && (*stage != MaterialStage::SHADOW
                                || native_materials[i.material].cast_shadow())
                            && match pass.view {
                                wxsl_frame::pass::PassView::Light { index } => {
                                    env.light_casts_shadow(index)
                                }
                                _ => true,
                            }
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>(),
                PassKind::Geometry {
                    source: DrawSource::Indirect { draw, .. },
                    ..
                } => vec![*draw],
                _ => Vec::new(),
            };
            let parameter_file = effect
                .map(|e| {
                    let bytes = e.initial_parameters(&pass.parameters)?;
                    save(
                        &root,
                        &format!("{name}.pass{}.params.bin", passes.len()),
                        bytes,
                    )
                    .map_err(|e| e.to_string())
                })
                .transpose()?;
            passes.push(json!({"bindings": native.pass_binding_kinds(pass, params).into_iter().map(binding).collect::<Vec<_>>(), "view_slot": pass.view.slot(), "draws": draws, "params": parameter_file}));
        }
        data["pass_data"] = json!(passes);
    }
    manifest["pipelines"] = json!(pipelines);
    save(
        &root,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    if reference {
        render_reference(
            &root,
            &scene,
            &cpu_meshes,
            &native_materials,
            &attributes,
            &checker,
            &env,
            &config,
            &graphs,
            &effects,
            library,
            hdri.as_ref(),
        )?;
    }
    println!(
        "exported {}: {} materials, {} meshes -> {}",
        scene.name,
        scene.materials.len(),
        scene.meshes.len(),
        root.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_stdlib_category_has_a_valid_rendered_sample() {
        let listed = include_str!("../../../dawn/tests/scenes.txt")
            .lines()
            .filter_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                (fields[1] == "sample").then(|| fields[2].to_string())
            })
            .collect::<BTreeSet<_>>();
        let shipped =
            std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../wxsl-stdlib/shaders"))
                .unwrap()
                .map(|entry| entry.unwrap())
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().into_string().unwrap())
                .filter(|name| name != "wxsl")
                .collect::<BTreeSet<_>>();
        assert_eq!(
            listed, shipped,
            "new categories owe the backend harness a sample"
        );
        for category in listed {
            let graph = category_sample(&category).unwrap();
            for stage in MaterialStage::ALL {
                response(wxsl_ffi::api::material(
                    &serde_json::to_vec(&json!({"graph": graph, "stage": stage.name()})).unwrap(),
                ))
                .unwrap();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_reference(
    root: &Path,
    scene: &Scene,
    cpu_meshes: &[MeshData],
    materials: &[Material],
    attributes: &[InstanceAttributes],
    checker: &[u8],
    env: &Environment,
    config: &PipelineConfig,
    graphs: &[(String, wxsl_frame::graph::RenderGraph)],
    effects: &EffectRegistry,
    library: ShaderLibrary,
    hdri: Option<&(u32, u32, Vec<[f32; 3]>)>,
) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("reference adapter: {}", gpu.adapter.get_info().name);
    let target = OffscreenTarget::new(&gpu.device, config.target.width, config.target.height);
    let mut renderer = Renderer::new(
        &gpu.device,
        library,
        wxsl_render::TargetConfig::new(config.target.width, config.target.height, target.format()),
    )?;
    renderer.set_lighting(config.lighting.clone())?;
    renderer.set_features(
        &config
            .features
            .iter()
            .map(|request| request.source.name())
            .collect::<Vec<_>>(),
    )?;
    for effect in effects.iter() {
        renderer.add_effect(effect.clone());
    }
    let meshes = cpu_meshes
        .iter()
        .enumerate()
        .map(|(i, data)| Mesh::upload(&gpu.device, &format!("mesh{i}"), data))
        .collect::<Vec<_>>();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("checker"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        checker,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(256),
            rows_per_image: Some(64),
        },
        texture.size(),
    );
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bindings = materials
        .iter()
        .map(|material| -> Result<MaterialBindings, Box<dyn Error>> {
            let mut bindings = renderer.material_bindings(&gpu.device, material);
            for resource in &material.interface().resources {
                match resource.ty {
                    ValueType::Sampler => bindings.set_sampler(resource.name.as_str(), &sampler)?,
                    ValueType::Texture2d => {
                        bindings.set_texture(resource.name.as_str(), &texture_view)?
                    }
                    _ => {
                        return Err(format!("unsupported demo resource `{}`", resource.name).into())
                    }
                }
            }
            bindings.upload(&gpu.device, &gpu.queue)?;
            Ok(bindings)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let draws = scene
        .instances
        .iter()
        .map(|instance| {
            DrawItem::new(&meshes[instance.mesh], &materials[instance.material])
                .with_transform(Mat4::from_cols_array(&instance.transform))
                .with_bindings(&bindings[instance.material])
                .with_attributes(&attributes[instance.material])
                .with_tags(scene.instance_tags(instance))
        })
        .collect::<Vec<_>>();
    let mut draw_list = wxsl_render::draw::DrawList::new();
    for draw in draws {
        draw_list.push(draw);
    }
    for (name, graph) in graphs {
        renderer.remove_import("source HDR");
        let mut graph = graph.clone();
        if graph.resource_by_label("source HDR").is_some() {
            let default = (1, 1, vec![[2.0, 3.0, 4.0]]);
            let (width, height, rgb) = hdri.unwrap_or(&default);
            let image = wxsl_render::environment::upload_environment_image(
                &gpu.device,
                &gpu.queue,
                *width,
                *height,
                rgb,
            )?;
            graph.set_environment_scale(image.scale);
            renderer.import_resource("source HDR", image.view);
        }
        renderer.set_graph(graph)?;
        for _ in 0..if name.ends_with("_probe") { 3 } else { 1 } {
            renderer.render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: env,
                    draws: &draw_list,
                },
            )?;
        }
        gpu.wait();
        image::save_buffer(
            root.join(format!("reference_{name}.png")),
            &target.read_rgba8(&gpu.device, &gpu.queue),
            config.target.width,
            config.target.height,
            image::ExtendedColorType::Rgba8,
        )?;
    }
    Ok(())
}
