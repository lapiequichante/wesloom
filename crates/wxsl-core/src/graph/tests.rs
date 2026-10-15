use super::*;

/// A registry with one node reading a declared attribute.
fn attribute_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(
        NodeDefinition::builder("input.attribute", "Attribute")
            .setting(crate::node::SettingDef::new(
                node::SETTING_NAME,
                "name",
                "Which attribute.",
            ))
            .generic_param(crate::node::GenericParam::new("T", ValueType::ALL.to_vec()))
            .output(Socket::new("out", ValueType::F32).generic("T"))
            .declaration(NodeBody::AttributeRead),
    );
    registry
}

#[test]
fn reading_an_attribute_the_graph_does_not_declare_is_reported() {
    let registry = attribute_registry();
    let mut graph = Graph::new("undeclared");
    let node = graph.add(Node::new("input.attribute").with_setting("name", "color"));
    graph
        .set_generic(&registry, node, "T", ValueType::Vec3)
        .expect("allowed");
    let errors = graph.validate(&registry).expect_err("undeclared");
    assert!(
        errors
            .0
            .iter()
            .any(|error| matches!(error, GraphError::UnknownAttribute { .. })),
        "{errors:?}"
    );

    // And declaring it — at the same type — is all it takes.
    graph.declare_attribute(AttributeDecl::vertex("color", ValueType::Vec3));
    graph.validate(&registry).expect("declared now");

    // At a different type it is a mismatch, not a silent
    // reinterpretation: the type comes from the document, so this is
    // one of the few places a resolved generic can be *wrong*.
    graph.declare_attribute(AttributeDecl::vertex("color", ValueType::Vec2));
    let errors = graph.validate(&registry).expect_err("mistyped");
    assert!(
        errors
            .0
            .iter()
            .any(|error| matches!(error, GraphError::AttributeTypeMismatch { .. })),
        "{errors:?}"
    );
}

#[test]
fn a_per_vertex_attribute_of_a_type_no_vertex_buffer_carries_is_reported() {
    let registry = attribute_registry();
    let mut graph = Graph::new("integers");
    // Fine per instance — a storage buffer holds anything — and not
    // fine per vertex, where `wgpu::VertexFormat` decides.
    graph.declare_attribute(AttributeDecl::instance("count", ValueType::U32));
    graph
        .validate(&registry)
        .expect("a storage row holds a u32");
    graph.declare_attribute(AttributeDecl::vertex("count", ValueType::U32));
    let errors = graph.validate(&registry).expect_err("not a vertex format");
    assert!(
        errors
            .0
            .iter()
            .any(|error| matches!(error, GraphError::InvalidAttribute { .. })),
        "{errors:?}"
    );
}

#[test]
fn overrunning_a_budget_says_which_budget_and_by_how_much() {
    // The accountant. Both limits are counted in one place, so a
    // graph that overruns either is told rather than discovering it
    // as a shader-compiler error about location 16.
    let registry = attribute_registry();
    let mut graph = Graph::new("greedy");
    for index in 0..abi::MAX_VERTEX_ATTRIBUTES + 1 {
        graph.declare_attribute(AttributeDecl::vertex(
            format!("stream_{index}"),
            ValueType::Vec2,
        ));
    }
    let errors = graph.validate(&registry).expect_err("over the slot budget");
    let reported: Vec<String> = errors.0.iter().map(|error| error.to_string()).collect();
    assert!(
        reported
            .iter()
            .any(|text| text.contains("per-vertex attributes declared")),
        "{reported:?}"
    );
}

#[test]
fn an_attribute_cannot_take_a_name_the_instance_row_already_uses() {
    let registry = attribute_registry();
    let mut graph = Graph::new("shadowing");
    graph.declare_attribute(AttributeDecl::instance(
        abi::INSTANCE_BASE_FIELDS[0].name,
        ValueType::Mat4,
    ));
    let errors = graph.validate(&registry).expect_err("that name is taken");
    assert!(
        errors
            .0
            .iter()
            .any(|error| matches!(error, GraphError::InvalidAttribute { .. })),
        "{errors:?}"
    );
}

use crate::node::TypeRule;
use crate::node::{Socket, ValueType};

fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register_all([
        NodeDefinition::builder("test.const", "Const")
            .output(Socket::new("out", ValueType::F32))
            .expr("1.0"),
        NodeDefinition::builder("test.add", "Add")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("b", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("{a} + {b}"),
        NodeDefinition::builder("test.vec", "Vec")
            .input(Socket::new("v", ValueType::Vec3).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::Vec3))
            .expr("{v}"),
        NodeDefinition::builder("test.required", "Required")
            .input(Socket::new("must", ValueType::F32))
            .output(Socket::new("out", ValueType::F32))
            .expr("{must}"),
        NodeDefinition::builder("test.generic_add", "Generic add")
            .generic_param(crate::node::GenericParam::new(
                "T",
                vec![ValueType::F32, ValueType::Vec3],
            ))
            .input(Socket::new("a", ValueType::F32).generic("T"))
            .input(Socket::new("b", ValueType::F32).generic("T"))
            .output(Socket::new("out", ValueType::F32).generic("T"))
            .expr("{a} + {b}"),
        NodeDefinition::builder("test.generic_negate", "Generic negate")
            .generic_param(crate::node::GenericParam::new(
                "T",
                vec![ValueType::F32, ValueType::Vec3],
            ))
            .input(Socket::new("a", ValueType::F32).generic("T"))
            .output(Socket::new("out", ValueType::F32).generic("T"))
            .expr("-{a}"),
        NodeDefinition::builder("test.product", "Product")
            .generic_param(crate::node::GenericParam::new(
                "A",
                vec![ValueType::F32, ValueType::Vec2, ValueType::Vec3],
            ))
            .generic_param(crate::node::GenericParam::new(
                "B",
                vec![ValueType::F32, ValueType::Vec2, ValueType::Vec3],
            ))
            .input(Socket::new("a", ValueType::F32).generic("A"))
            .input(Socket::new("b", ValueType::F32).generic("B"))
            .output(Socket::new("out", ValueType::F32).combine(TypeRule::Product, "A", "B"))
            .expr("{a} * {b}"),
    ]);
    registry
}

#[test]
fn connect_type_checks_both_ends() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let a = graph.add_node("test.const");
    let v = graph.add_node("test.vec");
    let error = graph
        .wire(&registry, (a, "out"), (v, "v"))
        .expect_err("f32 must not connect to vec3f");
    assert!(matches!(error, GraphError::TypeMismatch { .. }));
    assert!(graph.edges().is_empty(), "a rejected edge is not added");
}

// -- generic sockets ------------------------------------------------

#[test]
fn a_fresh_generic_node_is_invalid_until_something_resolves_it() {
    let registry = registry();
    let mut graph = Graph::new("g");
    graph.add_node("test.generic_add");
    let errors = graph.validate(&registry).expect_err("nothing resolved T");
    assert!(
        errors
            .0
            .iter()
            .any(|e| matches!(e, GraphError::UnresolvedGeneric { param, .. } if param == "T")),
        "{errors:?}"
    );
}

#[test]
fn connecting_a_concrete_source_resolves_the_generic_target() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.vec"); // vec3f out
    let add = graph.add_node("test.generic_add");
    graph
        .wire(&registry, (source, "out"), (add, "a"))
        .expect("vec3f is one of T's allowed types");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));

    // `b` is still unconnected, but resolving `T` gave it a value of
    // the type it turned out to be (`Graph::retype_params`), so there
    // is nothing left unfed — a socket with a known type always has
    // something to edit.
    assert_eq!(
        graph
            .node(add)
            .and_then(|node| node.params.get("b"))
            .copied(),
        Some(Value::Vec3([0.0; 3]))
    );
    graph.validate(&registry).expect("resolved and fed");
}

#[test]
fn connecting_a_concrete_target_resolves_the_generic_source() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_node("test.generic_add");
    let sink = graph.add_node("test.vec");
    graph
        .wire(&registry, (add, "out"), (sink, "v"))
        .expect("vec3f is one of T's allowed types");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));
}

#[test]
fn a_mismatched_connection_to_an_already_resolved_generic_is_rejected() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let vec_source = graph.add_node("test.vec");
    let f32_source = graph.add_node("test.const");
    let add = graph.add_node("test.generic_add");
    graph
        .wire(&registry, (vec_source, "out"), (add, "a"))
        .expect("resolves T to vec3f");

    let error = graph
        .wire(&registry, (f32_source, "out"), (add, "b"))
        .expect_err("f32 no longer matches T (vec3f)");
    assert!(matches!(error, GraphError::TypeMismatch { .. }));
    // The rejected edge must not have been added, and `a` must still be
    // the only connection.
    assert_eq!(graph.edges().len(), 1);
}

#[test]
fn two_generic_nodes_wired_together_stay_polymorphic_until_anchored() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let first = graph.add_node("test.generic_negate");
    let second = graph.add_node("test.generic_negate");
    graph
        .wire(&registry, (first, "out"), (second, "a"))
        .expect("neither side is resolved yet, so nothing to check");
    assert_eq!(graph.generic_type(first, "T"), None);
    assert_eq!(graph.generic_type(second, "T"), None);

    // Anchoring the far end of the chain resolves both nodes, not just
    // the one a concrete edge touches directly.
    let source = graph.add_node("test.vec");
    graph
        .wire(&registry, (source, "out"), (first, "a"))
        .expect("vec3f is allowed");
    assert_eq!(graph.generic_type(first, "T"), Some(ValueType::Vec3));
    assert_eq!(
        graph.generic_type(second, "T"),
        Some(ValueType::Vec3),
        "resolution should have propagated across the existing edge"
    );
}

#[test]
fn set_generic_rejects_a_type_the_parameter_does_not_allow() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_node("test.generic_add");
    let error = graph
        .set_generic(&registry, add, "T", ValueType::Vec4)
        .expect_err("vec4f is not in T's allowed set");
    assert!(matches!(
        error,
        GraphError::InvalidGenericType {
            ty: ValueType::Vec4,
            ..
        }
    ));
}

#[test]
fn set_generic_resolves_a_node_nothing_has_connected_yet() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_node("test.generic_add");
    let disconnected = graph
        .set_generic(&registry, add, "T", ValueType::F32)
        .expect("f32 is allowed");
    assert!(disconnected.is_empty(), "nothing was connected to disturb");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
}

#[test]
fn repinning_disconnects_edges_that_no_longer_fit() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let vec_source = graph.add_node("test.vec");
    let add = graph.add_node("test.generic_add");
    graph
        .wire(&registry, (vec_source, "out"), (add, "a"))
        .expect("resolves T to vec3f");
    assert_eq!(graph.edges().len(), 1);

    let disconnected = graph
        .set_generic(&registry, add, "T", ValueType::F32)
        .expect("f32 is allowed");
    assert_eq!(disconnected.len(), 1, "the now-mismatched edge came back");
    assert_eq!(disconnected[0].from.node, vec_source);
    assert!(graph.edges().is_empty(), "and is gone from the graph");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
}

#[test]
fn a_resolved_generic_socket_checks_its_param_against_the_resolved_type() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_node("test.generic_add");
    graph
        .set_generic(&registry, add, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    // `a`'s param is an f32, but T resolved to vec3f.
    graph.set_param(add, "a", Value::F32(1.0));
    graph.set_param(add, "b", Value::Vec3([0.0; 3]));
    let errors = graph.validate(&registry).expect_err("a is the wrong type");
    assert!(matches!(
        errors.0.as_slice(),
        [GraphError::ParamTypeMismatch {
            supplied: ValueType::F32,
            expected: ValueType::Vec3,
            ..
        }]
    ));
}

#[test]
fn a_fully_resolved_and_fed_generic_node_validates_and_compiles() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_node("test.generic_add");
    graph.set_param(add, "a", Value::Vec3([1.0, 2.0, 3.0]));
    graph.set_param(add, "b", Value::Vec3([4.0, 5.0, 6.0]));
    graph
        .set_generic(&registry, add, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    graph.validate(&registry).expect("fully resolved and fed");
}

#[test]
fn a_placed_node_starts_at_its_default_type_and_validates() {
    // What an editor gets from dropping a node on the canvas: complete,
    // not half-typed. Nothing is connected, so nothing has said what the
    // type should be — but "no type at all" is an error, and a node
    // nobody has touched should not report one.
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_resolved(&registry, Node::new("test.generic_add"));
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
    assert_eq!(
        graph
            .node(add)
            .and_then(|node| node.params.get("a"))
            .copied(),
        Some(Value::F32(0.0)),
        "a generic input has no fixed default, so it needs a pinned value"
    );
    graph.validate(&registry).expect("placed and complete");
}

#[test]
fn picking_a_type_carries_the_pinned_values_across() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let add = graph.add_resolved(&registry, Node::new("test.generic_add"));
    graph.set_param(add, "a", Value::F32(0.5));
    graph
        .set_generic(&registry, add, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    assert_eq!(
        graph
            .node(add)
            .and_then(|node| node.params.get("a"))
            .copied(),
        Some(Value::Vec3([0.5; 3])),
        "the value the author typed should widen, not become a mismatch"
    );
    graph.validate(&registry).expect("retyped and still fed");
}

#[test]
fn plugging_a_vector_into_a_scalar_socket_retypes_the_node() {
    // The default `f32` is a default, not a refusal waiting to happen.
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.vec");
    let add = graph.add_resolved(&registry, Node::new("test.generic_add"));
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
    graph
        .wire(&registry, (source, "out"), (add, "a"))
        .expect("a wire retypes what only a default backs");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));
    graph.validate(&registry).expect("retyped and still fed");
}

#[test]
fn retyping_a_combined_output_solves_back_through_its_operands() {
    // `out`'s type is derived, so nothing can adopt into it directly —
    // but changing what it derives *from* does derive it, and that is a
    // plan too. The smallest one wins: `vec3f * f32` is already a
    // `vec3f`, so only `A` moves and the other operand stays a scalar
    // the author can still type a single number into.
    let registry = registry();
    let mut graph = Graph::new("g");
    let mul = graph.add_resolved(&registry, Node::new("test.product"));
    let sink = graph.add_node("test.vec"); // takes a vec3f
    assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::F32));
    graph
        .wire(&registry, (mul, "out"), (sink, "v"))
        .expect("A can become vec3f, which makes the product one");
    assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::Vec3));
    assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::F32));

    let def = registry.get("test.product").unwrap();
    let out = def.output("out").unwrap();
    assert_eq!(graph.effective_type(mul, out), Some(ValueType::Vec3));
    graph.validate(&registry).expect("solved and fed");
}

#[test]
fn retyping_stops_where_it_would_break_an_edge_that_already_exists() {
    // The line between adapting and vandalising: `out` is wired to
    // something that really is an `f32`, so `a` cannot quietly become a
    // `vec3f` and take that wire down with it.
    let registry = registry();
    let mut graph = Graph::new("g");
    let mul = graph.add_resolved(&registry, Node::new("test.product"));
    let sink = graph.add_node("test.add"); // concrete f32 inputs
    let vector = graph.add_node("test.vec");
    graph
        .wire(&registry, (mul, "out"), (sink, "a"))
        .expect("f32 * f32 is an f32");

    let error = graph
        .wire(&registry, (vector, "out"), (mul, "a"))
        .expect_err("retyping A would break the output's edge");
    assert!(
        matches!(error, GraphError::TypeMismatch { .. }),
        "{error:?}"
    );
    assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::F32));
    assert_eq!(graph.edges().len(), 1, "and nothing was disconnected");
}

#[test]
fn resolving_one_operand_seeds_the_other() {
    // Two independent parameters exist so `f32 * vec3f` is expressible,
    // not because two operands of different types are the common case —
    // so wiring one operand leaves the node complete, not half-typed.
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.vec");
    let mul = graph.add_node("test.product");
    graph
        .wire(&registry, (source, "out"), (mul, "a"))
        .expect("A resolves to vec3f");
    assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::Vec3));
    assert_eq!(
        graph.generic_type(mul, "B"),
        Some(ValueType::Vec3),
        "the other operand should follow, so the node is usable at once"
    );
    graph.set_param(mul, "b", Value::Vec3([2.0; 3]));
    graph.validate(&registry).expect("complete from one wire");
}

#[test]
fn a_wire_beats_a_resolution_no_other_wire_backs() {
    // The seeded `B` above is a default, so connecting a `f32` to `b`
    // must retype it rather than be refused — which is exactly the
    // reported bug: "connect a float and the other socket only accepts
    // floats".
    let registry = registry();
    let mut graph = Graph::new("g");
    let vector = graph.add_node("test.vec");
    let scalar = graph.add_node("test.const");
    let mul = graph.add_node("test.product");
    graph
        .wire(&registry, (vector, "out"), (mul, "a"))
        .expect("A resolves to vec3f, B is seeded to match");
    assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::Vec3));
    graph
        .wire(&registry, (scalar, "out"), (mul, "b"))
        .expect("a wire retypes a seeded parameter");
    assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::F32));

    let def = registry.get("test.product").unwrap();
    let out = def.output("out").unwrap();
    assert_eq!(graph.effective_type(mul, out), Some(ValueType::Vec3));
    graph.validate(&registry).expect("vec3f * f32 is valid");
}

#[test]
fn an_explicit_pick_is_also_only_a_default_until_a_wire_backs_it() {
    // Same rule, reached the other way: nothing is connected to the
    // node whose type was picked, so the pick loses to the wire. Once a
    // wire *does* back it, `a_mismatched_connection_to_an_already_
    // resolved_generic_is_rejected` is the case that applies instead.
    let registry = registry();
    let mut graph = Graph::new("g");
    let vector = graph.add_node("test.vec");
    let negate = graph.add_node("test.generic_negate");
    graph
        .set_generic(&registry, negate, "T", ValueType::F32)
        .expect("f32 is allowed");
    graph
        .wire(&registry, (vector, "out"), (negate, "a"))
        .expect("the wire retypes an unbacked pick");
    assert_eq!(graph.generic_type(negate, "T"), Some(ValueType::Vec3));
}

#[test]
fn disconnecting_everything_forgets_both_of_two_parameters() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.const"); // f32
    let mul = graph.add_node("test.product");
    graph
        .wire(&registry, (source, "out"), (mul, "a"))
        .expect("A resolves to f32, B is seeded to match");
    assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::F32));
    assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::F32));

    graph
        .disconnect(&registry, &SocketRef::new(mul, "a"))
        .expect("there was an edge");
    // Both are back to the default — the type the node would have been
    // placed with — rather than to nothing, since an unresolved
    // parameter is an error and nothing about the node is broken.
    let default = registry.get("test.product").unwrap().default_generics();
    assert_eq!(graph.generic_type(mul, "A"), Some(default["A"]));
    assert_eq!(
        graph.generic_type(mul, "B"),
        Some(default["B"]),
        "a seeded parameter is released with the one that seeded it"
    );
    graph.validate(&registry).expect("resolved and fed");
}

#[test]
fn unplugging_a_combined_output_keeps_what_the_operands_still_say() {
    // `out` is the only edge that goes, and it never resolved anything
    // by itself — but `a` is still wired, so `A` stays, and `B` (seeded
    // from `A`) has to be seeded again rather than left blank.
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.vec");
    let mul = graph.add_node("test.product");
    let sink = graph.add_node("test.vec");
    graph
        .wire(&registry, (source, "out"), (mul, "a"))
        .expect("A resolves to vec3f, B is seeded to match");
    graph
        .wire(&registry, (mul, "out"), (sink, "v"))
        .expect("vec3f * vec3f is a vec3f");

    graph
        .disconnect(&registry, &SocketRef::new(sink, "v"))
        .expect("there was an edge");
    assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::Vec3));
    assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::Vec3));
}

#[test]
fn disconnecting_every_edge_releases_a_generic_resolution() {
    // The bug this pins down: a node that once resolved `T` to `f32`
    // stayed stuck there forever, even fully disconnected — so it could
    // never again accept a `vec3f`.
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.vec"); // vec3f
    let add = graph.add_node("test.generic_add");
    graph
        .wire(&registry, (source, "out"), (add, "a"))
        .expect("resolves T to vec3f");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));

    graph
        .disconnect(&registry, &SocketRef::new(add, "a"))
        .expect("there was an edge");
    assert_eq!(
        graph.generic_type(add, "T"),
        Some(ValueType::F32),
        "nothing constrains T anymore, so it falls back to the default"
    );

    let vec_source = graph.add_node("test.vec");
    graph
        .wire(&registry, (vec_source, "out"), (add, "a"))
        .expect("T is free to become vec3f now");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));
}

#[test]
fn removing_a_node_releases_a_surviving_neighbors_generic_resolution() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let source = graph.add_node("test.vec"); // vec3f
    let add = graph.add_node("test.generic_add");
    graph
        .wire(&registry, (source, "out"), (add, "a"))
        .expect("resolves T to vec3f");
    assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));

    graph.remove_node(&registry, source).expect("it existed");
    assert_eq!(
        graph.generic_type(add, "T"),
        Some(ValueType::F32),
        "its only connection went with the node that made it, so `T` \
         is back to the default rather than stuck or unresolved"
    );
    graph.validate(&registry).expect("resolved and fed");
}

#[test]
fn a_node_still_wired_to_another_generic_node_keeps_its_resolution() {
    // The deliberately-scoped half of the same rule: removing the edge
    // that anchored a *chain* does not chase back through it — see
    // `Graph::refresh_generics`'s doc comment for why.
    let registry = registry();
    let mut graph = Graph::new("g");
    let first = graph.add_node("test.generic_negate");
    let second = graph.add_node("test.generic_negate");
    graph
        .wire(&registry, (first, "out"), (second, "a"))
        .expect("both stay polymorphic");
    let sink = graph.add_node("test.vec");
    graph
        .wire(&registry, (second, "out"), (sink, "v"))
        .expect("resolves the whole chain to vec3f");
    assert_eq!(graph.generic_type(first, "T"), Some(ValueType::Vec3));
    assert_eq!(graph.generic_type(second, "T"), Some(ValueType::Vec3));

    graph
        .disconnect(&registry, &SocketRef::new(sink, "v"))
        .unwrap();
    assert_eq!(
        graph.generic_type(second, "T"),
        Some(ValueType::Vec3),
        "second is still wired to first, so it keeps its resolution"
    );
}

#[test]
fn a_combined_output_adapts_to_a_scalar_and_a_vector_operand() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let scalar = graph.add_node("test.const"); // f32
    let vector = graph.add_node("test.vec"); // vec3f
    let mul = graph.add_node("test.product");
    graph
        .wire(&registry, (scalar, "out"), (mul, "a"))
        .expect("A resolves to f32");
    graph
        .wire(&registry, (vector, "out"), (mul, "b"))
        .expect("B resolves to vec3f");

    let def = registry.get("test.product").unwrap();
    let out = def.output("out").unwrap();
    assert_eq!(graph.effective_type(mul, out), Some(ValueType::Vec3));
    graph.validate(&registry).expect("f32 * vec3f is valid");
}

#[test]
fn mismatched_combined_operands_are_reported_but_do_not_block_connecting() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let two = graph.add_node("test.product");
    graph
        .set_generic(&registry, two, "A", ValueType::Vec2)
        .unwrap();
    graph
        .set_generic(&registry, two, "B", ValueType::Vec3)
        .unwrap();

    let def = registry.get("test.product").unwrap();
    let out = def.output("out").unwrap();
    assert_eq!(
        graph.effective_type(two, out),
        None,
        "vec2f and vec3f do not combine"
    );
    graph.set_param(two, "a", crate::node::Value::Vec2([0.0; 2]));
    graph.set_param(two, "b", crate::node::Value::Vec3([0.0; 3]));
    let errors = graph
        .validate(&registry)
        .expect_err("incompatible operands");
    assert!(
        errors
            .0
            .iter()
            .any(|e| matches!(e, GraphError::IncompatibleGenerics { .. })),
        "{errors:?}"
    );
}

#[test]
fn repinning_one_combined_operand_disconnects_a_now_mismatched_output() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let mul = graph.add_node("test.product");
    let sink = graph.add_node("test.vec"); // takes a vec3f
    graph
        .set_generic(&registry, mul, "A", ValueType::Vec3)
        .unwrap();
    graph
        .set_generic(&registry, mul, "B", ValueType::F32)
        .unwrap();
    graph.wire(&registry, (mul, "out"), (sink, "v")).unwrap();

    // Repinning B to vec2f makes the product of (vec3f, vec2f)
    // incompatible, so the output no longer agrees with `sink`.
    let disconnected = graph
        .set_generic(&registry, mul, "B", ValueType::Vec2)
        .expect("vec2f is allowed for B");
    assert_eq!(disconnected.len(), 1, "the output's stale edge came back");
    assert_eq!(disconnected[0].to.node, sink);
    assert!(graph.edges().is_empty());
}

#[test]
fn connect_rejects_unknown_sockets() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let a = graph.add_node("test.const");
    let b = graph.add_node("test.add");
    assert!(matches!(
        graph.wire(&registry, (a, "nope"), (b, "a")),
        Err(GraphError::UnknownSocket {
            direction: Direction::Output,
            ..
        })
    ));
    assert!(matches!(
        graph.wire(&registry, (a, "out"), (b, "nope")),
        Err(GraphError::UnknownSocket {
            direction: Direction::Input,
            ..
        })
    ));
}

#[test]
fn inputs_take_exactly_one_edge() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let a = graph.add_node("test.const");
    let b = graph.add_node("test.const");
    let add = graph.add_node("test.add");
    graph.wire(&registry, (a, "out"), (add, "a")).unwrap();
    assert!(matches!(
        graph.wire(&registry, (b, "out"), (add, "a")),
        Err(GraphError::InputAlreadyConnected { .. })
    ));
    // Unplugging frees the input again.
    graph
        .disconnect(&registry, &SocketRef::new(add, "a"))
        .unwrap();
    graph.wire(&registry, (b, "out"), (add, "a")).unwrap();
}

#[test]
fn cycles_are_rejected_at_connect_time() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let first = graph.add_node("test.add");
    let second = graph.add_node("test.add");
    graph
        .wire(&registry, (first, "out"), (second, "a"))
        .unwrap();
    assert!(matches!(
        graph.wire(&registry, (second, "out"), (first, "a")),
        Err(GraphError::WouldCycle { .. })
    ));
    assert!(matches!(
        graph.wire(&registry, (first, "out"), (first, "b")),
        Err(GraphError::WouldCycle { .. })
    ));
    assert_eq!(graph.edges().len(), 1);
    graph.validate(&registry).expect("still a valid DAG");
}

#[test]
fn topological_order_respects_dependencies() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let a = graph.add_node("test.const");
    let b = graph.add_node("test.add");
    let c = graph.add_node("test.add");
    graph.wire(&registry, (a, "out"), (b, "a")).unwrap();
    graph.wire(&registry, (b, "out"), (c, "a")).unwrap();
    let order = graph.topological_order(None).unwrap();
    let position = |id: NodeId| order.iter().position(|x| *x == id).unwrap();
    assert!(position(a) < position(b));
    assert!(position(b) < position(c));

    // Restricted to a subset, unrelated nodes are left out.
    let subset = graph.dependencies_of(b);
    let order = graph.topological_order(Some(&subset)).unwrap();
    assert_eq!(order, vec![a, b]);
}

#[test]
fn removing_a_node_removes_its_edges() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let a = graph.add_node("test.const");
    let b = graph.add_node("test.add");
    graph.wire(&registry, (a, "out"), (b, "a")).unwrap();
    graph.remove_node(&registry, a).unwrap();
    assert!(graph.edges().is_empty());
    assert!(graph.node(a).is_none());
    // Ids are never reused, so old edges cannot be resurrected.
    let c = graph.add_node("test.const");
    assert_ne!(c, a);
}

#[test]
fn validate_reports_every_problem_at_once() {
    let registry = registry();
    let mut graph = Graph::new("g");
    let missing = graph.add(Node::new("test.required"));
    let unknown_def = graph.add(Node::new("test.does_not_exist"));
    let bad_param = graph.add(Node::new("test.add").with_param("a", Value::Vec3([0.0; 3])));
    let unknown_param = graph.add(Node::new("test.add").with_param("nope", Value::F32(1.0)));

    let errors = graph.validate(&registry).expect_err("graph is invalid");
    assert_eq!(errors.len(), 4, "{errors}");
    assert!(errors.0.iter().any(|e| matches!(
        e,
        GraphError::MissingInput { socket } if socket.node == missing
    )));
    assert!(errors
        .0
        .iter()
        .any(|e| matches!(e, GraphError::UnknownDefinition { node, .. } if *node == unknown_def)));
    assert!(errors.0.iter().any(|e| matches!(
        e,
        GraphError::ParamTypeMismatch { socket, .. } if socket.node == bad_param
    )));
    assert!(errors
        .0
        .iter()
        .any(|e| matches!(e, GraphError::UnknownParam { node, .. } if *node == unknown_param)));
}

#[test]
fn a_pinned_parameter_satisfies_a_required_input() {
    let registry = registry();
    let mut graph = Graph::new("g");
    graph.add(Node::new("test.required").with_param("must", Value::F32(0.25)));
    graph.validate(&registry).unwrap();
}

// -- bake declarations (ADR 0045) ------------------------------------

fn bake_of(node: NodeId, texture: &str) -> BakeDecl {
    BakeDecl {
        node,
        socket: None,
        texture: texture.to_string(),
        size: [64, 64],
        precision: crate::abi::GBufferPrecision::HighDynamicRange,
    }
}

#[test]
fn a_bake_naming_a_socket_the_node_lacks_is_reported() {
    let registry = registry();
    let mut graph = Graph::new("baked");
    let term = graph.add_node("test.vec");
    // A declaration is validated against the *registry's* shape of the
    // node, not the wiring: an unknown socket is refused by name, and
    // so is a texture name nothing downstream could spell.
    let mut wrong = bake_of(term, "normal_bake");
    wrong.socket = Some("no_such_socket".to_string());
    graph.declare_bake(wrong);
    let mut unnamed = bake_of(term, "not an identifier");
    unnamed.node = graph.add_node("test.vec");
    graph.declare_bake(unnamed);
    let errors = graph
        .validate(&registry)
        .expect_err("the bake names a socket that is not there");
    let message = errors.to_string();
    assert!(
        message.contains("normal_bake") && message.contains("no output socket `no_such_socket`"),
        "{message}"
    );
    assert!(
        message.contains("not an identifier") && message.contains("WXSL identifier"),
        "{message}"
    );
}

#[test]
fn one_node_cannot_stand_in_for_two_bakes() {
    let registry = registry();
    let mut graph = Graph::new("shared");
    let term = graph.add_node("test.vec");
    graph.declare_bake(bake_of(term, "normal_bake"));
    graph.declare_bake(bake_of(term, "other_bake"));
    let errors = graph
        .validate(&registry)
        .expect_err("one node cannot stand in for two bakes");
    assert!(
        errors.to_string().contains("other_bake") && errors.to_string().contains("baked twice"),
        "{errors}"
    );
}

#[cfg(feature = "serde")]
mod document_versions {
    //! The two version fields every serialized document carries: absent
    //! means this build's, a newer schema is refused by name, and anything
    //! this crate writes stamps both.

    use super::*;

    #[test]
    fn a_document_without_version_fields_reads_as_this_build_wrote_it() {
        let graph: Graph = serde_json::from_str(r#"{"name": "old", "nodes": [], "edges": []}"#)
            .expect("a pre-versioning document parses");
        assert_eq!(graph.name(), "old");
    }

    #[test]
    fn a_newer_schema_version_is_refused_by_name() {
        let error = serde_json::from_str::<Graph>(
            r#"{"version": 99, "abi": 1, "name": "from the future"}"#,
        )
        .expect_err("refused");
        assert!(
            error.to_string().contains("schema version 99"),
            "the refusal names both versions: {error}"
        );
    }

    #[test]
    fn a_document_pinning_another_abi_is_refused_by_name() {
        let error =
            serde_json::from_str::<Graph>(r#"{"version": 1, "abi": 7, "name": "another ABI"}"#)
                .expect_err("refused");
        assert!(
            error.to_string().contains("ABI revision 7"),
            "the refusal names both revisions: {error}"
        );
    }

    #[test]
    fn a_saved_document_stamps_its_version_and_abi() {
        let text = serde_json::to_string(&Graph::new("stamped")).expect("serializes");
        assert!(
            text.contains(r#""version":1"#) && text.contains(r#""abi":1"#),
            "the stamp is on the wire: {text}"
        );
    }

    #[test]
    fn a_bake_declaration_round_trips_through_the_wire() {
        let mut graph = Graph::new("baked");
        let term = graph.add_node("math.multiply.f32");
        graph.declare_bake(BakeDecl {
            node: term,
            socket: None,
            texture: "roughness_bake".to_string(),
            size: [256, 128],
            precision: crate::abi::GBufferPrecision::Normalized,
        });
        let text = serde_json::to_string(&graph).expect("serializes");
        // Absent default fields stay off the wire, and the precision
        // travels as the name every document spells it with.
        assert!(
            text.contains(r#""bakes":[{"node":1,"texture":"roughness_bake""#)
                && text.contains(r#""size":[256,128]"#)
                && text.contains(r#""precision":"standard""#),
            "{text}"
        );
        let mut back: Graph = serde_json::from_str(&text).expect("parses back");
        let decl = back.bake("roughness_bake").expect("the bake survived");
        assert_eq!(decl.node, term);
        assert_eq!(decl.size, [256, 128]);
        assert_eq!(decl.precision, crate::abi::GBufferPrecision::Normalized);
        // And the declaration is addressable the way codegen and the
        // renderer address it: by the texture's name.
        assert!(back.remove_bake("roughness_bake").is_some());
        assert!(back.bake("roughness_bake").is_none());
    }
}
