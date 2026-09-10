//! What a material declares it needs from outside itself, checked against a
//! real GPU: uniform parameters, textures and samplers, and the block the
//! application supplies (ADR 0023).
//!
//! Every test here skips rather than fails without an adapter, like
//! `render_cube.rs`. What they are for is the class of bug that only a GPU
//! finds: a computed offset the shader reads somewhere else, a bind group
//! layout that does not match the generated declarations, a texture bound
//! at the wrong index.
//!
//! # Why the scene is unlit
//!
//! Every graph below drives `emissive` with no lights and no ambient, and
//! turns the tonemap off. What lands in the framebuffer is then
//! `srgb(emissive)` and nothing else, so a pixel is a readable answer
//! rather than a lighting result to eyeball.

use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node, UserBlockDecl, UserField};
use wxsl::core::node::{NodeRegistry, Value, ValueType};
use wxsl::render::gpu::GpuContext;
use wxsl::render::wgpu;
use wxsl::render::{DrawItem, RenderRequest};

mod probe;
use probe::{close, declare_as_parameter, gpu, probe_graph, probe_values, srgb, unlit, Harness};

// ---------------------------------------------------------------------
// Uniform parameters
// ---------------------------------------------------------------------

/// A graph whose emissive is one `vec3f` parameter called `tint`.
fn tint_graph() -> Graph {
    let mut graph = Graph::new("tint");
    let tint = graph.add(
        Node::new("param.value")
            .with_setting("name", "tint")
            .with_param("value", Value::Vec3([0.25, 0.5, 0.75])),
    );
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    let registry = wxsl::stdlib::registry();
    graph
        .wire(&registry, (tint, "out"), (output, "emissive"))
        .expect("vec3f into emissive");
    graph
}

#[test]
fn a_parameter_reaches_the_shader_at_the_value_the_graph_declared() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let graph = tint_graph();
    let material = harness.material(&graph);

    // The layout is in the generated source, so it is in the variant key.
    assert!(
        material
            .wxsl(abi::MaterialStage::FORWARD_LIT)
            .contains("tint: vec3f"),
        "{}",
        material.wxsl(abi::MaterialStage::FORWARD_LIT)
    );

    let mut bindings = harness.bindings(&material);
    bindings
        .upload(&harness.gpu.device, &harness.gpu.queue)
        .expect("nothing is unbound");
    let pixel = harness.shade(&material, Some(&bindings), None);
    assert!(
        close(pixel[0], srgb(0.25)) && close(pixel[1], srgb(0.5)) && close(pixel[2], srgb(0.75)),
        "the declared default did not reach the shader: {pixel:?}"
    );
}

#[test]
fn changing_a_parameter_costs_a_buffer_write_and_not_a_variant() {
    // The acceptance test of the whole milestone: a slider that does not
    // recompile. A `const` node here would compile a new variant on every
    // drag, which is exactly the difference `param` exists to draw.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&tint_graph());
    let mut bindings = harness.bindings(&material);
    bindings
        .upload(&harness.gpu.device, &harness.gpu.queue)
        .expect("nothing is unbound");
    let before = harness.shade(&material, Some(&bindings), None);

    let stats = harness.renderer.cache_stats();
    let pipelines = harness.renderer.pipeline_count();

    for step in 1..=8u32 {
        let level = step as f32 / 8.0;
        bindings
            .set("tint", Value::Vec3([level, level, level]))
            .expect("declared");
        bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .expect("nothing is unbound");
        let pixel = harness.shade(&material, Some(&bindings), None);
        assert!(
            close(pixel[0], srgb(level)),
            "step {step} did not reach the shader: {pixel:?}"
        );
    }

    let after = harness.renderer.cache_stats();
    assert_eq!(
        after.misses,
        stats.misses,
        "dragging a parameter compiled {} new shader variants",
        after.misses - stats.misses
    );
    assert_eq!(
        harness.renderer.pipeline_count(),
        pipelines,
        "dragging a parameter built a new pipeline"
    );
    // And it really did change something, so the assertion above is not
    // passing because nothing happened.
    let last = harness.shade(&material, Some(&bindings), None);
    assert_ne!(before, last);
}

// ---------------------------------------------------------------------
// The computed layout, at every type
// ---------------------------------------------------------------------

#[test]
fn every_parameter_type_arrives_where_the_computed_layout_says() {
    // The layouts elsewhere in this repo are two halves — a `#[repr(C)]`
    // struct and a WGSL struct — checked against each other by a test on
    // their sizes (ADR 0008). This one has no second half to check
    // against, so it is checked against the GPU instead: a shader
    // comparing what it read with a literal the compiler inlined.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    for (ty, value) in probe_values() {
        let graph = probe_graph(&harness.registry, ty, value, &declare_as_parameter);
        let material = harness.material(&graph);
        let mut bindings = harness.bindings(&material);
        bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .expect("nothing is unbound");
        // The host side of the same round trip, through the same offsets.
        assert_eq!(bindings.get("probe"), Some(value), "{ty} on the host");
        let pixel = harness.shade(&material, Some(&bindings), None);
        assert!(
            pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200,
            "{ty} did not arrive intact in the shader: {pixel:?}"
        );
    }
}

#[test]
fn a_parameter_set_after_the_fact_arrives_at_every_type() {
    // The same round trip, but with the value written by the host rather
    // than declared by the graph — which is the path a slider takes, and
    // the one where an offset computed twice would show up.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    for (ty, value) in probe_values() {
        // Built with a *wrong* value, then corrected through `set`.
        let wrong = ty.splat(0.0).expect("every value type splats");
        let graph = probe_graph(&harness.registry, ty, wrong, &declare_as_parameter);
        let material = harness.material(&graph);
        let mut bindings = harness.bindings(&material);
        bindings.set("probe", value).expect("declared");
        bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .expect("nothing is unbound");
        let pixel = harness.shade(&material, Some(&bindings), None);
        // The graph compares against the *wrong* value it was built with,
        // so a correct write makes the comparison fail: black is the pass.
        assert!(
            pixel[0] < 40 && pixel[1] < 40 && pixel[2] < 40,
            "{ty} was not overwritten by the host: {pixel:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Textures and samplers
// ---------------------------------------------------------------------

/// A 2x2 texture with one distinctive texel per corner, in linear space.
fn checker(gpu: &GpuContext) -> wgpu::TextureView {
    let texels: [u8; 16] = [
        255, 0, 0, 255, // top left
        0, 255, 0, 255, // top right
        0, 0, 255, 255, // bottom left
        255, 255, 255, 255, // bottom right
    ];
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("checker"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Not `Srgb`: the ABI encodes sRGB itself at the end of shading,
        // so what a material samples is linear like everything else.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// A graph that samples `albedo` at the surface's UV and emits it.
fn sampled_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::new("sampled");
    let texture = graph.add(Node::new("texture.texture_2d").with_setting("name", "albedo"));
    let sampler = graph.add(Node::new("texture.sampler").with_setting("name", "clamped"));
    let uv = graph.add_node("input.uv");
    let sample = graph.add_node("sample.texture_2d");
    let split = graph.add_node("convert.split.vec4f");
    let combine = graph.add_node("convert.combine.vec3f");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);

    graph
        .wire(registry, (texture, "out"), (sample, "tex"))
        .expect("a texture");
    graph
        .wire(registry, (sampler, "out"), (sample, "samp"))
        .expect("a sampler");
    graph
        .wire(registry, (uv, "out"), (sample, "uv"))
        .expect("a vec2f");
    graph
        .wire(registry, (sample, "out"), (split, "v"))
        .expect("a vec4f");
    for channel in ["x", "y", "z"] {
        let field = match channel {
            "x" => "x",
            "y" => "y",
            _ => "z",
        };
        graph
            .wire(registry, (split, channel), (combine, field))
            .expect("a component");
    }
    graph
        .wire(registry, (combine, "out"), (output, "emissive"))
        .expect("vec3f into emissive");
    graph
}

#[test]
fn a_graph_samples_a_real_texture() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let graph = sampled_graph(&harness.registry);
    let material = harness.material(&graph);

    // Two bindings after the parameter slot, in name order.
    let interface = material.interface();
    assert_eq!(interface.resources.len(), 2);
    assert_eq!(interface.resource("albedo").expect("declared").binding, 1);
    assert_eq!(interface.resource("clamped").expect("declared").binding, 2);
    assert!(
        interface.params.is_empty(),
        "this graph declares no parameters, so there is no buffer"
    );

    let mut bindings = harness.bindings(&material);
    // Nothing bound yet: reported by name rather than drawn as black.
    let error = bindings
        .upload(&harness.gpu.device, &harness.gpu.queue)
        .expect_err("the texture is unbound");
    assert!(error.to_string().contains("albedo"), "{error}");

    let view = checker(&harness.gpu);
    let sampler = harness
        .gpu
        .device
        .create_sampler(&wgpu::SamplerDescriptor::default());
    bindings.set_texture("albedo", &view).expect("declared");
    bindings.set_sampler("clamped", &sampler).expect("declared");
    bindings
        .upload(&harness.gpu.device, &harness.gpu.queue)
        .expect("both are bound now");

    let pixel = harness.shade(&material, Some(&bindings), None);
    // The plane's centre is the meeting point of all four texels, and
    // nearest filtering picks one of them. Whichever it is, it is one of
    // the four written above and not the material default.
    let corners = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
    assert!(
        corners.iter().any(|corner| {
            corner
                .iter()
                .enumerate()
                .all(|(index, channel)| close(pixel[index], srgb(f32::from(*channel) / 255.0)))
        }),
        "the sampled colour is none of the texture's texels: {pixel:?}"
    );
}

#[test]
fn binding_a_texture_under_a_name_the_graph_does_not_declare_is_an_error() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&sampled_graph(&harness.registry));
    let mut bindings = harness.bindings(&material);
    let view = checker(&harness.gpu);
    let error = bindings
        .set_texture("albedoo", &view)
        .expect_err("a typo is not silently ignored");
    assert!(error.to_string().contains("albedoo"), "{error}");
    // And a sampler bound where a texture is declared.
    let sampler = harness
        .gpu
        .device
        .create_sampler(&wgpu::SamplerDescriptor::default());
    let error = bindings
        .set_sampler("albedo", &sampler)
        .expect_err("a texture is not a sampler");
    assert!(error.to_string().contains("texture_2d"), "{error}");
}

// ---------------------------------------------------------------------
// The application's own block
// ---------------------------------------------------------------------

#[test]
fn an_application_uniform_reaches_a_node_without_the_renderer_knowing_what_is_in_it() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);

    // The graph declares the *shape* of a block it does not own. Note the
    // `f32` before the `vec3f`: the application's buffer is laid out by
    // the same computer as the material's own, so the same alignment trap
    // applies and the same code closes it.
    let mut graph = Graph::new("user block");
    graph.set_user_block(UserBlockDecl {
        name: "app".to_string(),
        fields: vec![
            UserField::new("intensity", ValueType::F32),
            UserField::new("tint", ValueType::Vec3),
        ],
    });
    let read = graph.add(Node::new("input.user").with_setting("field", "tint"));
    graph
        .set_generic(&harness.registry, read, "T", ValueType::Vec3)
        .expect("the declared type");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&harness.registry, (read, "out"), (output, "emissive"))
        .expect("vec3f into emissive");

    let material = harness.material(&graph);
    let block = material
        .interface()
        .user
        .as_ref()
        .expect("the graph declares one");
    // The renderer knows the shape and nothing else — which is the point:
    // it can hand out a layout without ever holding the data.
    let tint = block.layout.field("tint").expect("declared");
    assert_eq!(tint.offset, 0, "the vec3f goes first, and the f32 after it");
    assert_eq!(
        block.layout.field("intensity").expect("declared").offset,
        12
    );
    assert_eq!(block.layout.size(), 16);

    // The application fills its own buffer, through the layout it was
    // handed, and builds its own bind group.
    let mut bytes = block.layout.zeroed();
    block
        .layout
        .write(&mut bytes, "tint", Value::Vec3([0.9, 0.2, 0.4]))
        .expect("declared");
    block
        .layout
        .write(&mut bytes, "intensity", Value::F32(1.0))
        .expect("declared");
    let buffer = harness.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("application block"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    harness.gpu.queue.write_buffer(&buffer, 0, &bytes);
    let layout = harness
        .renderer
        .user_layout(&harness.gpu.device, &material)
        .expect("the material declares a block")
        .clone();
    let group = harness
        .gpu
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("application block"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: abi::BINDING_USER_BLOCK,
                resource: buffer.as_entire_binding(),
            }],
        });

    let bindings = harness.bindings(&material);
    let pixel = harness.shade(&material, Some(&bindings), Some(&group));
    assert!(
        close(pixel[0], srgb(0.9)) && close(pixel[1], srgb(0.2)) && close(pixel[2], srgb(0.4)),
        "the application's own value did not reach the node: {pixel:?}"
    );
}

#[test]
fn a_draw_that_forgets_the_groups_its_material_declares_is_told_so() {
    // Rather than drawing something wrong, or a `wgpu` validation error
    // about a bind group index.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&tint_graph());
    let Harness {
        gpu,
        target,
        renderer,
        mesh,
        ..
    } = &mut harness;
    let draws = wxsl::render::single_draw(DrawItem::new(mesh, &material));
    let error = renderer
        .render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: target.view(),
                environment: &unlit(),
                draws: &draws,
            },
        )
        .expect_err("the material declares a parameter buffer");
    assert!(error.to_string().contains("material"), "{error}");
}
