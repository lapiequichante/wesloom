//! The `area-texture` and `neon` demos (ADR 0095): a rectangle emitting a
//! texture drawn in WXSL, and tubes, neither casting a shadow, over a
//! glossy floor that shows their reflections; the `headlights` demo, two
//! spots shaped by an IES profile (ADR 0097); plus the light textures
//! (`light_textures.wxsl`, `resources/ies/headlight.ies`) every textured light of the
//! gallery reads.

use std::borrow::Cow;

use wxsl::render::environment::IesProfile;

use super::*;

const TEXTURES_MODULE: &str = "package::gallery::light_textures";
const COOKIE_SIZE: u32 = 512;
const COOKIE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The fragment entries of `light_textures.wxsl`, in layer order.
const COOKIE_LAYERS: [&str; 3] = ["window", "panel", "neon"];
pub(crate) const WINDOW_LAYER: u32 = 0;
const PANEL_LAYER: u32 = 1;
const NEON_LAYER: u32 = 2;
/// The layer after the drawn ones: `resources/ies/headlight.ies`, baked
/// on the CPU.
const HEADLIGHT_LAYER: u32 = 3;

/// A non-negative value at most 1 as f16 bits, truncated; below f16's
/// smallest normal it is zero.
fn f16_bits(value: f32) -> u16 {
    if value < 6.1e-5 {
        return 0;
    }
    let bits = value.min(1.0).to_bits();
    let exponent = ((bits >> 23) & 0xff) + 15 - 127;
    ((exponent << 10) | ((bits >> 13) & 0x3ff)) as u16
}

/// The cookie array, and single-layer views of it for the materials that
/// draw the emitters themselves.
pub(crate) struct LightTextures {
    pub(crate) array: wgpu::TextureView,
    panel: wgpu::TextureView,
    neon: wgpu::TextureView,
}

/// Draw `light_textures.wxsl` into a mipmapped cookie array, once.
pub(crate) fn light_textures(gpu: &GpuContext) -> Result<LightTextures, Box<dyn Error>> {
    let device = &gpu.device;
    let wgsl = wxsl::render::variants::compile(
        &wxsl::stdlib_library(),
        &[(
            TEXTURES_MODULE,
            Cow::Borrowed(include_str!("light_textures.wxsl")),
        )],
        TEXTURES_MODULE,
        &wxsl::core::macros::MacroSet::new(),
    )?;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gallery light textures"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(wgsl)),
    });
    let levels = COOKIE_SIZE.ilog2() + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gallery light textures"),
        size: wgpu::Extent3d {
            width: COOKIE_SIZE,
            height: COOKIE_SIZE,
            depth_or_array_layers: COOKIE_LAYERS.len() as u32 + 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COOKIE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pipeline = |entry: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(entry),
                targets: &[Some(COOKIE_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
    };
    let level_view = |layer: u32, level: u32| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_mip_level: level,
            mip_level_count: Some(1),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    };
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gallery light texture downsample"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let downsample = pipeline("downsample");
    let mut encoder = device.create_command_encoder(&Default::default());
    let mut draw = |pipeline: &wgpu::RenderPipeline,
                    target: &wgpu::TextureView,
                    group: Option<&wgpu::BindGroup>| {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gallery light texture"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        if let Some(group) = group {
            pass.set_bind_group(0, group, &[]);
        }
        pass.draw(0..3, 0..1);
    };
    // The profile's first level is uploaded, white in every channel so
    // that it also reads as a mask; its mips come from the same chain.
    let profile = IesProfile::parse(include_str!("../../../../resources/ies/headlight.ies"))?;
    let texels: Vec<u8> = profile
        .texels(COOKIE_SIZE, COOKIE_SIZE)
        .into_iter()
        .flat_map(|value| {
            let bits = f16_bits(value).to_le_bytes();
            [bits, bits, bits, f16_bits(1.0).to_le_bytes()].concat()
        })
        .collect();
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: HEADLIGHT_LAYER,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(COOKIE_SIZE * 8),
            rows_per_image: Some(COOKIE_SIZE),
        },
        wgpu::Extent3d {
            width: COOKIE_SIZE,
            height: COOKIE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    for layer in 0..=HEADLIGHT_LAYER {
        if let Some(entry) = COOKIE_LAYERS.get(layer as usize) {
            draw(&pipeline(entry), &level_view(layer, 0), None);
        }
        for level in 1..levels {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("gallery light texture level"),
                layout: &downsample.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&level_view(layer, level - 1)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            draw(&downsample, &level_view(layer, level), Some(&group));
        }
    }
    gpu.queue.submit([encoder.finish()]);
    let layer_view = |layer: u32| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    };
    Ok(LightTextures {
        array: texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        }),
        panel: layer_view(PANEL_LAYER),
        neon: layer_view(NEON_LAYER),
    })
}

/// Where the floor of both demos sits.
const FLOOR: f32 = -1.6;
/// The panel's radiance at full texture.
const PANEL_RADIANCE: f32 = 2.0;
/// The neon tubes' radius.
const TUBE_RADIUS: f32 = 0.035;

/// The textured panel: upright at the back left, turned towards the
/// objects. u runs to a viewer's right, v up.
fn panel_light() -> Light {
    let facing = Vec3::new(1.0, 0.0, 0.8).normalize();
    Light::rectangle(
        Vec3::new(-2.6, 0.15, -1.6),
        Vec3::new(facing.z, 0.0, -facing.x) * 2.0,
        Vec3::new(0.0, 1.2, 0.0),
        Vec3::ONE,
        PANEL_RADIANCE,
    )
    .with_cookie(PANEL_LAYER)
}

/// The neon sign: (centre, half-axis, radiance colour), and whether the
/// tube carries the gradient instead of a colour.
fn neon_tubes() -> [(Vec3, Vec3, Vec3, bool); 6] {
    let wall = -2.9;
    [
        // A frame: magenta top, cyan sides, and the gradient along the base.
        (
            Vec3::new(0.0, 1.9, wall),
            Vec3::new(2.2, 0.0, 0.0),
            Vec3::new(6.0, 0.4, 2.6),
            false,
        ),
        (
            Vec3::new(-2.3, 0.65, wall),
            Vec3::new(0.0, 1.15, 0.0),
            Vec3::new(0.3, 3.4, 6.0),
            false,
        ),
        (
            Vec3::new(2.3, 0.65, wall),
            Vec3::new(0.0, 1.15, 0.0),
            Vec3::new(0.3, 3.4, 6.0),
            false,
        ),
        (
            Vec3::new(0.0, -0.6, wall),
            Vec3::new(2.2, 0.0, 0.0),
            Vec3::splat(5.0),
            true,
        ),
        // A slanted amber stroke across the frame, and a low violet bar
        // in front, close enough to the floor to streak it.
        (
            Vec3::new(0.2, 0.7, wall + 0.05),
            Vec3::new(1.1, 0.55, 0.0),
            Vec3::new(6.0, 2.6, 0.4),
            false,
        ),
        (
            Vec3::new(-0.4, FLOOR + 0.35, 0.6),
            Vec3::new(1.2, 0.0, 0.5),
            Vec3::new(2.4, 0.6, 6.0),
            false,
        ),
    ]
}

/// The lights of `demo`, if it is one of this module's.
pub(crate) fn lights(demo: &str) -> Option<Vec<Light>> {
    match demo {
        "area-texture" => Some(vec![panel_light()]),
        "neon" => Some(
            neon_tubes()
                .into_iter()
                .map(|(centre, axis, color, gradient)| {
                    let light = Light::tube(centre, axis, TUBE_RADIUS, color, 1.0);
                    if gradient {
                        light.with_cookie(NEON_LAYER)
                    } else {
                        light
                    }
                })
                .collect(),
        ),
        "headlights" => Some(
            headlights()
                .into_iter()
                .map(|position| {
                    Light::spot(
                        position,
                        HEADLIGHT_AIM,
                        40.0_f32.to_radians(),
                        48.0_f32.to_radians(),
                        Vec3::new(1.0, 0.95, 0.88),
                        HEADLIGHT_INTENSITY,
                    )
                    .with_profile(HEADLIGHT_LAYER)
                    .casting_shadow(8.0)
                    .with_source_radius(HEADLIGHT_RADIUS)
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Where the car's front sits, facing -z.
const CAR_FRONT: f32 = 0.0;
/// Straight ahead and a touch down, as headlights are aimed; the profile
/// holds the beam's shape and its cutoff.
const HEADLIGHT_AIM: Vec3 = Vec3::new(0.0, -0.012, -1.0);
/// The profile's brightest direction, in the gallery's units.
const HEADLIGHT_INTENSITY: f32 = 1500.0;
/// The lamp's lens, as the source its shadows soften from (ADR 0098).
const HEADLIGHT_RADIUS: f32 = 0.09;
/// How far ahead the end wall stands.
const ROAD_END: f32 = -26.0;

/// The two headlights, left then right.
fn headlights() -> [Vec3; 2] {
    [-0.62, 0.62].map(|x| Vec3::new(x, FLOOR + 0.68, CAR_FRONT - 0.04))
}

/// The camera of `demo`, if it is one of this module's.
pub(crate) fn camera(demo: &str, aspect: f32) -> Option<Camera> {
    let (eye, target) = match demo {
        "area-texture" => (Vec3::new(2.6, 1.0, 6.2), Vec3::new(-0.6, -0.6, -0.8)),
        "neon" => (Vec3::new(-1.4, 0.4, 6.6), Vec3::new(0.0, 0.0, -1.2)),
        "headlights" => (Vec3::new(0.6, 2.4, 10.5), Vec3::new(0.0, FLOOR, -12.0)),
        _ => return None,
    };
    Some(Camera {
        eye,
        target,
        aspect,
        ..Camera::default()
    })
}

/// A floor and wall that keep a sharp enough reflection to show what the
/// lights' samples follow.
fn glossy_graph(name: &str, base: f32, roughness: f32, metallic: f32) -> Graph {
    let mut graph = Graph::new(name);
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(out, "base_color", Value::Vec3([base; 3]));
    graph.set_param(out, "roughness", Value::F32(roughness));
    graph.set_param(out, "metallic", Value::F32(metallic));
    graph
}

/// An emitter drawing `scale` times its texture at uv, unlit.
fn textured_emitter_graph(name: &str, scale: f32) -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new(name);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let sampler = graph.add(Node::new("texture.sampler").with_setting("name", "linear"));
    let texture = graph.add(Node::new("texture.texture_2d").with_setting("name", "emission"));
    let sample = graph.add_node("sample.texture_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let gain = graph.add(Node::new("const.value").with_param("value", Value::Vec3([scale; 3])));
    let scaled = graph.add_node("math.multiply");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    for side in ["A", "B"] {
        graph
            .set_generic(&registry, scaled, side, ValueType::Vec3)
            .map_err(|error| format!("the emitter's gain does not resolve: {error}"))?;
    }
    graph.set_param(output, "base_color", Value::Vec3([0.0; 3]));
    graph.set_param(output, "roughness", Value::F32(1.0));
    let mut wires = vec![
        ((texture, "out"), (sample, "tex")),
        ((sampler, "out"), (sample, "samp")),
        ((uv, "out"), (sample, "uv")),
        ((sample, "out"), (split, "v")),
        ((color, "out"), (scaled, "a")),
        ((gain, "out"), (scaled, "b")),
        ((scaled, "out"), (output, "emissive")),
    ];
    for channel in ["x", "y", "z"] {
        wires.push(((split, channel), (color, channel)));
    }
    for (from, to) in wires {
        graph
            .wire(&registry, from, to)
            .map_err(|error| format!("the emitter graph does not wire: {error}"))?;
    }
    Ok(graph)
}

/// An unlit emitter of one radiance.
fn emitter_graph(name: &str, radiance: [f32; 3]) -> Graph {
    let mut graph = Graph::new(name);
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(out, "base_color", Value::Vec3([0.0; 3]));
    graph.set_param(out, "roughness", Value::F32(1.0));
    graph.set_param(out, "emissive", Value::Vec3(radiance));
    graph
}

/// An emitter whose radiance is its instance's tint.
fn tinted_emitter_graph() -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("neon tube");
    graph.declare_attribute(AttributeDecl::instance("instance_tint", ValueType::Vec3));
    let tint = graph.add(Node::new("input.attribute").with_setting("name", "instance_tint"));
    graph
        .set_generic(&registry, tint, "T", ValueType::Vec3)
        .map_err(|error| format!("the tube's tint does not resolve: {error}"))?;
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3([0.0; 3]));
    graph.set_param(output, "roughness", Value::F32(1.0));
    graph
        .wire(&registry, (tint, "out"), (output, "emissive"))
        .map_err(|error| format!("the tube graph does not wire: {error}"))?;
    Ok(graph)
}

/// A cylinder along x from -1 to 1, `radius` thick, u along its length.
fn tube_mesh(device: &wgpu::Device, radius: f32) -> Mesh {
    const AROUND: u32 = 24;
    let mut vertices = Vec::new();
    for end in 0..2u32 {
        for step in 0..=AROUND {
            let angle = step as f32 / AROUND as f32 * core::f32::consts::TAU;
            let normal = Vec3::new(0.0, angle.cos(), angle.sin());
            vertices.push(wxsl::render::Vertex {
                position: (Vec3::X * (end as f32 * 2.0 - 1.0) + normal * radius).to_array(),
                normal: normal.to_array(),
                tangent: [1.0, 0.0, 0.0, 1.0],
                uv: [end as f32, step as f32 / AROUND as f32],
            });
        }
    }
    let stride = AROUND + 1;
    let indices: Vec<u32> = (0..AROUND)
        .flat_map(|step| {
            let (a, b) = (step, step + 1);
            let (c, d) = (a + stride, b + stride);
            // Counter-clockwise seen from outside.
            [a, b, d, a, d, c]
        })
        .collect();
    Mesh::new(device, "gallery neon tube", &vertices, &indices)
}

/// A tube's transform: the unit mesh stretched and turned onto `axis`.
fn tube_transform(centre: Vec3, axis: Vec3) -> Mat4 {
    Mat4::from_translation(centre)
        * Mat4::from_quat(glam::Quat::from_rotation_arc(Vec3::X, axis.normalize()))
        * Mat4::from_scale(Vec3::new(axis.length(), 1.0, 1.0))
}

/// The stage's meshes these demos borrow.
pub(crate) struct Shared<'a> {
    pub(crate) floor: &'a Mesh,
    pub(crate) cube: &'a Mesh,
    pub(crate) sphere: &'a Mesh,
    pub(crate) plate: &'a Mesh,
}

/// What the two demos draw beside their lights.
pub(crate) struct LightStage {
    floor: DemoMaterial,
    wall: DemoMaterial,
    panel: DemoMaterial,
    neon: DemoMaterial,
    tube: DemoMaterial,
    chrome: DemoMaterial,
    plaster: DemoMaterial,
    tube_mesh: Mesh,
    tints: Vec<InstanceAttributes>,
    asphalt: DemoMaterial,
    body: DemoMaterial,
    lamp: DemoMaterial,
    road: Mesh,
    block: Mesh,
    ball: Mesh,
}

impl LightStage {
    pub(crate) fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        textures: &LightTextures,
        texture: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> Result<Self, Box<dyn Error>> {
        let lit = wxsl::render::material::MaterialConfig::default();
        let unlit = wxsl::render::material::MaterialConfig::default().with_shadows(false, false);
        let mut material =
            |view: &wgpu::TextureView,
             graph: &Graph,
             config: &wxsl::render::material::MaterialConfig| {
                DemoMaterial::new(gpu, renderer, view, sampler, graph, config)
            };
        let tint = |rgb: Vec3| {
            InstanceAttributes::new().with("instance_tint", Value::Vec3(rgb.to_array()))
        };
        Ok(LightStage {
            floor: material(
                texture,
                &glossy_graph("glossy floor", 0.18, 0.15, 0.0),
                &lit,
            )?,
            wall: material(texture, &glossy_graph("neon wall", 0.35, 0.6, 0.0), &lit)?,
            chrome: material(texture, &glossy_graph("chrome", 0.95, 0.12, 1.0), &lit)?,
            plaster: material(texture, &glossy_graph("plaster", 0.75, 0.55, 0.0), &lit)?,
            panel: material(
                &textures.panel,
                &textured_emitter_graph("area panel", PANEL_RADIANCE)?,
                &unlit,
            )?,
            neon: material(
                &textures.neon,
                &textured_emitter_graph("neon gradient", 5.0)?,
                &unlit,
            )?,
            tube: material(texture, &tinted_emitter_graph()?, &unlit)?,
            tube_mesh: tube_mesh(&gpu.device, TUBE_RADIUS),
            tints: neon_tubes()
                .iter()
                .map(|(_, _, color, _)| tint(*color))
                .collect(),
            asphalt: material(texture, &glossy_graph("asphalt", 0.09, 0.4, 0.0), &lit)?,
            body: material(texture, &glossy_graph("car body", 0.05, 0.3, 0.0), &lit)?,
            lamp: material(texture, &emitter_graph("headlamp", [9.0, 8.6, 8.0]), &unlit)?,
            road: Mesh::plane(&gpu.device, 60.0),
            block: Mesh::cube(&gpu.device, 1.0),
            ball: Mesh::sphere(&gpu.device, 1.0),
        })
    }

    /// The draws of `demo`: floor, two objects, and the emitters' own
    /// geometry, which is where the lights are.
    pub(crate) fn draws<'a>(&'a self, stage: Shared<'a>, demo: &str, time: f32) -> DrawList<'a> {
        if demo == "headlights" {
            return self.headlight_draws();
        }
        let floor = Mat4::from_translation(Vec3::new(0.0, FLOOR, 0.0));
        let (cube, sphere) = if demo == "area-texture" {
            (
                Vec3::new(1.2, FLOOR + 0.75, -1.4),
                Vec3::new(-1.9, FLOOR + 0.6, 1.3),
            )
        } else {
            (
                Vec3::new(-2.3, FLOOR + 0.75, -0.3),
                Vec3::new(2.2, FLOOR + 0.6, 0.4),
            )
        };
        let mut draws: DrawList = [
            DrawItem::new(stage.floor, &self.floor.material)
                .with_bindings(&self.floor.bindings)
                .with_transform(floor),
            DrawItem::new(stage.cube, &self.plaster.material)
                .with_transform(
                    Mat4::from_translation(cube) * Mat4::from_rotation_y(time * 0.4 + 0.5),
                )
                .with_bindings(&self.plaster.bindings),
            DrawItem::new(stage.sphere, &self.chrome.material)
                .with_transform(Mat4::from_translation(sphere))
                .with_bindings(&self.chrome.bindings),
        ]
        .into_iter()
        .collect();
        if demo == "area-texture" {
            let light = panel_light();
            // The plate is 3.4 wide: x onto the u edge, z down the v edge,
            // so its uv is the light's.
            draws.push(
                DrawItem::new(stage.plate, &self.panel.material)
                    .with_bindings(&self.panel.bindings)
                    .with_transform(Mat4::from_cols(
                        (light.area_u * (2.0 / 3.4)).extend(0.0),
                        light.area_u.cross(light.area_v).normalize().extend(0.0),
                        (-light.area_v * (2.0 / 3.4)).extend(0.0),
                        light.position_or_direction.extend(1.0),
                    )),
            );
        } else {
            draws.push(
                DrawItem::new(stage.floor, &self.wall.material)
                    .with_bindings(&self.wall.bindings)
                    .with_transform(
                        Mat4::from_translation(Vec3::new(0.0, FLOOR, -3.0))
                            * Mat4::from_rotation_x(FRAC_PI_2)
                            * Mat4::from_translation(Vec3::new(0.0, 0.0, -7.0)),
                    ),
            );
            for ((centre, axis, _, gradient), tint) in neon_tubes().into_iter().zip(&self.tints) {
                let surface = if gradient { &self.neon } else { &self.tube };
                draws.push(
                    DrawItem::new(&self.tube_mesh, &surface.material)
                        .with_bindings(&surface.bindings)
                        .with_transform(tube_transform(centre, axis))
                        .with_attributes(tint),
                );
            }
        }
        draws
    }

    /// A road at night: the car's dark body with its lamps, posts down
    /// both sides that throw long shadows, a sphere on the beam's hot
    /// spot and a wall at the end where the cutoff line shows.
    fn headlight_draws(&self) -> DrawList<'_> {
        fn item<'a>(mesh: &'a Mesh, surface: &'a DemoMaterial, transform: Mat4) -> DrawItem<'a> {
            DrawItem::new(mesh, &surface.material)
                .with_bindings(&surface.bindings)
                .with_transform(transform)
        }
        let block =
            |centre: Vec3, size: Vec3| Mat4::from_translation(centre) * Mat4::from_scale(size);
        let mut draws = DrawList::new();
        draws.push(item(
            &self.road,
            &self.asphalt,
            Mat4::from_translation(Vec3::new(0.0, FLOOR, ROAD_END + 22.0)),
        ));
        draws.push(item(
            &self.block,
            &self.body,
            block(
                Vec3::new(0.0, FLOOR + 0.75, CAR_FRONT + 2.1),
                Vec3::new(1.8, 1.0, 4.2),
            ),
        ));
        for light in headlights() {
            draws.push(item(
                &self.ball,
                &self.lamp,
                block(light, Vec3::new(0.16, 0.09, 0.03)),
            ));
        }
        for step in 0..7 {
            let z = CAR_FRONT - 5.0 - step as f32 * 3.4;
            for x in [-2.7, 2.7] {
                draws.push(item(
                    &self.block,
                    &self.plaster,
                    block(Vec3::new(x, FLOOR + 0.55, z), Vec3::new(0.18, 1.1, 0.18)),
                ));
            }
        }
        draws.push(item(
            &self.ball,
            &self.chrome,
            block(
                Vec3::new(0.9, FLOOR + 0.5, CAR_FRONT - 9.0),
                Vec3::splat(0.5),
            ),
        ));
        draws.push(item(
            &self.block,
            &self.wall,
            block(
                Vec3::new(0.0, FLOOR + 3.0, ROAD_END),
                Vec3::new(30.0, 6.0, 0.4),
            ),
        ));
        draws
    }
}
