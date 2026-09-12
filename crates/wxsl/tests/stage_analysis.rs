//! Stage analysis, on real hardware (plan2 P9).
//!
//! The strongest assertion available: a node the compiler places in the
//! vertex stage and interpolates down — a *stage cut* — renders exactly
//! what the same value hand-wired through a declared interpolant renders.
//! The author wrote one graph with no stage machinery at all; the analysis
//! chose the placement; the picture is the one the manual wiring would
//! have produced.
//!
//! Object space is deliberate, as in `interpolants.rs`: it is the one
//! thing the fragment stage *has not got*, so a value that arrives at all
//! can only have come down an interpolant from the vertex stage.

use wxsl::core::abi;
use wxsl::core::graph::{AttributeDecl, Graph, Node, NodeId};
use wxsl::core::node::{NodeRegistry, ValueType};

mod probe;
use probe::{gpu, Harness};

/// Declare `name` and write it from `source` in the vertex stage.
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

#[test]
fn the_stage_cut_matches_a_hand_wired_interpolant() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let registry = &harness.registry;

    // What the author writes today: declare the interpolant, wire the
    // vertex side, read it back on the fragment side.
    let mut manual = Graph::new("hand wired");
    let object = manual.add_node("input.object_position");
    write_varying(
        &mut manual,
        registry,
        "local",
        ValueType::Vec3,
        (object, "out"),
    );
    let read = manual.add(Node::new("input.attribute").with_setting("name", "local"));
    manual
        .set_generic(registry, read, "T", ValueType::Vec3)
        .expect("an interpolant type is allowed");
    let output = manual.add_node(abi::SURFACE_OUTPUT_ID);
    manual
        .wire(registry, (read, "out"), (output, "emissive"))
        .expect("vec3 into emissive");
    let vertex = manual.add_node(abi::VERTEX_OUTPUT_ID);
    manual
        .wire(
            registry,
            (object, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .expect("vec3 into the offset");

    // What the analysis makes possible: the same value, wired to the same
    // two consumers, with no stage machinery in the graph at all. Object
    // space cannot be computed in the fragment stage, so the only way
    // this graph can compile is by cutting it — the compiler's decision,
    // not the author's.
    let mut automatic = Graph::new("compiler placed");
    let object = automatic.add_node("input.object_position");
    let output = automatic.add_node(abi::SURFACE_OUTPUT_ID);
    automatic
        .wire(registry, (object, "out"), (output, "emissive"))
        .expect("vec3 into emissive");
    let vertex = automatic.add_node(abi::VERTEX_OUTPUT_ID);
    automatic
        .wire(
            registry,
            (object, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .expect("vec3 into the offset");

    let hand_wired = harness.material(&manual);
    let compiler_placed = harness.material(&automatic);

    // The compiler-placed material really does ride a synthesized
    // interpolant, and the hand-wired one rides the declared one.
    let placed = harness
        .renderer
        .material_wgsl(&harness.gpu.device, &compiler_placed)
        .expect("the material compiled");
    assert!(placed.contains("attrs.auto0"), "{placed}");
    let wired = harness
        .renderer
        .material_wgsl(&harness.gpu.device, &hand_wired)
        .expect("the material compiled");
    assert!(wired.contains("attrs.local"), "{wired}");

    let a = harness.shade(&hand_wired, None, None);
    let b = harness.shade(&compiler_placed, None, None);
    let worst = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .expect("non-empty");
    assert!(
        worst <= 1,
        "the compiler-placed graph shades differently from the hand-wired \
         one: {worst}"
    );
}
