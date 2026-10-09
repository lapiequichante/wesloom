//! Export validated documents, shared plans, WGSL and CPU upload data for an offline backend.

use glam::{Mat4, Vec3};
use serde_json::{json, Value as Json};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
};
use wxsl_core::{
    abi::{self, MaterialStage},
    graph::Graph,
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
    let defaults = effect
        .parameters
        .iter()
        .map(|p| (p.name.to_string(), p.default))
        .collect();
    Ok(
        json!({"kind": effect.kind, "wgsl": save(root, &format!("{}.wgsl", effect.id), wgsl)?,
        "params": save(root, &format!("{}.params.bin", effect.id), effect.param_layout().filled(&defaults))?,
        "param_size": effect.param_layout().size()}),
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
    ]
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut root = PathBuf::from("target/dawn-assets/pbr");
    let mut scene_path = None;
    let mut graph_path = None;
    let mut reference = false;
    let mut probes = false;
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
            "--probes" => probes = true,
            "--width" => width = args.next().ok_or("--width needs a value")?.parse()?,
            "--height" => height = args.next().ok_or("--height needs a value")?.parse()?,
            _ => return Err(format!("unknown argument `{arg}`").into()),
        }
    }
    if width == 0 || height == 0 {
        return Err("target dimensions must be nonzero".into());
    }
    let scene: Scene = if let Some(path) = scene_path {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        let graph: Graph = if let Some(path) = graph_path {
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
    let wire_config = json!({"target": config.target, "features": features});
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
            let result = response(wxsl_ffi::api::material(&serde_json::to_vec(
                &json!({"graph": entry.graph, "stage": stage.name(), "material": entry.config, "config": wire_config}),
            )?))?;
            let metadata: Json = serde_json::from_slice(&result.json)?;
            stages.insert(stage.name(), json!({"wgsl": save(&root, &format!("material{index}.{}.wgsl", stage.name()), &result.wgsl)?, "fragment_entry": metadata["data"]["fragment_entry"]}));
        }
        materials.push(json!({"name": entry.name, "interface": interface,
            "params": save(&root, &format!("material{index}.params.bin"), interface.params.filled(&interface.defaults))?,
            "attributes": save(&root, &format!("material{index}.attributes.bin"), &rows)?, "stages": stages}));
        native_materials.push(material);
    }
    manifest["materials"] = json!(materials);
    let effects = EffectRegistry::shipped()
        .with(BRDF_LUT)
        .with(LUT_VIEW)
        .with(RAMP_FILL)
        .with(RAMP_VIEW)
        .with(INDIRECT);
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
        pipelines.insert(stock.name().to_string(), data["data"].clone());
        graphs.push((stock.name().to_string(), stock.graph(&config)));
    }
    if probes {
        if scene.instances.is_empty() || cpu_meshes[scene.instances[0].mesh].indices.len() < 36 {
            return Err("device probes require a first draw with at least 36 indices".into());
        }
        graphs.extend(device_probes(config.target.format));
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
            passes.push(json!({"bindings": native.pass_binding_kinds(pass, params).into_iter().map(binding).collect::<Vec<_>>(), "view_slot": pass.view.slot(), "draws": draws}));
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
) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("reference adapter: {}", gpu.adapter.get_info().name);
    let target = OffscreenTarget::new(&gpu.device, config.target.width, config.target.height);
    let mut renderer = Renderer::new(
        &gpu.device,
        library,
        wxsl_render::TargetConfig::new(config.target.width, config.target.height, target.format()),
    )?;
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
        renderer.set_graph(graph.clone())?;
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
