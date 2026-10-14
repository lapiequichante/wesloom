//! The demo materials' graphs, textures, bindings and draws.

use super::*;

/// A procedural 64x64 warm checker, in linear space, for whatever the
/// material's graph declares — the same supply-what-was-declared move
/// `pbr_cube` makes.
pub(crate) fn demo_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::Sampler) {
    const SIDE: u32 = 64;
    let mut texels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let square = ((x / 8) + (y / 8)) % 2 == 0;
            let grain = (((x * 7 + y * 13) % 11) as f32 / 11.0 - 0.5) * 0.06;
            let base = if square { 0.82 } else { 0.30 };
            let level = ((base + grain).clamp(0.0, 1.0) * 255.0) as u8;
            texels.extend_from_slice(&[
                level,
                (level as f32 * 0.94) as u8,
                (level as f32 * 0.86) as u8,
                255,
            ]);
        }
    }
    let extent = wgpu::Extent3d {
        width: SIDE,
        height: SIDE,
        depth_or_array_layers: 1,
    };
    // The full mip chain, box-filtered on the CPU: the floors are seen at
    // grazing angles, and a mip-less checker minified past one texel per
    // square hatches the ground with moiré. The side rides alongside the
    // texels rather than being re-derived (an integer square root would
    // spend MSRV the workspace has not adopted).
    let mut levels = vec![(SIDE as usize, texels)];
    while levels.last().unwrap().0 > 1 {
        let (prev_side, prev) = levels.last().unwrap();
        let (prev_side, prev) = (*prev_side, prev.as_slice());
        let side = (prev_side / 2).max(1);
        let mut next = Vec::with_capacity(side * side * 4);
        for y in 0..side {
            for x in 0..side {
                for c in 0..4 {
                    let sample = |yy: usize, xx: usize| {
                        prev[(yy.min(prev_side - 1) * prev_side + xx.min(prev_side - 1)) * 4 + c]
                            as u32
                    };
                    let y2 = y * 2;
                    let x2 = x * 2;
                    let sum = sample(y2, x2)
                        + sample(y2, x2 + 1)
                        + sample(y2 + 1, x2)
                        + sample(y2 + 1, x2 + 1);
                    next.push((sum / 4) as u8);
                }
            }
        }
        levels.push((side, next));
    }
    let mip_count = levels.len() as u32;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gallery checker"),
        size: extent,
        mip_level_count: mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (side, texels)) in levels.iter().enumerate() {
        let side = *side as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            texels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(side * 4),
                rows_per_image: Some(side),
            },
            wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
        );
    }
    (
        texture.create_view(&wgpu::TextureViewDescriptor::default()),
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("gallery sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        }),
    )
}

/// Bind everything the material's graph declared, by name.
pub(crate) fn demo_bindings(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    material: &wxsl::render::Material,
    texture: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> Result<wxsl::render::MaterialBindings, Box<dyn Error>> {
    let mut bindings = renderer.material_bindings(device, material);
    for resource in &material.interface().resources {
        let name = resource.name.as_str();
        if resource.ty == wxsl::core::node::ValueType::Sampler {
            bindings.set_sampler(name, sampler)?;
        } else {
            bindings.set_texture(name, texture)?;
        }
    }
    bindings.upload(device, queue)?;
    Ok(bindings)
}

impl BakeStage {
    /// Parse the bake material, compile it, create the table its
    /// declaration names — the host owns the texture; the pass list only
    /// writes through it — and bind the material's declared resources to
    /// it. The declaration is the one place size and precision live, so
    /// the texture is created from it and the pipeline's `resource.color`
    /// is expected to agree.
    pub(crate) fn new(gpu: &GpuContext, renderer: &mut Renderer) -> Result<Self, Box<dyn Error>> {
        let graph = bake_material_graph();
        let decl = graph
            .bake("roughness_bake")
            .expect("the demo material declares its table");
        let material = wxsl::render::Material::with_lighting(
            &graph,
            &wxsl::stdlib::registry(),
            &wxsl::render::material::MaterialConfig::default(),
            renderer.lighting(),
        )?;
        let format = wxsl::render::pipeline::gbuffer_format(decl.precision);
        let [width, height] = decl.size;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("roughness bake table"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // Written by the bake pass as storage, sampled by the material
            // as a texture — one texture, both halves of the bake.
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("roughness bake sampler"),
            // The domain is 0..1 — the table covers exactly that — so an
            // out-of-range uv clamps to the edge rather than tiling noise
            // that was never baked.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut bindings = renderer.material_bindings(&gpu.device, &material);
        for resource in &material.interface().resources {
            let name = resource.name.as_str();
            if resource.bake.is_some() && resource.ty == wxsl::core::node::ValueType::Sampler {
                bindings.set_sampler(name, &sampler)?;
            } else if resource.bake.is_some() {
                bindings.set_texture(name, &view)?;
            }
        }
        bindings.upload(&gpu.device, &gpu.queue)?;
        Ok(BakeStage {
            material,
            bindings,
            view,
        })
    }
}

/// One distinct tint per copy, hue-swept; a single copy stays white, the
/// identity for the graph's multiply.
pub(crate) fn instance_tints(count: u32) -> Vec<InstanceAttributes> {
    let count = count.max(1);
    (0..count)
        .map(|index| {
            let tint = if count == 1 {
                Vec3::ONE
            } else {
                let hue = index as f32 / count as f32 * std::f32::consts::TAU;
                Vec3::new(hue.cos(), (hue + 2.09).cos(), (hue + 4.19).cos()) * 0.4 + 0.6
            };
            InstanceAttributes::new().with("instance_tint", Value::Vec3(tint.to_array()))
        })
        .collect()
}

/// The PBR cube graph, plus an `alpha` uniform.
///
/// The cube's surface output leaves `alpha` at its default of 1, which is
/// inlined. A `param.value` is a uniform the host can move without
/// recompiling; this demo pins it at 0.3 and tags the material
/// `transparent`, which is the only tag `pass.peel` draws.
pub(crate) fn peel_material_graph(base: &Graph) -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = base.clone();
    graph.set_name("peel surfaces");
    let output = graph
        .nodes()
        .find(|(_, node)| node.def == abi::SURFACE_OUTPUT_ID)
        .map(|(id, _)| id)
        .expect("the PBR cube graph ends at output.surface");
    let alpha = graph.add(
        Node::new("param.value")
            .with_label("alpha")
            .with_setting("name", "alpha")
            .with_param("value", Value::F32(0.3)),
    );
    graph
        .set_generic(&registry, alpha, "T", ValueType::F32)
        .map_err(|error| format!("the alpha uniform does not resolve: {error}"))?;
    graph
        .wire(&registry, (alpha, "out"), (output, "alpha"))
        .map_err(|error| format!("the alpha uniform does not wire: {error}"))?;
    graph
        .validate(&registry)
        .map_err(|error| format!("the peel material does not validate: {error}"))?;
    Ok(graph)
}

pub(crate) fn cube_transform(time: f32) -> Mat4 {
    Mat4::from_rotation_y(time * 0.45) * Mat4::from_rotation_x(time * 0.21)
}

/// The fragment-only family as one surface (plan5 D6): `checker_aa`
/// carries the pattern, and an SDF disc's alpha takes its width from
/// `screen_width` — the exact caller-side recipe `sdf_coverage`'s doc
/// describes. Blended by the demo's document, so the disc's edge is real
/// coverage, not a hard step.
pub(crate) fn sdf_aa_material_graph() -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("sdf aa");
    let uv = graph.add_node("input.uv");
    let checker = graph
        .add(Node::new("generative.checker_aa").with_param("cells", Value::Vec2([24.0, 24.0])));
    let pattern = graph.add_node("convert.splat");
    let centre = graph.add(Node::new("const.value").with_param("value", Value::Vec2([0.5, 0.5])));
    let p = graph.add_node("math.subtract");
    let circle = graph.add(Node::new("sdf.circle").with_param("radius", Value::F32(0.36)));
    let width = graph.add_node("math.screen_width");
    let coverage = graph.add_node("sdf.coverage");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .set_generic(&registry, pattern, "T", ValueType::Vec3)
        .map_err(|error| format!("the splat does not resolve: {error}"))?;
    for side in ["A", "B"] {
        graph
            .set_generic(&registry, p, side, ValueType::Vec2)
            .map_err(|error| format!("the subtract does not resolve: {error}"))?;
    }
    for (from, to) in [
        ((uv, "out"), (checker, "uv")),
        ((checker, "out"), (pattern, "value")),
        ((uv, "out"), (p, "a")),
        ((centre, "out"), (p, "b")),
        ((p, "out"), (circle, "p")),
        ((circle, "out"), (width, "value")),
        ((circle, "out"), (coverage, "distance")),
        ((width, "out"), (coverage, "width")),
        ((pattern, "out"), (output, "base_color")),
        ((coverage, "out"), (output, "alpha")),
    ] {
        graph
            .wire(&registry, from, to)
            .map_err(|error| format!("the sdf-aa graph does not wire: {error}"))?;
    }
    graph
        .validate(&registry)
        .map_err(|error| format!("the sdf-aa material does not validate: {error}"))?;
    Ok(graph)
}

/// A thin-film sphere whose graph authors thickness, not reflected colour.
pub(crate) fn iridescent_material_graph() -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("iridescence");
    // The thickness field lives in *object* space — sampled from the
    // world position it would stay put while the sphere turns underneath
    // it, and the film would appear to swim across the surface instead of
    // rotating with it — and only the vertex stage has object space, so
    // the field crosses through a declared interpolant (ADR 0027).
    let position = graph.add_node("input.object_position");
    let scale = graph.add(Node::new("math.multiply").with_param("b", Value::Vec3([1.1, 1.1, 1.1])));
    let noise = graph.add_node("generative.simplex3");
    let thickness = graph.add(
        Node::new("math.remap")
            .with_param("in_min", Value::F32(-1.0))
            .with_param("in_max", Value::F32(1.0))
            .with_param("out_min", Value::F32(240.0))
            .with_param("out_max", Value::F32(880.0)),
    );
    graph.declare_attribute(AttributeDecl::computed("film_thickness", ValueType::F32));
    let varying =
        graph.add(Node::new(abi::VARYING_OUTPUT_ID).with_setting("name", "film_thickness"));
    let thickness_in =
        graph.add(Node::new("input.attribute").with_setting("name", "film_thickness"));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3([0.42, 0.38, 0.36]));
    graph.set_param(output, "metallic", Value::F32(0.85));
    graph.set_param(output, "roughness", Value::F32(0.3));
    graph.set_param(output, "iridescence_strength", Value::F32(1.0));
    for side in ["A", "B"] {
        graph
            .set_generic(&registry, scale, side, ValueType::Vec3)
            .map_err(|error| format!("the scale does not resolve: {error}"))?;
    }
    graph
        .set_generic(&registry, thickness, "T", ValueType::F32)
        .map_err(|error| format!("the remap does not resolve: {error}"))?;
    for interpolant in [&varying, &thickness_in] {
        graph
            .set_generic(&registry, *interpolant, "T", ValueType::F32)
            .map_err(|error| format!("the interpolant does not resolve: {error}"))?;
    }
    for (from, to) in [
        ((position, "out"), (scale, "a")),
        ((scale, "out"), (noise, "p")),
        ((noise, "out"), (thickness, "value")),
        ((thickness, "out"), (varying, abi::SOCKET_VARYING)),
        ((thickness_in, "out"), (output, "iridescence_thickness")),
    ] {
        graph
            .wire(&registry, from, to)
            .map_err(|error| format!("the iridescence graph does not wire: {error}"))?;
    }
    graph
        .validate(&registry)
        .map_err(|error| format!("the iridescent material does not validate: {error}"))?;
    Ok(graph)
}

/// The motion-blur demo's path: a sweep across the frame, fast enough
/// that one frame's motion is a visible smear, and slow enough to stay
/// on screen.
pub(crate) fn swept_transform(time: f32) -> Mat4 {
    let travel = (time * 0.9).sin() * 2.2;
    Mat4::from_translation(Vec3::new(travel, 0.0, 0.0)) * Mat4::from_rotation_y(time * 1.8)
}

/// How a frame's cube moves: the demo's motion, if it has one, and the
/// frame step its previous-frame transform answers for — one bundle so
/// the draw-list builder stays readable.
#[derive(Clone, Copy)]
pub(crate) struct MotionPlan {
    pub(crate) motion: Option<Motion>,
    pub(crate) step: f32,
}

pub(crate) fn cube_draws<'a>(
    mesh: &'a Mesh,
    material: &'a wxsl::render::Material,
    bindings: &'a wxsl::render::MaterialBindings,
    tints: &'a [InstanceAttributes],
    count: u32,
    time: f32,
    plan: MotionPlan,
) -> DrawList<'a> {
    (0..count.max(1))
        .map(|index| {
            let offset = index as f32 - (count.max(1) - 1) as f32 * 0.5;
            let place = Mat4::from_translation(Vec3::new(offset * 2.4, 0.0, 0.0));
            let mut item = DrawItem::new(mesh, material)
                .with_transform(match plan.motion {
                    Some(motion) => place * (motion.path)(time),
                    None => place * cube_transform(time),
                })
                .with_bindings(bindings);
            // Where the cube was one frame ago, for the velocity stage's
            // previous-instance row. Only a demo whose cube *moves*
            // states it: every other draw stays `None`, which the frame
            // group reads as this frame's own transform — zero motion,
            // and the behaviour every frame had before the row existed.
            if let Some(motion) = plan.motion {
                item = item.with_previous(place * (motion.path)((time - plan.step).max(0.0)));
            }
            if let Some(tint) = tints.get(index as usize) {
                item = item.with_attributes(tint);
            }
            item
        })
        .collect()
}
