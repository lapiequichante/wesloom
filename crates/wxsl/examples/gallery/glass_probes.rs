//! Opaque local-probe glass with torus-only self interfaces (ADR 0070).
use super::*;
use wxsl::core::node::{NodeDefinition, Socket, ValueType};
use wxsl::core::scene::TagExpr;
use wxsl::render::effect::{Effect, EffectInput, EffectInputKind, EffectKind, EffectShader};
use wxsl::render::pass::{
    Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassKind, PassState,
    Read, ResourceDesc, ResourceId,
};
use wxsl::render::types::{Color, TextureFormat};

const SIDE: u32 = 256;
const CENTRES: [Vec3; 3] = [
    Vec3::new(-1.65, 0.0, 0.7),
    Vec3::new(1.85, 0.0, 0.7),
    Vec3::new(0.0, 0.0, -2.8),
];
const TAGS: [&str; 2] = ["glass_sphere", "glass_torus"];

const fn image(name: &'static str) -> EffectInput {
    EffectInput {
        name,
        kind: EffectInputKind::Image,
        description: "Linear probe image.",
        history: 0,
    }
}
const DISTANCE: Effect = Effect {
    id: "gallery.probe_distance",
    label: "probe radial distance",
    description: "World distance to nearest opaque surface; zero for sky.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[EffectInput {
        name: "nearest_depth",
        kind: EffectInputKind::DepthImage,
        description: "Opaque scene depth.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::gallery::probe_distance",
        wxsl: include_str!("probe_distance.wxsl"),
    },
    downlevel: wxsl::render::effect::Downlevel::Portable,
};
const FLIP: Effect = Effect {
    id: "gallery.probe_flip",
    label: "cube face orientation",
    description: "Flip camera V into cube V.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[image("image")],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::gallery::probe_flip",
        wxsl: include_str!("probe_flip.wxsl"),
    },
    downlevel: wxsl::render::effect::Downlevel::Portable,
};
const FLIP_DISTANCE: Effect = Effect {
    id: "gallery.probe_flip_distance",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment_distance",
    },
    ..FLIP
};

const COPY: Effect = Effect {
    id: "gallery.probe_copy",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment_copy",
    },
    ..FLIP
};

struct Cube {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}
impl Cube {
    fn new(gpu: &GpuContext, label: &str, format: wgpu::TextureFormat) -> Self {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: SIDE,
                height: SIDE,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        Self { texture, view }
    }
    fn face(&self, face: u32) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: face,
            array_layer_count: Some(1),
            ..Default::default()
        })
    }
}
struct Probe {
    color: Cube,
    distance: Cube,
}
impl Probe {
    fn new(gpu: &GpuContext) -> Self {
        Self {
            color: Cube::new(
                gpu,
                "local probe radiance",
                wgpu::TextureFormat::Rgba16Float,
            ),
            distance: Cube::new(gpu, "local probe distance", wgpu::TextureFormat::R16Float),
        }
    }
}

fn library() -> wxsl::render::ShaderLibrary {
    let mut library = wxsl::stdlib_library();
    library.insert(
        "package::gallery::probe_glass",
        include_str!("probe_glass.wxsl"),
    );
    library.insert(
        "package::gallery::probe_torus",
        include_str!("probe_torus.wxsl"),
    );
    library.insert(
        "package::gallery::probe_sample",
        include_str!("probe_sample.wxsl"),
    );
    library.insert(
        "package::gallery::probe_mirror",
        include_str!("probe_mirror.wxsl"),
    );
    library
}
fn cube_resource(
    graph: &mut wxsl::render::graph::RenderGraph,
    name: &str,
    format: TextureFormat,
) -> ResourceId {
    graph.resource(
        ResourceDesc::imported(name, format)
            .with_dimension(Dimension::Cube, 6)
            .with_extent(Extent::Fixed {
                width: SIDE,
                height: SIDE,
            }),
    )
}
fn face_camera(origin: Vec3, face: usize) -> Camera {
    let (direction, up) = [
        (Vec3::X, -Vec3::Y),
        (-Vec3::X, -Vec3::Y),
        (Vec3::Y, Vec3::Z),
        (-Vec3::Y, -Vec3::Z),
        (Vec3::Z, -Vec3::Y),
        (-Vec3::Z, -Vec3::Y),
    ][face];
    Camera {
        eye: origin,
        target: origin + direction,
        up,
        fov_y: FRAC_PI_2,
        aspect: 1.0,
        near: 0.02,
        far: 100.0,
        ..Default::default()
    }
}
fn transforms() -> [Mat4; 3] {
    [
        Mat4::from_translation(CENTRES[0]),
        Mat4::from_translation(CENTRES[1]) * Mat4::from_rotation_x(0.3),
        Mat4::from_translation(CENTRES[2]),
    ]
}

fn probe_material(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    color: &wgpu::TextureView,
    distance: &wgpu::TextureView,
    scale: f32,
    owner: usize,
    sky: &wgpu::TextureView,
) -> Result<DemoMaterial, Box<dyn Error>> {
    let mut registry = wxsl::stdlib::registry();
    let function = ["probe_glass", "probe_torus", "probe_mirror"][owner];
    let id = format!("gallery.{function}");
    let mut node = NodeDefinition::builder(&id, "opaque local probe");
    for (name, ty) in [
        ("color", ValueType::TextureCube),
        ("distances", ValueType::TextureCube),
        ("filtering", ValueType::Sampler),
        ("position", ValueType::Vec3),
        ("normal", ValueType::Vec3),
        ("view", ValueType::Vec3),
        ("origin", ValueType::Vec3),
        ("scale", ValueType::F32),
        ("sky", ValueType::TextureCube),
    ] {
        node = node.input(Socket::new(name, ty));
    }
    registry.register(node.output(Socket::new("out", ValueType::Vec3))
        .import(&format!("package::gallery::{function}"), function)
        .expr(format!("{function}({{color}}, {{distances}}, {{filtering}}, {{position}}, {{normal}}, {{view}}, {{origin}}, {{scale}}, {{sky}})")));
    let mut graph = Graph::new("local mirror");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "base_color", Value::Vec3([0.0; 3]));
    graph.set_param(out, "metallic", Value::F32(1.0));
    graph.set_param(out, "roughness", Value::F32(0.0));
    graph.set_param(out, "alpha", Value::F32(1.0));
    graph.set_param(out, "occlusion", Value::F32(0.0)); // Only the explicitly sampled local radiance.
    let sample = graph.add_node(&id);
    graph.set_stage(sample, wxsl::core::graph::StageConstraint::Fragment);
    let color_node = graph.add_node("texture.texture_cube");
    graph.set_setting(color_node, "name", "local_color");
    let distance_node = graph.add_node("texture.texture_cube");
    graph.set_setting(distance_node, "name", "local_distance");
    let sky_node = graph.add_node("texture.texture_cube");
    graph.set_setting(sky_node, "name", "global_sky");
    let sampler = graph.add_node("texture.sampler");
    graph.set_setting(sampler, "name", "local_filter");
    for (id, input) in [
        (color_node, "color"),
        (distance_node, "distances"),
        (sampler, "filtering"),
        (sky_node, "sky"),
    ] {
        graph.wire(&registry, (id, "out"), (sample, input))?;
    }
    for (field, input) in [
        ("world_position", "position"),
        ("world_normal", "normal"),
        ("view_direction", "view"),
    ] {
        let id = graph.add_node(format!("input.{field}"));
        graph.wire(&registry, (id, "out"), (sample, input))?;
    }
    graph.set_param(sample, "origin", Value::Vec3(CENTRES[owner].to_array()));
    graph.set_param(sample, "scale", Value::F32(scale));
    graph.wire(&registry, (sample, "out"), (out, "emissive"))?;
    let material = wxsl::render::Material::with_config(
        &graph,
        &registry,
        &wxsl::render::material::MaterialConfig::default()
            .with_shadows(false, false)
            .with_tags(wxsl::core::scene::Tags::from_iter([
                "opaque",
                if owner < 2 { TAGS[owner] } else { "mirror" },
            ])),
    )?;
    let mut bindings = renderer.material_bindings(&gpu.device, &material);
    bindings.set_texture("local_color", color)?;
    bindings.set_texture("local_distance", distance)?;
    bindings.set_texture("global_sky", sky)?;
    bindings.set_sampler(
        "local_filter",
        &gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }),
    )?;
    bindings.upload(&gpu.device, &gpu.queue)?;
    Ok(DemoMaterial { material, bindings })
}

fn opaque_inputs(graph: &mut wxsl::render::graph::RenderGraph) -> (usize, ResourceId, ResourceId) {
    let output = graph
        .passes()
        .iter()
        .position(|p| matches!(&p.kind, PassKind::Screen { effect } if effect == "wxsl.tonemap"))
        .unwrap();
    let color = graph.passes()[output].reads[0].resource;
    let depth = graph
        .passes()
        .iter()
        .filter_map(|p| p.depth.as_ref().map(|d| d.resource))
        .next_back()
        .unwrap();
    (output, color, depth)
}

// Gather data only: four torus interfaces, no colour resolve or scene composition.
fn append_self_peel(graph: &mut wxsl::render::graph::RenderGraph) -> Result<(), Box<dyn Error>> {
    use wxsl::core::abi::MaterialStage;
    use wxsl::render::types::CompareFunction;
    let empty = graph.resource(ResourceDesc::color(
        "self empty depth",
        TextureFormat::Depth32Float,
    ));
    let mut bounds = graph.resource(ResourceDesc::color(
        "self bounds 0",
        TextureFormat::Rg32Float,
    ));
    graph.pass(
        PassDesc::geometry(
            "self init",
            DrawSource::Scene(TagExpr::Never),
            MaterialStage::FORWARD_LIT,
        )
        .with_depth(DepthAttachment::clear(empty, 1.0))
        .with_color(Attachment::clear(
            bounds,
            Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 0.0,
            },
        )),
    );
    let normals = graph.resource(
        ResourceDesc::color("torus self interfaces", TextureFormat::Rgba32Float)
            .with_dimension(Dimension::D2Array, 4),
    );
    let optics = graph.resource(
        ResourceDesc::color("torus self optics", TextureFormat::Rgba16Float)
            .with_dimension(Dimension::D2Array, 4),
    );
    // Ordinary opaque passes exclude the data-only copy of the torus.
    for i in 0..graph.passes().len() {
        if let PassKind::Geometry { stage, source, .. } = &mut graph.pass_mut(i).unwrap().kind {
            if (*stage == MaterialStage::DEPTH_ONLY || *stage == MaterialStage::FORWARD_LIT)
                && !matches!(source, DrawSource::Scene(TagExpr::Never))
            {
                *source = DrawSource::Scene(TagExpr::parse("opaque && !refractive")?);
            }
        }
        if matches!(&graph.passes()[i].kind, PassKind::Geometry { stage, source, .. } if *stage == MaterialStage::FORWARD_LIT && !matches!(source, DrawSource::Scene(TagExpr::Never)))
        {
            graph.pass_mut(i).unwrap().reads = vec![Read::current(normals)];
        }
    }
    for layer in 0..4 {
        let depth = graph.resource(ResourceDesc::color(
            format!("self depth {layer}"),
            TextureFormat::Depth32Float,
        ));
        let scratch = graph.resource(ResourceDesc::color(
            format!("self scratch {layer}"),
            TextureFormat::Rg32Float,
        ));
        let source = || DrawSource::Scene(TagExpr::parse("glass_torus && refractive").unwrap());
        graph.pass(
            PassDesc::geometry(
                format!("torus self peel {layer}"),
                source(),
                MaterialStage::REFRACTION_DEPTH,
            )
            .with_reads([Read::current(empty), Read::current(bounds)])
            .with_depth(DepthAttachment::clear(depth, 1.0))
            .with_color(Attachment::clear(scratch, Color::TRANSPARENT))
            .with_state(
                PassState::OPAQUE
                    .with_cull_mode(None)
                    .with_depth_test(CompareFunction::Less, true),
            ),
        );
        graph.pass(
            PassDesc::geometry(
                format!("torus self surface {layer}"),
                source(),
                MaterialStage::REFRACTION_SURFACE,
            )
            .with_depth(DepthAttachment::load(depth))
            .with_color(
                Attachment::clear(
                    normals,
                    Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                )
                .with_layer(layer),
            )
            .with_color(Attachment::clear(optics, Color::TRANSPARENT).with_layer(layer))
            .with_state(
                PassState::OPAQUE
                    .with_cull_mode(None)
                    .with_depth_test(CompareFunction::Equal, false),
            ),
        );
        if layer < 3 {
            let next = graph.resource(ResourceDesc::color(
                format!("self bounds {}", layer + 1),
                TextureFormat::Rg32Float,
            ));
            graph.pass(
                PassDesc::screen(
                    format!("torus self advance {layer}"),
                    wxsl::render::effect::refraction::BOUNDS.id,
                )
                .with_reads([Read::current(depth), Read::current(bounds)])
                .with_color(Attachment::clear(next, Color::TRANSPARENT)),
            );
            bounds = next;
        }
    }
    Ok(())
}

pub(super) struct GlassProbes {
    source: wgpu::TextureView,
    sky: bool,
    pub captures: u32,
    #[cfg(test)]
    capture_counts: [[u32; 3]; 2],
    renderer: Renderer,
    probes: [Probe; 3],
    global: Cube,
    mirror: DemoMaterial,
    scale: f32,
    glass: [DemoMaterial; 2],
}
impl GlassProbes {
    pub fn matches_source(&self, stage: &Stage) -> bool {
        self.source == stage.ibl_source.view
            && self.sky == stage.ibl_sky
            && self.scale
                == if stage.ibl_sky {
                    1.0
                } else {
                    stage.ibl_source.scale
                }
    }

    pub fn new(
        stage: &Stage,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self, Box<dyn Error>> {
        let gpu = &stage.gpu;
        let global = Cube::new(
            gpu,
            "probe global environment",
            wgpu::TextureFormat::Rgba16Float,
        );
        let probes = std::array::from_fn(|_| Probe::new(gpu));
        let mut baker = Renderer::new(
            &gpu.device,
            library(),
            TargetConfig::new(SIDE, SIDE, wgpu::TextureFormat::Rgba16Float),
        )?;
        baker.add_effect(wxsl::effects::sky(&wxsl::stdlib::registry())?);
        let mut graph = wxsl::render::graph::RenderGraph::new(wgpu::TextureFormat::Rgba16Float);
        let source = graph.resource(ResourceDesc::imported(
            "probe HDR",
            if stage.ibl_sky {
                TextureFormat::Rgba8Unorm
            } else {
                TextureFormat::Rgba32Float
            },
        ));
        let source = if stage.ibl_sky {
            let sky = graph.resource(
                ResourceDesc::color("probe sky", TextureFormat::Rgba16Float).with_extent(
                    Extent::Fixed {
                        width: 1024,
                        height: 512,
                    },
                ),
            );
            graph.pass(
                PassDesc::screen("probe sky", "wxsl.sky")
                    .with_reads([Read::current(source)])
                    .with_color(Attachment::clear(sky, Color::BLACK)),
            );
            sky
        } else {
            source
        };
        for face in 0..6 {
            // Imported attachment views are host-selected 2D faces, not a
            // sampled Cube view with an attachment-layer selector.
            let target = graph.resource(
                ResourceDesc::imported(format!("probe global {face}"), TextureFormat::Rgba16Float)
                    .with_extent(Extent::Fixed {
                        width: SIDE,
                        height: SIDE,
                    }),
            );
            graph.pass(
                PassDesc::screen(
                    format!("probe global {face}"),
                    wxsl::render::effect::ibl::EQUIRECT_TO_CUBE.id,
                )
                .with_reads([Read::current(source)])
                .with_parameter("face", Value::U32(face))
                .with_color(Attachment::clear(target, Color::BLACK)),
            );
        }
        baker.set_graph(graph)?;
        baker.import_resource(
            "probe HDR",
            if stage.ibl_sky {
                stage.sky_extent.clone()
            } else {
                stage.ibl_source.view.clone()
            },
        );
        for face in 0..6 {
            baker.import_resource(format!("probe global {face}"), global.face(face));
        }
        let empty = DrawList::new();
        baker.render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: &global.face(0),
                environment: &Environment::default(),
                draws: &empty,
            },
        )?;
        let mut renderer = Renderer::new(
            &gpu.device,
            library(),
            TargetConfig::new(width, height, format),
        )?;
        renderer.add_effect(wxsl::effects::tonemap(&wxsl::stdlib::registry())?);
        let scale = if stage.ibl_sky {
            1.0
        } else {
            stage.ibl_source.scale
        };
        let local_mirror = probe_material(
            gpu,
            &mut renderer,
            &probes[2].color.view,
            &probes[2].distance.view,
            1.0,
            2,
            &global.view,
        )?;
        let glass = [
            probe_material(
                gpu,
                &mut renderer,
                &probes[0].color.view,
                &probes[0].distance.view,
                1.0,
                0,
                &global.view,
            )?,
            probe_material(
                gpu,
                &mut renderer,
                &probes[1].color.view,
                &probes[1].distance.view,
                1.0,
                1,
                &global.view,
            )?,
        ];
        let mut result = Self {
            source: stage.ibl_source.view.clone(),
            sky: stage.ibl_sky,
            captures: 0,
            #[cfg(test)]
            capture_counts: [[0; 3]; 2],
            renderer,
            probes,
            global,
            mirror: local_mirror,
            scale,
            glass,
        };
        result.capture(stage)?;
        result.configure()?;
        Ok(result)
    }

    fn draws<'a>(
        stage: &'a Stage,
        mirror: &'a DemoMaterial,
        glass: &'a [DemoMaterial; 2],
        excluded: Option<usize>,
    ) -> DrawList<'a> {
        let matrices = transforms();
        let mut draws = DrawList::new();
        for (i, matrix) in matrices.into_iter().enumerate() {
            if excluded == Some(i) {
                continue;
            }
            let (mesh, material) = match i {
                0 => (&stage.sphere, &glass[0]),
                1 => (&stage.glass_torus, &glass[1]),
                _ => (&stage.sphere, mirror),
            };
            draws.push(
                DrawItem::new(mesh, &material.material)
                    .with_bindings(&material.bindings)
                    .with_transform(matrix),
            );
        }
        // Separate draw used ONLY by self-interface passes, never ordinary visibility.
        if excluded != Some(1) {
            draws.push(
                DrawItem::new(&stage.glass_torus, &stage.glass[1].material)
                    .with_bindings(&stage.glass[1].bindings)
                    .with_transform(matrices[1]),
            );
        }
        draws
    }

    fn capture(&mut self, stage: &Stage) -> Result<(), Box<dyn Error>> {
        self.capture_scene(stage, 7)
    }

    // The mask also lets the regression test isolate each reflected object.
    fn capture_scene(&mut self, stage: &Stage, scene_mask: u8) -> Result<(), Box<dyn Error>> {
        let gpu = &stage.gpu;
        let mut capture = Renderer::new(
            &gpu.device,
            library(),
            TargetConfig::new(SIDE, SIDE, wgpu::TextureFormat::Rgba16Float),
        )?;
        capture.add_effect(DISTANCE);
        capture.add_effect(COPY);
        // First generation samples global radiance; second generation samples
        // the completed first cubes for ALL objects, including the mirror.
        let first: [Probe; 3] = std::array::from_fn(|_| Probe::new(gpu));
        let empty_distance = Cube::new(gpu, "zero sky distance", wgpu::TextureFormat::R16Float);
        let global_mirror = probe_material(
            gpu,
            &mut capture,
            &self.global.view,
            &empty_distance.view,
            1.0,
            2,
            &self.global.view,
        )?;
        let glass = [
            probe_material(
                gpu,
                &mut capture,
                &self.global.view,
                &empty_distance.view,
                1.0,
                0,
                &self.global.view,
            )?,
            probe_material(
                gpu,
                &mut capture,
                &self.global.view,
                &empty_distance.view,
                1.0,
                1,
                &self.global.view,
            )?,
        ];
        let mut graph = capture.render_graph().clone();
        let (output, color, depth) = opaque_inputs(&mut graph);
        let env = cube_resource(&mut graph, "capture global", TextureFormat::Rgba16Float);
        let background = wxsl::render::effect::ibl::append_background(
            &mut graph,
            color,
            depth,
            env,
            "capture background",
        );
        let color = background;
        append_self_peel(&mut graph)?;
        let distance = graph.resource(ResourceDesc::imported(
            "capture distance",
            TextureFormat::R16Float,
        ));
        graph.pass(
            PassDesc::screen("capture distance", DISTANCE.id)
                .with_reads([Read::current(depth)])
                .with_color(Attachment::clear(distance, Color::TRANSPARENT)),
        );
        let present = graph.pass_mut(output).unwrap();
        present.kind = PassKind::Screen {
            effect: COPY.id.into(),
        }; // Linear copy, no display transform in a probe.
        present.reads = vec![Read::current(color)];
        #[cfg(test)]
        tests::assert_opaque_probe_plan(&graph);
        capture.set_graph(graph)?;
        capture.import_resource("capture global", self.global.view.clone());
        let staging = |format| {
            gpu.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("probe camera staging"),
                    size: wgpu::Extent3d {
                        width: SIDE,
                        height: SIDE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let staged_color = staging(wgpu::TextureFormat::Rgba16Float);
        let staged_distance = staging(wgpu::TextureFormat::R16Float);
        capture.import_resource("capture distance", staged_distance.clone());
        let mut flip = Renderer::new(
            &gpu.device,
            library(),
            TargetConfig::new(SIDE, SIDE, wgpu::TextureFormat::Rgba16Float),
        )?;
        flip.add_effect(FLIP);
        flip.add_effect(FLIP_DISTANCE);
        let mut graph = wxsl::render::graph::RenderGraph::new(wgpu::TextureFormat::Rgba16Float);
        let color = graph.resource(ResourceDesc::imported(
            "staged color",
            TextureFormat::Rgba16Float,
        ));
        let distance = graph.resource(ResourceDesc::imported(
            "staged distance",
            TextureFormat::R16Float,
        ));
        let distance_face = graph.resource(ResourceDesc::imported(
            "distance face",
            TextureFormat::R16Float,
        ));
        graph.pass(
            PassDesc::screen("cube color orientation", FLIP.id)
                .with_reads([Read::current(color)])
                .with_color(Attachment::clear(
                    wxsl::render::graph::RenderGraph::TARGET,
                    Color::BLACK,
                )),
        );
        graph.pass(
            PassDesc::screen("cube distance orientation", FLIP_DISTANCE.id)
                .with_reads([Read::current(distance)])
                .with_color(Attachment::clear(distance_face, Color::TRANSPARENT)),
        );
        flip.set_graph(graph)?;
        flip.import_resource("staged color", staged_color.clone());
        flip.import_resource("staged distance", staged_distance);
        for generation in 0..2 {
            let local_glass;
            let local_mirror;
            let (mirror, materials, targets) = if generation == 0 {
                (&global_mirror, &glass, &first)
            } else {
                local_glass = [
                    probe_material(
                        gpu,
                        &mut capture,
                        &first[0].color.view,
                        &first[0].distance.view,
                        1.0,
                        0,
                        &self.global.view,
                    )?,
                    probe_material(
                        gpu,
                        &mut capture,
                        &first[1].color.view,
                        &first[1].distance.view,
                        1.0,
                        1,
                        &self.global.view,
                    )?,
                ];
                local_mirror = probe_material(
                    gpu,
                    &mut capture,
                    &first[2].color.view,
                    &first[2].distance.view,
                    1.0,
                    2,
                    &self.global.view,
                )?;
                (&local_mirror, &local_glass, &self.probes)
            };
            for (owner, origin) in CENTRES.into_iter().enumerate() {
                let all = Self::draws(stage, mirror, materials, Some(owner));
                let draws: DrawList = all
                    .items()
                    .iter()
                    .copied()
                    .filter(|item| {
                        (scene_mask & 1 != 0 || !item.tags.contains(TAGS[0]))
                            && (scene_mask & 2 != 0 || !item.tags.contains(TAGS[1]))
                            && (scene_mask & 4 != 0 || !item.tags.contains("mirror"))
                    })
                    .collect();
                for face in 0..6 {
                    let environment = Environment {
                        camera: face_camera(origin, face as usize),
                        lights: vec![],
                        ambient_sky: Vec3::ZERO,
                        ambient_ground: Vec3::ZERO,
                        ..Default::default()
                    };
                    capture.render(
                        &gpu.device,
                        &gpu.queue,
                        &RenderRequest {
                            view: &staged_color,
                            environment: &environment,
                            draws: &draws,
                        },
                    )?;
                    flip.import_resource("distance face", targets[owner].distance.face(face));
                    flip.render(
                        &gpu.device,
                        &gpu.queue,
                        &RenderRequest {
                            view: &targets[owner].color.face(face),
                            environment: &environment,
                            draws: &DrawList::new(),
                        },
                    )?;
                    self.captures += 1;
                    #[cfg(test)]
                    {
                        self.capture_counts[generation][owner] += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn configure(&mut self) -> Result<(), Box<dyn Error>> {
        let mut graph = self.renderer.render_graph().clone();
        let (output, color, depth) = opaque_inputs(&mut graph);
        let env = cube_resource(&mut graph, "local global", TextureFormat::Rgba16Float);
        let color = wxsl::render::effect::ibl::append_background(
            &mut graph,
            color,
            depth,
            env,
            "local background",
        );
        append_self_peel(&mut graph)?;
        graph.pass_mut(output).unwrap().reads[0] = Read::current(color);
        #[cfg(test)]
        tests::assert_opaque_probe_plan(&graph);
        self.renderer.set_graph(graph)?;
        self.renderer
            .import_resource("local global", self.global.view.clone());
        Ok(())
    }

    pub fn render(
        &mut self,
        stage: &Stage,
        view: &wgpu::TextureView,
        environment: &Environment,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn Error>> {
        if (self.renderer.target().width, self.renderer.target().height) != (width, height) {
            self.renderer.resize(&stage.gpu.device, width, height);
        }
        let environment = Environment {
            // This graph samples application-owned cubes, not frame IBL maps.
            // Restore HDR scaling through exposure, exactly once, for both
            // the glass and mirror's presampled emissive radiance.
            exposure: environment.exposure * self.scale,
            ..environment.clone()
        };
        let draws = Self::draws(stage, &self.mirror, &self.glass, None);
        self.renderer.render(
            &stage.gpu.device,
            &stage.gpu.queue,
            &RenderRequest {
                view,
                environment: &environment,
                draws: &draws,
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn assert_opaque_probe_plan(graph: &wxsl::render::graph::RenderGraph) {
        use wxsl::core::abi::MaterialStage;
        let mut surfaces = 0;
        let mut depths = 0;
        for pass in graph.passes() {
            assert!(pass.state.blend.is_none(), "{}", pass.label);
            assert!(pass.color.iter().all(|target| target.blend.is_none()));
            match &pass.kind {
                PassKind::Geometry { stage, source, .. }
                    if *stage == MaterialStage::REFRACTION_DEPTH
                        || *stage == MaterialStage::REFRACTION_SURFACE =>
                {
                    let DrawSource::Scene(tags) = source else {
                        panic!("self peel must select one object");
                    };
                    assert_eq!(tags, &TagExpr::parse("glass_torus && refractive").unwrap());
                    depths += usize::from(*stage == MaterialStage::REFRACTION_DEPTH);
                    surfaces += usize::from(*stage == MaterialStage::REFRACTION_SURFACE);
                }
                PassKind::Screen { effect } => {
                    assert!(
                        !effect.contains("refraction")
                            || effect == wxsl::render::effect::refraction::BOUNDS.id,
                        "no radiance/depth composition: {effect}"
                    );
                    assert!(!effect.contains("peel_over"));
                }
                _ => {}
            }
        }
        assert_eq!((depths, surfaces), (4, 4));
        assert!(graph
            .resources()
            .iter()
            .all(|r| !r.label.contains("composed") && !r.label.contains("local glass")));
    }

    #[test]
    fn objects_form_a_disjoint_triangle_and_shader_bounds_match() {
        let a = CENTRES[1] - CENTRES[0];
        let b = CENTRES[2] - CENTRES[0];
        assert!(a.cross(b).length() > 5.0);
        let radii = [0.86, 1.54, 0.86];
        let shader = include_str!("probe_sample.wxsl");
        let bounds: Vec<Vec<f32>> = shader
            .lines()
            .filter_map(|line| {
                let literal = line.trim().strip_prefix("vec4f(")?.split_once(')')?.0;
                Some(
                    literal
                        .split(',')
                        .map(|v| v.trim().parse().unwrap())
                        .collect(),
                )
            })
            .collect();
        assert_eq!(bounds.len(), 3);
        for i in 0..3 {
            let p = CENTRES[i];
            assert_eq!(
                bounds[i],
                [p.x, p.y, p.z, radii[i]],
                "shader broad-phase bound must track centre {i}"
            );
            for j in 0..i {
                let distance = (CENTRES[i] - CENTRES[j]).length();
                assert!(distance > radii[i] + radii[j]);
                assert!(distance + radii[j] < 6.0);
                assert!(distance + radii[i] < 6.0);
            }
        }
    }

    #[test]
    fn capture_cameras_match_webgpu_cube_faces() {
        let uv = glam::Vec2::new(0.31, 0.72);
        let p = uv * 2.0 - glam::Vec2::ONE;
        let rays = [
            Vec3::new(1.0, -p.y, -p.x),
            Vec3::new(-1.0, -p.y, p.x),
            Vec3::new(p.x, 1.0, p.y),
            Vec3::new(p.x, -1.0, -p.y),
            Vec3::new(p.x, -p.y, 1.0),
            Vec3::new(-p.x, -p.y, -1.0),
        ];
        for (face, ray) in rays.iter().enumerate() {
            let camera = face_camera(CENTRES[0], face);
            let h = camera.view_proj() * (CENTRES[0] + ray.normalize() * 3.0).extend(1.0);
            let ndc = h.truncate() / h.w;
            // The capture's colour and distance V are flipped together.
            let projected = glam::Vec2::new(ndc.x * 0.5 + 0.5, ndc.y * 0.5 + 0.5);
            assert!(
                (projected - uv).length() < 1e-5,
                "face {face}: {projected:?}"
            );
        }
    }

    fn stage(width: u32, height: u32) -> Option<(Stage, OffscreenTarget)> {
        let gpu = match pollster::block_on(GpuContext::headless()) {
            Ok(gpu) => gpu,
            Err(error) => {
                eprintln!("skipping GPU test: {error}");
                return None;
            }
        };
        let target = OffscreenTarget::new(&gpu.device, width, height);
        let stage = Stage::new(
            gpu,
            TargetConfig::new(width, height, target.format()),
            "sky",
        )
        .unwrap();
        Some((stage, target))
    }

    #[test]
    fn probes_are_cached_during_orbit_resize_and_demo_switches() {
        let Some((mut stage, target)) = stage(192, 144) else {
            return;
        };
        let demos = demos();
        let demo = demos.iter().find(|d| d.name == "glass-probes").unwrap();
        stage
            .render(demo, target.view(), 192, 144, 0.0, 0.0)
            .unwrap();
        stage.gpu.wait();
        let first = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let cached = stage.probes.as_ref().unwrap();
        assert_eq!(cached.captures, 36);
        assert_eq!(cached.capture_counts, [[6; 3]; 2]);
        assert_opaque_probe_plan(cached.renderer.render_graph());
        for layer in 0..4 {
            assert_eq!(
                cached
                    .renderer
                    .pass_draw_count(&format!("torus self peel {layer}")),
                Some(1)
            );
            assert_eq!(
                cached
                    .renderer
                    .pass_draw_count(&format!("torus self surface {layer}")),
                Some(1)
            );
        }
        for glass in &cached.glass {
            assert!(glass.material.tags().contains("opaque"));
            assert!(!glass.material.tags().contains("refractive"));
            assert!(!glass.material.tags().contains("transparent"));
        }
        let misses = cached.renderer.cache_stats().misses;
        let cube = cached.probes[2].color.view.clone();
        for owner in 0..3 {
            let draws = GlassProbes::draws(&stage, &cached.mirror, &cached.glass, Some(owner));
            assert_eq!(draws.len(), if owner == 1 { 2 } else { 3 });
            assert!(!draws
                .items()
                .iter()
                .any(|d| d.transform == transforms()[owner]));
        }
        let original = demo_environment(demo, 4.0 / 3.0, 0.0).camera;
        for eye in [Vec3::new(0.5, 1.4, 8.2), Vec3::new(0.0, 1.4, -8.2)] {
            stage.camera_override = Some((Camera { eye, ..original }, None));
            stage
                .render(demo, target.view(), 192, 144, 0.0, 0.0)
                .unwrap();
        }
        let mirror_demo = demos.iter().find(|d| d.name == "ibl-mirror").unwrap();
        stage
            .render(mirror_demo, target.view(), 192, 144, 0.0, 0.0)
            .unwrap();
        stage.camera_override = None;
        stage
            .render(demo, target.view(), 192, 144, 0.0, 0.0)
            .unwrap();
        stage.gpu.wait();
        assert_eq!(
            first,
            target.read_rgba8(&stage.gpu.device, &stage.gpu.queue)
        );
        let resized = OffscreenTarget::new(&stage.gpu.device, 256, 192);
        stage
            .render(demo, resized.view(), 256, 192, 0.0, 0.0)
            .unwrap();
        let cached = stage.probes.as_ref().unwrap();
        assert_eq!(cached.captures, 36);
        assert_eq!(cached.renderer.cache_stats().misses, misses);
        assert_eq!(cached.probes[2].color.view, cube);
        stage.ibl_source = wxsl::render::environment::upload_environment_image(
            &stage.gpu.device,
            &stage.gpu.queue,
            1,
            1,
            &[[1.0, 0.5, 0.25]],
        )
        .unwrap();
        stage.ibl_sky = false;
        stage
            .render(demo, resized.view(), 256, 192, 0.0, 0.0)
            .unwrap();
        let new = stage.probes.as_ref().unwrap();
        assert_eq!(new.captures, 36);
        assert_ne!(new.probes[2].color.view, cube);
    }

    #[test]
    fn opaque_probes_are_independent_of_draw_submission_order() {
        let Some((stage, target)) = stage(256, 192) else {
            return;
        };
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        let environment = demo_environment(&demo, 4.0 / 3.0, 0.0);
        let mut probes = GlassProbes::new(&stage, 256, 192, target.format()).unwrap();
        probes
            .render(&stage, target.view(), &environment, 256, 192)
            .unwrap();
        stage.gpu.wait();
        let baseline = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let draws = GlassProbes::draws(&stage, &probes.mirror, &probes.glass, None);
        let reversed: DrawList = draws.items().iter().rev().copied().collect();
        let environment = Environment {
            exposure: environment.exposure * probes.scale,
            ..environment
        };
        probes
            .renderer
            .render(
                &stage.gpu.device,
                &stage.gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: &environment,
                    draws: &reversed,
                },
            )
            .unwrap();
        stage.gpu.wait();
        assert_eq!(
            baseline,
            target.read_rgba8(&stage.gpu.device, &stage.gpu.queue)
        );
    }

    #[test]
    fn probe_intersections_use_exit_position_and_misses_use_global_sky() {
        use wxsl::render::effect::EffectParameter;
        let Some((stage, target)) = stage(8, 8) else {
            return;
        };
        let gpu = &stage.gpu;
        const BAKE: &str = r#"
@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return vec4f(f32(i32(i & 1u) * 4 - 1), f32(i32(i >> 1u) * 4 - 1), 0.0, 1.0);
}
fn wall(p: vec4f) -> vec4f {
    let uv = p.xy / 256.0 * 2.0 - 1.0;
    var ray = vec3f(1.0, -uv.y, -uv.x);
    switch params.face {
        case 1u: { ray = vec3f(-1.0, -uv.y, uv.x); }
        case 2u: { ray = vec3f(uv.x, 1.0, uv.y); }
        case 3u: { ray = vec3f(uv.x, -1.0, -uv.y); }
        case 4u: { ray = vec3f(uv.x, -uv.y, 1.0); }
        case 5u: { ray = vec3f(-uv.x, -uv.y, -1.0); }
        default: {}
    }
    if ray.x <= 0.0 { return vec4f(0.0); }
    let point = ray * (2.0 / ray.x);
    if abs(point.y) >= 1.0 || abs(point.z) >= 1.0 { return vec4f(0.0); }
    return vec4f(point, length(point));
}
@fragment fn color(@builtin(position) p: vec4f) -> @location(0) vec4f {
    let hit = wall(p);
    if hit.w == 0.0 { return vec4f(0.0, 1.0, 0.0, 1.0); }
    return vec4f((hit.y + 1.0) * 0.5, 0.0, 0.0, 1.0);
}
@fragment fn distance(@builtin(position) p: vec4f) -> @location(0) f32 { return wall(p).w; }
@fragment fn sky(@builtin(position) p: vec4f) -> @location(0) vec4f { return vec4f(0.0, 0.0, 1.0, 1.0); }
"#;
        const QUERY: &str = r#"
import package::gallery::probe_sample::probe_sample;
import package::wxsl::bindings::environment_lut_sampler;
@group(3) @binding(0) var color: texture_cube<f32>;
@group(3) @binding(1) var distances: texture_cube<f32>;
@group(3) @binding(2) var sky: texture_cube<f32>;
@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return vec4f(f32(i32(i & 1u) * 4 - 1), f32(i32(i >> 1u) * 4 - 1), 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) p: vec4f) -> @location(0) vec4f {
    return vec4f(probe_sample(color, distances, environment_lut_sampler,
        params.position, normalize(params.ray), vec3f(0.0), sky), 1.0);
}
"#;
        let mut renderer = Renderer::new(
            &gpu.device,
            library(),
            TargetConfig::new(SIDE, SIDE, target.format()),
        )
        .unwrap();
        let color = Cube::new(gpu, "test wall color", wgpu::TextureFormat::Rgba16Float);
        let distance = Cube::new(gpu, "test wall depth", wgpu::TextureFormat::R16Float);
        let sky = Cube::new(gpu, "test global sky", wgpu::TextureFormat::Rgba16Float);
        for (id, entry) in [
            ("gallery.test_wall_color", "color"),
            ("gallery.test_wall_distance", "distance"),
            ("gallery.test_wall_sky", "sky"),
        ] {
            renderer.add_effect(Effect {
                id,
                label: "test wall",
                description: "Analytic finite wall for ray-query regression.",
                kind: EffectKind::Screen {
                    vertex_entry: "vertex",
                    fragment_entry: entry,
                },
                inputs: &[],
                outputs: &[],
                parameters: &[EffectParameter {
                    name: "face",
                    default: Value::U32(0),
                }],
                shader: EffectShader::Source {
                    path: "package::gallery::test_wall",
                    wxsl: BAKE,
                },
                downlevel: wxsl::render::effect::Downlevel::Portable,
            });
        }
        let mut graph = wxsl::render::graph::RenderGraph::new(target.format());
        for (cube, effect, format) in [
            (
                &color,
                "gallery.test_wall_color",
                TextureFormat::Rgba16Float,
            ),
            (
                &distance,
                "gallery.test_wall_distance",
                TextureFormat::R16Float,
            ),
            (&sky, "gallery.test_wall_sky", TextureFormat::Rgba16Float),
        ] {
            for face in 0..6 {
                let label = format!("{effect} {face}");
                let resource = graph.resource(ResourceDesc::imported(&label, format).with_extent(
                    Extent::Fixed {
                        width: SIDE,
                        height: SIDE,
                    },
                ));
                graph.pass(
                    PassDesc::screen(&label, effect)
                        .with_parameter("face", Value::U32(face))
                        .with_color(Attachment::clear(resource, Color::BLACK)),
                );
                renderer.import_resource(label, cube.face(face));
            }
        }
        renderer.set_graph(graph).unwrap();
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: &Environment::default(),
                    draws: &DrawList::new(),
                },
            )
            .unwrap();
        for effect in [
            "gallery.test_wall_color",
            "gallery.test_wall_distance",
            "gallery.test_wall_sky",
        ] {
            for face in 0..6 {
                renderer.remove_import(&format!("{effect} {face}"));
            }
        }
        renderer.add_effect(Effect {
            id: "gallery.test_probe_query",
            label: "probe ray query",
            description: "Intersect a controlled finite wall.",
            kind: EffectKind::Screen {
                vertex_entry: "vertex",
                fragment_entry: "fragment",
            },
            inputs: {
                const INPUTS: &[EffectInput] = &[image("color"), image("distance"), image("sky")];
                INPUTS
            },
            outputs: &[],
            parameters: &[
                EffectParameter {
                    name: "position",
                    default: Value::Vec3([0.0; 3]),
                },
                EffectParameter {
                    name: "ray",
                    default: Value::Vec3([1.0, 0.0, 0.0]),
                },
            ],
            shader: EffectShader::Source {
                path: "package::gallery::test_probe_query",
                wxsl: QUERY,
            },
            downlevel: wxsl::render::effect::Downlevel::Portable,
        });
        let mut graph = wxsl::render::graph::RenderGraph::new(target.format());
        let reads = [
            cube_resource(&mut graph, "query color", TextureFormat::Rgba16Float),
            cube_resource(&mut graph, "query distance", TextureFormat::R16Float),
            cube_resource(&mut graph, "query sky", TextureFormat::Rgba16Float),
        ];
        graph.pass(
            PassDesc::screen("query", "gallery.test_probe_query")
                .with_reads(reads.map(Read::current))
                .with_color(Attachment::clear(
                    wxsl::render::graph::RenderGraph::TARGET,
                    Color::BLACK,
                )),
        );
        renderer.set_graph(graph).unwrap();
        renderer.import_resource("query color", color.view);
        renderer.import_resource("query distance", distance.view);
        renderer.import_resource("query sky", sky.view);
        renderer.resize(&gpu.device, 8, 8);
        // A redirected ray whose angular lookup sees sky, but which hits the
        // wall from the displaced exit point; the old reprojection misses it.
        for (position, ray, expected) in [
            ([0.0, 1.0, 0.0], [1.0, -0.7, 0.0], [77u8, 0, 0]),
            ([0.0, -0.6, 0.0], [1.0, 0.0, 0.0], [51, 0, 0]),
            ([0.0, 0.6, 0.0], [1.0, 0.0, 0.0], [204, 0, 0]),
            ([0.0, 2.0, 0.0], [1.0, 0.0, 0.0], [0, 0, 255]),
            ([0.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0, 0, 255]),
        ] {
            renderer
                .set_pass_param("query", "position", Value::Vec3(position))
                .unwrap();
            renderer
                .set_pass_param("query", "ray", Value::Vec3(ray))
                .unwrap();
            renderer
                .render(
                    &gpu.device,
                    &gpu.queue,
                    &RenderRequest {
                        view: target.view(),
                        environment: &Environment::default(),
                        draws: &DrawList::new(),
                    },
                )
                .unwrap();
            stage.gpu.wait();
            let pixel = target.read_rgba8(&gpu.device, &gpu.queue);
            assert!(
                (0..3).all(|c| pixel[c].abs_diff(expected[c]) < 8),
                "{position:?} / {ray:?}: {:?}, expected {expected:?}",
                &pixel[..3]
            );
        }
    }

    #[test]
    fn refracted_torus_moves_with_the_camera_without_recapturing_probes() {
        let Some((stage, target)) = stage(320, 240) else {
            return;
        };
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        let mut probes = GlassProbes::new(&stage, 320, 240, target.format()).unwrap();
        let initial = probes.captures;
        let mut pictures = [Vec::new(), Vec::new()];
        let mut cameras = [demo_environment(&demo, 4.0 / 3.0, 0.0).camera; 2];
        cameras[1].eye.z = -cameras[1].eye.z;
        for (i, camera) in cameras.iter().enumerate() {
            let environment = Environment {
                camera: *camera,
                lights: vec![],
                ambient_sky: Vec3::ZERO,
                ambient_ground: Vec3::ZERO,
                ..Default::default()
            };
            probes
                .render(&stage, target.view(), &environment, 320, 240)
                .unwrap();
            stage.gpu.wait();
            pictures[i] = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        }
        assert_eq!(probes.captures, initial);
        // Isolate the torus's contribution to the sphere, not the moving HDR
        // or the directly visible torus. Compare normalized sphere coordinates.
        probes.capture_scene(&stage, 5).unwrap();
        let mut centroids = Vec::new();
        for (i, camera) in cameras.iter().enumerate() {
            let environment = Environment {
                camera: *camera,
                lights: vec![],
                ambient_sky: Vec3::ZERO,
                ambient_ground: Vec3::ZERO,
                ..Default::default()
            };
            probes
                .render(&stage, target.view(), &environment, 320, 240)
                .unwrap();
            stage.gpu.wait();
            let without = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
            let project = |p: Vec3| {
                let h = camera.view_proj() * p.extend(1.0);
                glam::Vec2::new(
                    (h.x / h.w * 0.5 + 0.5) * 320.0,
                    (0.5 - h.y / h.w * 0.5) * 240.0,
                )
            };
            let centre = project(CENTRES[0]);
            let right = (camera.target - camera.eye).cross(camera.up).normalize();
            let radius = (project(CENTRES[0] + right * 0.85) - centre).length();
            let mut weighted = glam::Vec2::ZERO;
            let mut total = 0.0;
            for (pixel, (a, b)) in pictures[i]
                .chunks_exact(4)
                .zip(without.chunks_exact(4))
                .enumerate()
            {
                let p = (glam::Vec2::new((pixel % 320) as f32 + 0.5, (pixel / 320) as f32 + 0.5)
                    - centre)
                    / radius;
                if p.length() < 0.85 {
                    let difference = (0..3).map(|c| a[c].abs_diff(b[c]) as f32).sum::<f32>();
                    if difference > 12.0 {
                        weighted += p * difference;
                        total += difference;
                    }
                }
            }
            assert!(
                total > 100.0,
                "torus must affect sphere from view {i}: {total}"
            );
            centroids.push(weighted / total);
        }
        assert!(
            (centroids[0] - centroids[1]).length() > 0.1,
            "{centroids:?}"
        );
    }

    #[test]
    fn glass_probes_include_the_mirror_in_both_generations() {
        let Some((stage, target)) = stage(320, 240) else {
            return;
        };
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        let environment = demo_environment(&demo, 4.0 / 3.0, 0.0);
        let mut probes = GlassProbes::new(&stage, 320, 240, target.format()).unwrap();
        assert_eq!(probes.capture_counts, [[6; 3]; 2]);
        probes
            .render(&stage, target.view(), &environment, 320, 240)
            .unwrap();
        stage.gpu.wait();
        let baseline = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        // Keep the mirror in the main view but remove it from both capture
        // generations: only its contribution through the glass can change.
        probes.capture_scene(&stage, 3).unwrap();
        assert_eq!(probes.capture_counts, [[12; 3]; 2]);
        probes
            .render(&stage, target.view(), &environment, 320, 240)
            .unwrap();
        stage.gpu.wait();
        let without = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let project = |point: Vec3| {
            let h = environment.camera.view_proj() * point.extend(1.0);
            glam::Vec2::new(
                (h.x / h.w * 0.5 + 0.5) * 320.0,
                (0.5 - h.y / h.w * 0.5) * 240.0,
            )
        };
        for owner in 0..2 {
            let centre = project(CENTRES[owner]);
            let extent = if owner == 0 { 0.75 } else { 1.5 };
            let radius = (project(CENTRES[owner] + Vec3::X * extent) - centre).length();
            let changed = baseline
                .chunks_exact(4)
                .zip(without.chunks_exact(4))
                .enumerate()
                .filter(|(i, (a, b))| {
                    let p = glam::Vec2::new((i % 320) as f32 + 0.5, (i / 320) as f32 + 0.5);
                    (p - centre).length() < radius && (0..3).any(|c| a[c].abs_diff(b[c]) > 8)
                })
                .count();
            assert!(
                changed > 4,
                "glass object {owner} must see mirror: {changed}"
            );
        }
    }

    #[test]
    fn mirror_probe_reflects_both_glass_objects_not_just_the_hdr() {
        let Some((stage, target)) = stage(320, 240) else {
            return;
        };
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        let environment = demo_environment(&demo, 4.0 / 3.0, 0.0);
        let mut probes = GlassProbes::new(&stage, 320, 240, target.format()).unwrap();
        probes
            .render(&stage, target.view(), &environment, 320, 240)
            .unwrap();
        stage.gpu.wait();
        let baseline = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let project = |point: Vec3| {
            let h = environment.camera.view_proj() * point.extend(1.0);
            glam::Vec2::new(
                (h.x / h.w * 0.5 + 0.5) * 320.0,
                (0.5 - h.y / h.w * 0.5) * 240.0,
            )
        };
        let centre = project(CENTRES[2]);
        let radius = (project(CENTRES[2] + Vec3::X * 0.85) - centre).length() * 0.93;
        for (mask, omitted) in [(6, "sphere"), (5, "torus")] {
            probes.capture_scene(&stage, mask).unwrap();
            probes
                .render(&stage, target.view(), &environment, 320, 240)
                .unwrap();
            stage.gpu.wait();
            let without = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
            let changed = baseline
                .chunks_exact(4)
                .zip(without.chunks_exact(4))
                .enumerate()
                .filter(|(i, (a, b))| {
                    let pixel = glam::Vec2::new((i % 320) as f32 + 0.5, (i / 320) as f32 + 0.5);
                    (pixel - centre).length() < radius && (0..3).any(|c| a[c].abs_diff(b[c]) > 8)
                })
                .count();
            assert!(
                changed > 4,
                "mirror must reflect {omitted}: {changed} changed pixels"
            );
        }
    }

    #[test]
    fn hdr_safety_scale_is_restored_once_in_glass_and_mirror() {
        let Some((mut stage, target)) = stage(192, 144) else {
            return;
        };
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        stage.ibl_sky = false;
        let mut pictures = Vec::new();
        for (radiance, scale) in [([0.0625, 0.125, 0.25], 4.0), ([0.25, 0.5, 1.0], 1.0)] {
            stage.ibl_source = wxsl::render::environment::upload_environment_image(
                &stage.gpu.device,
                &stage.gpu.queue,
                1,
                1,
                &[radiance],
            )
            .unwrap();
            stage.ibl_source.scale = scale;
            stage
                .render(&demo, target.view(), 192, 144, 0.0, 0.0)
                .unwrap();
            stage.gpu.wait();
            pictures.push(target.read_rgba8(&stage.gpu.device, &stage.gpu.queue));
        }
        assert_eq!(
            pictures[0], pictures[1],
            "the same physical radiance must produce the same image"
        );
        stage.ibl_source.scale = 2.0;
        assert!(!stage.probes.as_ref().unwrap().matches_source(&stage));
    }

    #[test]
    fn glass_probe_preserves_the_spheres_lens_inversion() {
        let Some((mut stage, target)) = stage(320, 240) else {
            return;
        };
        // An asymmetric environment: red above the horizon, blue below.
        let pixels: Vec<_> = (0..128)
            .flat_map(|y| {
                (0..256).map(move |_| {
                    if y < 64 {
                        [1.0, 0.02, 0.02]
                    } else {
                        [0.02, 0.02, 1.0]
                    }
                })
            })
            .collect();
        stage.ibl_source = wxsl::render::environment::upload_environment_image(
            &stage.gpu.device,
            &stage.gpu.queue,
            256,
            128,
            &pixels,
        )
        .unwrap();
        stage.ibl_sky = false;
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        let mut camera = demo_environment(&demo, 4.0 / 3.0, 0.0).camera;
        camera.eye.y = 0.0;
        stage.camera_override = Some((camera, None));
        stage
            .render(&demo, target.view(), 320, 240, 0.0, 0.0)
            .unwrap();
        stage.gpu.wait();
        let pixels = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let project = |point: Vec3| {
            let h = camera.view_proj() * point.extend(1.0);
            glam::Vec2::new(
                (h.x / h.w * 0.5 + 0.5) * 320.0,
                (0.5 - h.y / h.w * 0.5) * 240.0,
            )
        };
        let centre = project(CENTRES[0]);
        let radius = (project(CENTRES[0] + Vec3::Y * 0.85) - centre).length();
        let mut sums = [[0u32; 3]; 2];
        let mut counts = [0u32; 2];
        for (i, pixel) in pixels.chunks_exact(4).enumerate() {
            let offset = glam::Vec2::new((i % 320) as f32 + 0.5, (i / 320) as f32 + 0.5) - centre;
            if offset.x.abs() < radius * 0.3
                && offset.y.abs() > radius * 0.3
                && offset.y.abs() < radius * 0.6
            {
                let half = usize::from(offset.y > 0.0);
                counts[half] += 1;
                for c in 0..3 {
                    sums[half][c] += u32::from(pixel[c]);
                }
            }
        }
        assert!(counts.iter().all(|n| *n > 20));
        assert!(
            sums[0][2] > sums[0][0] + counts[0] * 30,
            "top of glass must show blue bottom: {sums:?}"
        );
        assert!(
            sums[1][0] > sums[1][2] + counts[1] * 30,
            "bottom of glass must show red top: {sums:?}"
        );
    }

    #[test]
    #[ignore = "manual GPU timing of cached local probes"]
    fn local_probe_timing() {
        let Some((mut stage, target)) = stage(1280, 720) else {
            return;
        };
        let demo = demos()
            .into_iter()
            .find(|d| d.name == "glass-probes")
            .unwrap();
        let started = Instant::now();
        stage
            .render(&demo, target.view(), 1280, 720, 0.0, 0.0)
            .unwrap();
        stage.gpu.wait();
        println!(
            "initial capture + frame: {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        let misses = stage.probes.as_ref().unwrap().renderer.cache_stats().misses;
        let started = Instant::now();
        for _ in 0..10 {
            stage
                .render(&demo, target.view(), 1280, 720, 0.0, 0.0)
                .unwrap();
            stage.gpu.wait();
        }
        println!(
            "cached probes: {:.2} ms/frame",
            started.elapsed().as_secs_f64() * 100.0
        );
        let probes = stage.probes.as_ref().unwrap();
        assert_eq!(probes.captures, 36);
        assert_eq!(probes.renderer.cache_stats().misses, misses);
    }
}
