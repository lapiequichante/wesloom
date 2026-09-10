//! What a material requires of its *geometry*, checked against a real GPU:
//! declared per-vertex streams and declared per-instance fields (ADR 0024).
//!
//! The companion to `material_resources.rs`, and it shares that file's
//! harness — see `tests/probe/mod.rs` for why. What these tests are for is
//! the class of bug only hardware finds: a `@location` the vertex buffer
//! and the shader number differently, an instance row read at the wrong
//! stride, a varying that arrives interpolated when it had to be flat.

use std::f32::consts::FRAC_PI_2;

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::{AttributeDecl, AttributeFrequency, Graph, Node, NodeId};
use wxsl::core::node::{NodeRegistry, Value, ValueType};
use wxsl::render::material::Material;
use wxsl::render::mesh::plane_geometry;
use wxsl::render::{
    AttributeValues, DrawItem, DrawList, InstanceAttributes, Mesh, MeshData, RenderError,
};

mod probe;
use probe::{close, gpu, pixel, probe_graph, probe_values, render_list, srgb, Harness, SIZE};

/// The probe declared as a per-instance attribute (ADR 0024).
///
/// The value is not stored in the graph at all — a per-instance attribute
/// has no value until there is an instance — so the caller supplies it on
/// the draw. That asymmetry with a uniform parameter is the whole
/// difference between owning a resource and requiring one.
fn declare_as_instance(
    graph: &mut Graph,
    registry: &NodeRegistry,
    name: &str,
    ty: ValueType,
    _value: Value,
) -> NodeId {
    graph.declare_attribute(AttributeDecl::instance(name, ty));
    let node = graph.add(Node::new("input.attribute").with_setting("name", name));
    graph
        .set_generic(registry, node, "T", ty)
        .expect("every value type is allowed");
    node
}

/// A graph whose emissive is one declared `vec3f` attribute.
fn attribute_graph(registry: &NodeRegistry, name: &str, frequency: AttributeFrequency) -> Graph {
    let mut graph = Graph::new(format!("{name} {frequency}"));
    graph.declare_attribute(AttributeDecl {
        name: name.to_string(),
        ty: ValueType::Vec3,
        frequency,
    });
    let read = graph.add(Node::new("input.attribute").with_setting("name", name));
    graph
        .set_generic(registry, read, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(registry, (read, "out"), (output, "emissive"))
        .expect("vec3f into emissive");
    graph
}

/// A plane facing the camera, carrying a `color` stream built from each
/// vertex's own position.
///
/// Red rises left to right across the surface, so a test can tell an
/// interpolated varying from a constant one without knowing the camera's
/// field of view: it only has to know that red goes up.
fn colored_plane(device: &wgpu::Device, size: f32) -> Mesh {
    let (vertices, indices) = plane_geometry(size, 8);
    let colors: Vec<[f32; 3]> = vertices
        .iter()
        .map(|vertex| {
            let across = (vertex.position[0] / size + 0.5).clamp(0.0, 1.0);
            [across, 0.25, 0.75]
        })
        .collect();
    let data =
        MeshData::new(vertices, indices).with_attribute("color", AttributeValues::Vec3(colors));
    Mesh::upload(device, "colored plane", &data)
}

/// Stand the plane up to face the camera.
fn facing() -> Mat4 {
    Mat4::from_rotation_x(FRAC_PI_2)
}

use wxsl::render::wgpu;

// ---------------------------------------------------------------------
// Per-vertex
// ---------------------------------------------------------------------

#[test]
fn a_declared_vertex_stream_reaches_the_fragment_stage_interpolated() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&attribute_graph(
        &harness.registry,
        "color",
        AttributeFrequency::Vertex,
    ));

    // The stream is in the generated source, so it is in the variant key
    // and in the pipeline's vertex layout.
    let source = material.wxsl(abi::MaterialStage::FORWARD_LIT);
    assert!(
        source.contains("@location(4) color: vec3f"),
        "the declared stream is not at the first free location: {source}"
    );

    let mesh = colored_plane(&harness.gpu.device, 2.0);
    let draws: DrawList =
        wxsl::render::single_draw(DrawItem::new(&mesh, &material).with_transform(facing()));
    let image = render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect("the frame renders");

    let left = pixel(&image, SIZE / 4, SIZE / 2);
    let middle = pixel(&image, SIZE / 2, SIZE / 2);
    let right = pixel(&image, SIZE * 3 / 4, SIZE / 2);
    // Green and blue are constant across the surface, so they say the
    // stream arrived at all; red rises, so it says it was interpolated
    // rather than flattened to one vertex's value.
    for sample in [left, middle, right] {
        assert!(
            close(sample[1], srgb(0.25)) && close(sample[2], srgb(0.75)),
            "the stream did not reach the shader: {sample:?}"
        );
    }
    assert!(
        close(middle[0], srgb(0.5)),
        "the middle of the plane is not the middle of the gradient: {middle:?}"
    );
    assert!(
        left[0] < middle[0] && middle[0] < right[0],
        "the stream is not interpolated across the surface: {left:?} {middle:?} {right:?}"
    );
}

#[test]
fn a_mesh_that_cannot_supply_a_declared_stream_says_which() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&attribute_graph(
        &harness.registry,
        "color",
        AttributeFrequency::Vertex,
    ));
    // `harness.mesh` is the plain plane, with no streams at all.
    let draws: DrawList =
        wxsl::render::single_draw(DrawItem::new(&harness.mesh, &material).with_transform(facing()));
    let error = render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect_err("the mesh cannot supply `color`");
    let RenderError::MissingVertexAttribute {
        material: named,
        attribute,
        mesh,
        ..
    } = &error
    else {
        panic!("wrong error: {error}");
    };
    // All three, because any one of them alone leaves the reader
    // guessing which draw in the frame it was.
    assert_eq!(attribute, "color");
    assert_eq!(named, &material.name);
    assert!(!mesh.is_empty(), "the mesh is named: {error}");
}

#[test]
fn a_stream_of_the_wrong_type_is_an_error_rather_than_a_reinterpretation() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&attribute_graph(
        &harness.registry,
        "color",
        AttributeFrequency::Vertex,
    ));
    let (vertices, indices) = plane_geometry(2.0, 2);
    let data = MeshData::new(vertices.clone(), indices).with_attribute(
        "color",
        // Two floats where the graph declared three: without this check
        // the vertex stage would read the next vertex's first component
        // as its own third.
        AttributeValues::Vec2(vec![[0.0, 0.0]; vertices.len()]),
    );
    let mesh = Mesh::upload(&harness.gpu.device, "mistyped", &data);
    let draws: DrawList =
        wxsl::render::single_draw(DrawItem::new(&mesh, &material).with_transform(facing()));
    let error = render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect_err("vec2f is not vec3f");
    assert!(
        matches!(error, RenderError::VertexAttributeType { .. }),
        "wrong error: {error}"
    );
}

// ---------------------------------------------------------------------
// Per-instance
// ---------------------------------------------------------------------

/// The tint instance `index` of the thousand carries.
fn tint_of(index: usize) -> Vec3 {
    let step = index as f32 / 1024.0;
    Vec3::new(step, 1.0 - step, 0.5)
}

#[test]
fn a_thousand_instances_read_their_own_tint_from_one_storage_buffer() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&attribute_graph(
        &harness.registry,
        "tint",
        AttributeFrequency::Instance,
    ));
    // Read in the *fragment* stage, which is the half WGSL makes awkward:
    // `@builtin(instance_index)` is a vertex-stage builtin, so the index
    // has to travel down as a flat varying and the fragment re-indexes
    // the row itself.
    let source = material.wxsl(abi::MaterialStage::FORWARD_LIT);
    assert!(
        source.contains("@interpolate(flat)"),
        "the instance index is not flat: {source}"
    );
    assert!(
        source.contains("tint: vec3f"),
        "the row was not widened: {source}"
    );

    const COUNT: usize = 1000;
    let attributes: Vec<InstanceAttributes> = (0..COUNT)
        .map(|index| {
            let tint = tint_of(index);
            InstanceAttributes::new().with("tint", Value::Vec3(tint.to_array()))
        })
        .collect();

    // Every quad covers the whole view; the depth test keeps the nearest.
    // Which index that is comes from where the quad was put, so the same
    // thousand rows answer twice with two different rows on top — which
    // is the assertion that the index in the varying is really the one
    // being used to index the buffer.
    let list = |front_is_last: bool| -> DrawList {
        (0..COUNT)
            .map(|index| {
                // Nearer is a *smaller* distance from the camera, and the
                // depth test keeps the nearest, so the index that ends up
                // visible is chosen by where its quad was put.
                let depth = if front_is_last {
                    COUNT - 1 - index
                } else {
                    index
                };
                DrawItem::new(&harness.mesh, &material)
                    .with_transform(
                        Mat4::from_translation(Vec3::new(0.0, 0.0, -0.001 * depth as f32))
                            * facing(),
                    )
                    .with_attributes(&attributes[index])
            })
            .collect()
    };

    let image = render_list(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &list(true),
    )
    .expect("the frame renders");
    let front = pixel(&image, SIZE / 2, SIZE / 2);
    let expected = tint_of(COUNT - 1);
    assert!(
        close(front[0], srgb(expected.x)) && close(front[1], srgb(expected.y)),
        "instance {} did not read its own row: {front:?}",
        COUNT - 1
    );

    let pipelines = harness.renderer.pipeline_count();
    let image = render_list(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &list(false),
    )
    .expect("the frame renders");
    let front = pixel(&image, SIZE / 2, SIZE / 2);
    let expected = tint_of(0);
    assert!(
        close(front[0], srgb(expected.x)) && close(front[1], srgb(expected.y)),
        "instance 0 did not read its own row: {front:?}"
    );
    // One binding, one upload, one pipeline: a thousand instances differ
    // in a buffer row and in nothing a pass has to rebind.
    assert_eq!(
        harness.renderer.pipeline_count(),
        pipelines,
        "a thousand instances built more than one pipeline"
    );
    assert_eq!(
        harness.renderer.frame_bindings().instance_shapes(),
        1,
        "one material wanted more than one attribute row shape"
    );
}

#[test]
fn a_draw_that_forgets_a_declared_instance_attribute_is_told_so() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&attribute_graph(
        &harness.registry,
        "tint",
        AttributeFrequency::Instance,
    ));
    let draws: DrawList =
        wxsl::render::single_draw(DrawItem::new(&harness.mesh, &material).with_transform(facing()));
    let error = render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect_err("the draw supplies nothing");
    let RenderError::MissingInstanceAttribute {
        attribute, draw, ..
    } = &error
    else {
        panic!("wrong error: {error}");
    };
    assert_eq!(attribute, "tint");
    assert_eq!(*draw, 0);
}

#[test]
fn two_materials_wanting_two_row_shapes_draw_in_one_frame() {
    // The frame group is bound once per frame only while every material
    // agrees about the row. When they do not, each shape gets a buffer
    // and a bind group of its own, and the pass rebinds group 0 as it
    // crosses from one to the other.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let wide = harness.material(&attribute_graph(
        &harness.registry,
        "tint",
        AttributeFrequency::Instance,
    ));
    // A different row, not merely a narrower one: two shapes, two
    // buffers, two frame bind groups.
    let mut narrow = Graph::new("narrow");
    narrow.declare_attribute(AttributeDecl::instance("shade", ValueType::F32));
    let read = narrow.add(Node::new("input.attribute").with_setting("name", "shade"));
    narrow
        .set_generic(&harness.registry, read, "T", ValueType::F32)
        .expect("f32 is allowed");
    let combine = narrow.add_node("convert.combine.vec3f");
    for socket in ["x", "y", "z"] {
        narrow
            .wire(&harness.registry, (read, "out"), (combine, socket))
            .expect("f32 into a component");
    }
    let output = narrow.add_node(abi::SURFACE_OUTPUT_ID);
    narrow
        .wire(&harness.registry, (combine, "out"), (output, "emissive"))
        .expect("vec3f into emissive");
    narrow.validate(&harness.registry).expect("valid");
    let plain = harness.material(&narrow);
    assert_ne!(wide.instance_signature(), plain.instance_signature());

    let shade = InstanceAttributes::new().with("shade", Value::F32(0.5));
    let tint = InstanceAttributes::new().with("tint", Value::Vec3([1.0, 0.5, 0.25]));
    let Harness {
        gpu,
        target,
        renderer,
        mesh,
        ..
    } = &mut harness;
    // The plain one behind, the wide one in front.
    let draws: DrawList = [
        DrawItem::new(mesh, &plain)
            .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, -0.5)) * facing())
            .with_attributes(&shade),
        DrawItem::new(mesh, &wide)
            .with_transform(facing())
            .with_attributes(&tint),
    ]
    .into_iter()
    .collect();
    let image = render_list(gpu, renderer, target, &draws).expect("the frame renders");
    let front = pixel(&image, SIZE / 2, SIZE / 2);
    assert!(
        close(front[0], srgb(1.0)) && close(front[1], srgb(0.5)),
        "the wide material did not read its own row: {front:?}"
    );
    assert_eq!(
        renderer.frame_bindings().instance_shapes(),
        2,
        "two attribute row shapes did not get two buffers"
    );
}

// ---------------------------------------------------------------------
// The computed layout, at every type
// ---------------------------------------------------------------------

#[test]
fn every_instance_attribute_type_arrives_where_the_computed_layout_says() {
    // The other half of `material_resources.rs`'s parameter test, over
    // the same probe graph and the same layout computer, in the storage
    // address space instead of the uniform one. Neither layout has a
    // `#[repr(C)]` mirror to be checked against, so both are checked
    // against hardware.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    for (ty, value) in probe_values() {
        let graph = probe_graph(&harness.registry, ty, value, &declare_as_instance);
        let material = harness.material(&graph);
        let attributes = InstanceAttributes::new()
            .with("probe", value)
            .with("pad", Value::F32(0.75));

        let Harness {
            gpu,
            target,
            renderer,
            mesh,
            ..
        } = &mut harness;
        let draws: DrawList = wxsl::render::single_draw(
            DrawItem::new(mesh, &material)
                .with_transform(facing())
                .with_attributes(&attributes),
        );
        let image = render_list(gpu, renderer, target, &draws).expect("the frame renders");
        let sample = pixel(&image, SIZE / 2, SIZE / 2);
        assert!(
            close(sample[0], 255),
            "a {ty} instance attribute did not arrive intact: {sample:?}"
        );
    }
}

#[test]
fn an_instance_attribute_supplied_at_the_wrong_type_is_refused() {
    // The counterpart that keeps the test above from passing vacuously:
    // the same graph, a value of another type, and an error rather than
    // four bytes reinterpreted.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let material = harness.material(&attribute_graph(
        &harness.registry,
        "tint",
        AttributeFrequency::Instance,
    ));
    let wrong = InstanceAttributes::new().with("tint", Value::F32(1.0));
    let Harness {
        gpu,
        target,
        renderer,
        mesh,
        ..
    } = &mut harness;
    let draws: DrawList = wxsl::render::single_draw(
        DrawItem::new(mesh, &material)
            .with_transform(facing())
            .with_attributes(&wrong),
    );
    let error = render_list(gpu, renderer, target, &draws).expect_err("an f32 is not a vec3f");
    assert!(
        matches!(error, RenderError::InstanceAttributeType { .. }),
        "wrong error: {error}"
    );
}

// ---------------------------------------------------------------------
// What a declaration costs, and what it does not
// ---------------------------------------------------------------------

#[test]
fn moving_an_attribute_between_frequencies_rewires_nothing() {
    // The reading node names the attribute and not its frequency, so the
    // *same* graph with one word of the declaration changed is a valid
    // graph reading the other backing.
    let registry = wxsl::stdlib::registry();
    let mut graph = attribute_graph(&registry, "shade", AttributeFrequency::Vertex);
    let nodes: Vec<_> = graph.nodes().map(|(id, node)| (id, node.clone())).collect();
    graph.validate(&registry).expect("valid per-vertex");

    graph.declare_attribute(AttributeDecl::instance("shade", ValueType::Vec3));
    graph
        .validate(&registry)
        .expect("and still valid per-instance");
    let after: Vec<_> = graph.nodes().map(|(id, node)| (id, node.clone())).collect();
    assert_eq!(nodes, after, "changing the frequency touched a node");
}

#[test]
fn declaring_nothing_generates_what_it_always_did() {
    // A material that asks nothing of its geometry must emit the two
    // entry points it always emitted, and no wider IO struct beside
    // them: this is what makes the whole feature free for every graph
    // that does not use it.
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("plain");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "emissive", Value::Vec3([1.0, 0.0, 0.0]));
    let material = Material::from_graph(&graph, &registry).expect("compiles");

    for stage in abi::MaterialStage::ALL {
        let source = material.wxsl(*stage);
        assert!(
            source.contains(&format!(
                "fn {}(input: {}) -> {}",
                abi::VERTEX_ENTRY,
                abi::VERTEX_IN_STRUCT,
                abi::VERTEX_OUT_STRUCT
            )),
            "{stage} did not keep the plain vertex entry: {source}"
        );
        for invented in [
            abi::MATERIAL_VERTEX_IN_STRUCT,
            abi::MATERIAL_VERTEX_OUT_STRUCT,
            abi::MATERIAL_VARYINGS_STRUCT,
            abi::MATERIAL_ATTRIBUTES_STRUCT,
            abi::MATERIAL_INSTANCE_VAR,
        ] {
            assert!(
                !source.contains(invented),
                "{stage} declared `{invented}` for a graph that declares nothing"
            );
        }
    }
    assert!(material.vertex_attributes().is_empty());
    // No attribute row at all, so no second storage buffer and nothing
    // bound at `abi::BINDING_INSTANCE_ATTRIBUTES` but the placeholder.
    assert!(material.instance_attributes().is_empty());
    assert_eq!(material.instance_layout().size(), 0);
}

// ---------------------------------------------------------------------
// From a file
// ---------------------------------------------------------------------

#[cfg(feature = "gltf")]
#[test]
fn a_gltf_mesh_with_a_vertex_colour_stream_drives_a_material() {
    // The end of the chain the milestone is about: a file carries a
    // stream, the importer names it, a graph declares that name, and the
    // pixel is what the file said.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/vertex_colors.gltf");
    let data = wxsl::render::gltf::load_merged(path).expect("the file imports");
    assert!(
        data.attributes
            .contains_key(wxsl::render::gltf::COLOR_ATTRIBUTE),
        "COLOR_0 did not come across as a named stream"
    );
    let mesh = Mesh::upload(&harness.gpu.device, "vertex colours", &data);

    // The file stores RGBA; the graph reads three components of it.
    let mut graph = Graph::new("from a file");
    graph.declare_attribute(AttributeDecl::vertex(
        wxsl::render::gltf::COLOR_ATTRIBUTE,
        ValueType::Vec4,
    ));
    let read = graph.add(
        Node::new("input.attribute").with_setting("name", wxsl::render::gltf::COLOR_ATTRIBUTE),
    );
    graph
        .set_generic(&harness.registry, read, "T", ValueType::Vec4)
        .expect("vec4f is allowed");
    let split = graph.add_node("convert.split.vec4f");
    graph
        .wire(&harness.registry, (read, "out"), (split, "v"))
        .expect("a vec4f");
    let combine = graph.add_node("convert.combine.vec3f");
    for (from, to) in [("x", "x"), ("y", "y"), ("z", "z")] {
        graph
            .wire(&harness.registry, (split, from), (combine, to))
            .expect("a component");
    }
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&harness.registry, (combine, "out"), (output, "emissive"))
        .expect("vec3f into emissive");
    graph.validate(&harness.registry).expect("valid");

    let material = harness.material(&graph);
    let Harness {
        gpu,
        target,
        renderer,
        ..
    } = &mut harness;
    let draws: DrawList = wxsl::render::single_draw(
        DrawItem::new(&mesh, &material)
            .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0))),
    );
    let image = render_list(gpu, renderer, target, &draws).expect("the frame renders");
    // The asset is one triangle covering the middle of the view, green
    // at every vertex, so no interpolation weight can change the answer.
    let sample = pixel(&image, SIZE / 2, SIZE / 2);
    assert!(
        close(sample[0], srgb(0.0)) && close(sample[1], srgb(0.5)) && close(sample[2], srgb(0.0)),
        "the file's vertex colour did not reach the shader: {sample:?}"
    );
}
