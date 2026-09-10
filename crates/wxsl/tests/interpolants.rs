//! Interpolants the graph computes: a value the vertex partition works
//! out, handed to the fragment partition down an inter-stage location
//! (ADR 0027).
//!
//! The other half of ADR 0024. That milestone brought in the values the
//! *geometry* supplies; this is the one the graph supplies to itself, and
//! the reason it is the same `input.attribute` node on the reading side is
//! that from the fragment stage the two arrive identically.
//!
//! Why a GPU test and not only a codegen one: the whole point of an
//! interpolant is that it is *interpolated*, and a location that the
//! vertex entry and the fragment entry number differently produces a
//! shader that compiles and draws the wrong thing.

use std::f32::consts::FRAC_PI_2;

use glam::Mat4;
use wxsl::core::abi;
use wxsl::core::graph::{AttributeDecl, Graph, Node, NodeId};
use wxsl::core::node::{NodeRegistry, Value, ValueType};
use wxsl::render::material::Material;
use wxsl::render::DrawItem;

mod probe;
use probe::{close, gpu, no_tonemap, pixel, render_list, srgb, Harness, SIZE};

/// Declare `name` as a computed interpolant, and wire `source` into the
/// node that writes it.
fn write_varying(
    graph: &mut Graph,
    registry: &NodeRegistry,
    name: &str,
    ty: ValueType,
    source: (NodeId, &str),
) {
    graph.declare_attribute(AttributeDecl::computed(name, ty));
    let writer = graph.add(Node::new(abi::VARYING_OUTPUT_ID).with_setting("name", name));
    graph
        .set_generic(registry, writer, "T", ty)
        .expect("an interpolant type is allowed");
    graph
        .wire(registry, source, (writer, abi::SOCKET_VARYING))
        .expect("the source has the declared type");
}

/// Read `name` back on the fragment side.
fn read_attribute(graph: &mut Graph, registry: &NodeRegistry, name: &str, ty: ValueType) -> NodeId {
    let node = graph.add(Node::new("input.attribute").with_setting("name", name));
    graph
        .set_generic(registry, node, "T", ty)
        .expect("every value type is allowed");
    node
}

/// A graph that computes one interpolant from object space in the vertex
/// stage, and shows it as the surface's emissive.
///
/// Object space on purpose: it is the one thing the fragment stage *has
/// not got*, so a value that arrives at all can only have come down a
/// location from the vertex stage.
fn object_position_graph(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::new("interpolated object position");
    let object = graph.add_node("input.object_position");
    write_varying(
        &mut graph,
        registry,
        "local",
        ValueType::Vec3,
        (object, "out"),
    );
    let read = read_attribute(&mut graph, registry, "local", ValueType::Vec3);
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(registry, (read, "out"), (output, "emissive"))
        .expect("a vec3f is an emissive");
    graph
}

#[test]
fn an_interpolant_carries_object_space_into_the_fragment_stage() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let graph = object_position_graph(&harness.registry);
    let material = harness.material(&graph);

    // The plane is 2 units across in its own space and centred, so its
    // centre is the object-space origin and its corner is (1, 0, 1)
    // before the harness stands it up. An emissive of 0 is black; the
    // corner is where the interpolation shows.
    let item =
        DrawItem::new(&harness.mesh, &material).with_transform(Mat4::from_rotation_x(FRAC_PI_2));
    let draws = wxsl::render::single_draw(item);
    let image = render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect("the frame renders");

    // Object-space x runs -1 to +1 across the plane, and an emissive
    // clamps at zero, so the left half is black and the right half is a
    // ramp — a gradient the fragment stage has no other way to know.
    let centre = pixel(&image, SIZE / 2, SIZE / 2);
    let right = pixel(&image, SIZE * 5 / 6, SIZE / 2);
    let left = pixel(&image, SIZE / 6, SIZE / 2);
    assert!(
        close(left[0], srgb(0.0)),
        "negative x clamps to black: {left:?}"
    );
    assert!(
        centre[0] < 64,
        "the object-space origin is all but black: {centre:?}"
    );
    assert!(
        right[0] > centre[0] + 64,
        "and x ramps to the right: {right:?} against {centre:?}"
    );
    // Nothing leaks into the other channels: object y and z are zero
    // across a flat plane in its own space.
    assert!(close(right[1], srgb(0.0)), "y stays zero: {right:?}");
}

#[test]
fn a_computed_interpolant_and_a_supplied_one_are_read_the_same_way() {
    // The claim ADR 0024's reading node makes: which of the three
    // frequencies a name has is the *declaration's* business. Moving one
    // from `computed` to `instance` is a one-line edit to the
    // declaration, and the reading node does not change at all.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let registry = wxsl::stdlib::registry();

    let mut computed = Graph::new("computed");
    let value = computed.add(Node::new("const.value"));
    computed
        .set_generic(&registry, value, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    computed.set_param(value, "value", Value::Vec3([0.25, 0.5, 0.75]));
    write_varying(
        &mut computed,
        &registry,
        "tint",
        ValueType::Vec3,
        (value, "out"),
    );
    let read = read_attribute(&mut computed, &registry, "tint", ValueType::Vec3);
    let output = computed.add_node(abi::SURFACE_OUTPUT_ID);
    computed
        .wire(&registry, (read, "out"), (output, "emissive"))
        .expect("a vec3f is an emissive");

    let material = harness.material(&computed);
    let shade = harness.shade(&material, None, None);
    for (channel, expected) in shade.iter().zip([0.25, 0.5, 0.75]) {
        assert!(
            close(*channel, srgb(expected)),
            "a constant down an interpolant arrives intact: {shade:?}"
        );
    }
}

#[test]
fn two_interpolants_take_two_locations_and_do_not_cross() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let registry = wxsl::stdlib::registry();

    // Declared in the order that is *not* their name order, so a
    // numbering that followed declaration order would swap them.
    let mut graph = Graph::new("two");
    let red = graph.add(Node::new("const.value"));
    graph
        .set_generic(&registry, red, "T", ValueType::F32)
        .expect("f32 is allowed");
    graph.set_param(red, "value", Value::F32(0.8));
    let blue = graph.add(Node::new("const.value"));
    graph
        .set_generic(&registry, blue, "T", ValueType::F32)
        .expect("f32 is allowed");
    graph.set_param(blue, "value", Value::F32(0.2));
    write_varying(&mut graph, &registry, "warm", ValueType::F32, (red, "out"));
    write_varying(&mut graph, &registry, "cool", ValueType::F32, (blue, "out"));

    let warm = read_attribute(&mut graph, &registry, "warm", ValueType::F32);
    let cool = read_attribute(&mut graph, &registry, "cool", ValueType::F32);
    let combine = graph.add_node("convert.combine.vec3f");
    graph
        .wire(&registry, (warm, "out"), (combine, "x"))
        .expect("an f32 is a component");
    graph
        .wire(&registry, (cool, "out"), (combine, "z"))
        .expect("an f32 is a component");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (combine, "out"), (output, "emissive"))
        .expect("a vec3f is an emissive");

    let material = harness.material(&graph);
    // `cool` sorts before `warm`, so it takes the first location. If the
    // two crossed, this reads 0.2 in red and 0.8 in blue.
    let shade = harness.shade(&material, None, None);
    assert!(close(shade[0], srgb(0.8)), "warm in red: {shade:?}");
    assert!(close(shade[2], srgb(0.2)), "cool in blue: {shade:?}");
}

#[test]
fn a_material_with_no_interpolants_generates_what_it_always_did() {
    // The byte-identity claim, which is what makes an addition like this
    // safe to land: a graph that declares none must be unchanged.
    let registry = wxsl::stdlib::registry();
    let mut plain = Graph::new("plain");
    plain.add_node(abi::SURFACE_OUTPUT_ID);
    let material = Material::from_graph_with_macros(&plain, &registry, &no_tonemap())
        .expect("the graph compiles");
    for stage in abi::MaterialStage::ALL {
        let source = material.wxsl(*stage);
        assert!(
            !source.contains(abi::VARYING_FN_PREFIX),
            "{stage}: {source}"
        );
        assert!(!source.contains(abi::MATERIAL_VARYINGS_STRUCT), "{stage}");
    }
}

#[test]
fn a_stage_with_no_fragment_program_computes_no_interpolant() {
    // A depth prepass has nothing to hand an interpolant *to*, so it does
    // not run the subgraph that computes one — the same economy as
    // leaving out the material function (ADR 0025). The location stays in
    // the struct, because the interface is per material.
    let registry = wxsl::stdlib::registry();
    let graph = object_position_graph(&registry);
    let material = Material::from_graph_with_macros(&graph, &registry, &no_tonemap())
        .expect("the graph compiles");

    let shading = material.wxsl(abi::MaterialStage::FORWARD_LIT);
    assert!(shading.contains(abi::VARYING_FN_PREFIX));
    for stage in [abi::MaterialStage::DEPTH_ONLY, abi::MaterialStage::SHADOW] {
        let source = material.wxsl(stage);
        assert!(
            !source.contains(abi::VARYING_FN_PREFIX),
            "{stage} should compute no interpolant:\n{source}"
        );
        // But the location is still declared, so the two stages agree
        // about the shape of the vertex output.
        assert!(source.contains("local"), "{stage}:\n{source}");
    }
}

#[test]
fn an_interpolant_read_from_the_vertex_stage_is_refused_by_name() {
    // The one typing rule computed interpolants add: the vertex stage is
    // what computes them, so it cannot also read one. Reported rather
    // than compiled into a shader that reads an undefined field.
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("circular");
    let object = graph.add_node("input.object_position");
    write_varying(
        &mut graph,
        &registry,
        "local",
        ValueType::Vec3,
        (object, "out"),
    );
    graph.add_node(abi::SURFACE_OUTPUT_ID);

    // Read it back into the *vertex* output, which is the vertex stage.
    let read = read_attribute(&mut graph, &registry, "local", ValueType::Vec3);
    let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
    graph
        .wire(
            &registry,
            (read, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .expect("a vec3f is an offset");

    let errors = graph.validate(&registry).expect_err("this cannot compile");
    let message = errors.to_string();
    assert!(message.contains("local"), "{message}");
    assert!(
        message.contains("fragment stage"),
        "the error says which stage may read it: {message}"
    );
}

#[test]
fn a_declared_interpolant_nothing_writes_is_reported() {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("unwritten");
    graph.declare_attribute(AttributeDecl::computed("ghost", ValueType::Vec3));
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let errors = graph.validate(&registry).expect_err("this cannot compile");
    let message = errors.to_string();
    assert!(message.contains("ghost"), "{message}");
    assert!(message.contains(abi::VARYING_OUTPUT_ID), "{message}");
}

#[test]
fn writing_an_interpolant_the_geometry_supplies_is_reported() {
    // The declaration says where a value comes from. Writing one the mesh
    // carries is a contradiction, not an override.
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("contradiction");
    graph.declare_attribute(AttributeDecl::vertex("color", ValueType::Vec3));
    let object = graph.add_node("input.object_position");
    let writer = graph.add(Node::new(abi::VARYING_OUTPUT_ID).with_setting("name", "color"));
    graph
        .set_generic(&registry, writer, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    graph
        .wire(&registry, (object, "out"), (writer, abi::SOCKET_VARYING))
        .expect("a vec3f is a vec3f");
    graph.add_node(abi::SURFACE_OUTPUT_ID);

    let errors = graph.validate(&registry).expect_err("this cannot compile");
    let message = errors.to_string();
    assert!(message.contains("color"), "{message}");
    assert!(message.contains("vertex"), "{message}");
}

#[test]
fn the_location_budget_counts_interpolants_too() {
    // One accountant for all of it: the shading basis, the instance
    // index, the declared streams and now these.
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("greedy");
    let value = graph.add(Node::new("const.value"));
    graph
        .set_generic(&registry, value, "T", ValueType::F32)
        .expect("f32 is allowed");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let spare = abi::MAX_VARYING_LOCATIONS - abi::VERTEX_OUT_FIELDS.len();
    for index in 0..=spare {
        write_varying(
            &mut graph,
            &registry,
            &format!("v{index}"),
            ValueType::F32,
            (value, "out"),
        );
    }
    let errors = graph.validate(&registry).expect_err("over budget");
    let message = errors.to_string();
    assert!(
        message.contains("inter-stage locations"),
        "the error names the budget: {message}"
    );
    assert!(message.contains("interpolant"), "{message}");
}

#[test]
fn an_interpolant_may_not_be_a_matrix() {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("matrix");
    graph.declare_attribute(AttributeDecl::computed("basis", ValueType::Mat3));
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let errors = graph.validate(&registry).expect_err("this cannot compile");
    let message = errors.to_string();
    assert!(message.contains("basis"), "{message}");
    assert!(message.contains("float vector"), "{message}");
}
