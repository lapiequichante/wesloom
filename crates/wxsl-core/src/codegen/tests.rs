use super::*;
use crate::graph::Node;
use crate::macros::{MacroDef, MacroValue};
use crate::node::{GenericParam, Socket, ValueType, WxslFunction};

fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(abi::surface_output_def());
    registry.register_all(abi::context_node_defs());
    registry.register_all([
        NodeDefinition::builder("math.multiply.f32", "Multiply")
            .input(Socket::new("a", ValueType::F32).with_splat_default(1.0))
            .input(Socket::new("b", ValueType::F32).with_splat_default(1.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("{a} * {b}"),
        NodeDefinition::builder("math.negate.f32", "Negate")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("-{a}"),
        NodeDefinition::builder("test.macro_user", "Macro user")
            .macro_var(MacroDef::new(
                "WXSL_TEST_SCALE",
                MacroValue::Float(2.0),
                "test scale",
            ))
            .macro_var(MacroDef::new(
                "wxsl_test_flag",
                MacroValue::Flag(false),
                "test flag",
            ))
            .input(Socket::new("a", ValueType::F32).with_splat_default(1.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("{a} * WXSL_TEST_SCALE"),
        NodeDefinition::builder("test.generic_add", "Generic add")
            .generic_param(crate::node::GenericParam::new(
                "T",
                vec![ValueType::F32, ValueType::Vec3],
            ))
            .input(Socket::new("a", ValueType::F32).generic("T"))
            .input(Socket::new("b", ValueType::F32).generic("T"))
            .output(Socket::new("out", ValueType::F32).generic("T"))
            .expr("{a} + {b}"),
        NodeDefinition::builder("test.split", "Split").call(WxslFunction::new_struct(
            "package::test",
            "split_value",
            vec![Socket::new("value", ValueType::F32).with_splat_default(0.0)],
            "SplitResult",
            vec![
                Socket::new("low", ValueType::F32),
                Socket::new("high", ValueType::F32),
            ],
        )),
    ]);
    registry
}

fn generate_default(graph: &Graph, registry: &NodeRegistry) -> GeneratedShader {
    generate(graph, registry, &CodegenOptions::default()).expect("codegen succeeds")
}

/// A value both stages read: computed per vertex, handed to the
/// fragment stage as a synthesized interpolant — the whole of plan2
/// P9 in one graph.
#[test]
fn a_shared_node_is_computed_in_the_vertex_stage_and_interpolated_down() {
    let mut registry = NodeRegistry::new();
    registry.register(abi::surface_output_def());
    registry.register(abi::vertex_output_def());
    registry.register_all(abi::context_node_defs());
    registry.register_all([NodeDefinition::builder("test.vec3", "Vec3")
        .input(Socket::new("a", ValueType::F32).with_splat_default(0.5))
        .output(Socket::new("out", ValueType::Vec3))
        .expr("vec3f({a})")]);
    let mut graph = Graph::new("cut");
    let value = graph.add(Node::new("test.vec3"));
    let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
    graph
        .wire(&registry, (value, "out"), (surface, "base_color"))
        .expect("vec3 into base_color");
    graph
        .wire(
            &registry,
            (value, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .expect("vec3 into the offset");

    let shader = generate_default(&graph, &registry);
    // The cut is a real interpolant: declared on the interface,
    // computed by a vertex-stage function of its own, written by the
    // vertex entry, and read by the material function through the
    // attributes struct.
    assert!(
        shader
            .source
            .contains("fn wxsl_varying_auto0(vtx: VertexContext"),
        "{})",
        shader.source
    );
    assert!(
        shader
            .source
            .contains("out.auto0 = wxsl_varying_auto0(vtx, attrs);"),
        "{}",
        shader.source
    );
    assert!(shader.source.contains("attrs.auto0"), "{}", shader.source);
    // And the fragment partition stops at the cut: the shared node's
    // expression appears once, in the vertex function, not again in
    // the material function.
    // The material function assigns the surface field straight from
    // the interpolant read — it does not re-evaluate the node.
    assert!(
        shader.source.contains("surface.base_color = attrs.auto0;"),
        "{}",
        shader.source
    );

    // A stage with no fragment program computes no cut — there is
    // nothing to hand the value to.
    let options = CodegenOptions {
        stage: abi::MaterialStage::DEPTH_ONLY,
        ..CodegenOptions::default()
    };
    let depth = generate(&graph, &registry, &options).expect("depth compiles");
    assert!(
        !depth.source.contains("wxsl_varying_auto0"),
        "the depth module should not compute the cut:\n{}",
        depth.source
    );
}

#[test]
fn a_bare_output_node_still_compiles_to_a_material() {
    let registry = registry();
    let mut graph = Graph::new("bare");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let shader = generate_default(&graph, &registry);

    assert!(shader
        .source
        .contains("fn wxsl_material(ctx: SurfaceContext) -> Surface"));
    assert!(shader
        .source
        .contains("var surface: Surface = default_surface(ctx);"));
    // `normal` has no literal default, so it keeps what default_surface set.
    assert!(!shader.source.contains("surface.normal ="));
    assert!(shader
        .source
        .contains("surface.base_color = vec3f(0.8, 0.8, 0.8);"));
    // One stage, one fragment entry. The two `@if`s are the lighting
    // generator's debug-normals arm — the knobs the ABI honours are
    // conditional-translation features, and the pasted shading
    // function declares and switches them.
    assert!(shader
        .source
        .contains("fn fs_forward_lit(vertex: VertexOut)"));
    assert_eq!(shader.source.matches("@fragment").count(), 1);
    assert_eq!(shader.source.matches("@if(wxsl_debug_normals)").count(), 1);
    assert_eq!(shader.source.matches("@if(!wxsl_debug_normals)").count(), 1);
}

#[test]
fn the_velocity_stage_transforms_every_vertex_twice_and_shades_nothing() {
    // The stage's whole shape: a vertex entry whose IO carries both
    // frames' clip positions, a fragment that is their difference in
    // uv units, and no surface anywhere — the material function and
    // the shading function are not in this module at all, which is
    // the velocity stage's share of the partitioning saving
    // (ADR 0025).
    let registry = registry();
    let mut graph = Graph::new("plain");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let options = CodegenOptions {
        stage: abi::MaterialStage::VELOCITY,
        ..CodegenOptions::default()
    };
    let source = generate(&graph, &registry, &options)
        .expect("compiles")
        .source;
    assert!(
        source
            .contains("fn vs_main(input: VertexIn) -> VelocityOut {\n    return transform_vertex_velocity(input, vec3f(0.0), vec3f(0.0));"),
        "the undismayed vertex entry passes both zero offsets: {source}"
    );
    assert!(
        source.contains(
            "fn fs_velocity(vertex: VelocityOut) -> @location(0) vec2f {\n    \
             return screen_motion(vertex.current_clip, vertex.previous_clip);"
        ),
        "the fragment is the difference of the two interpolated clips: {source}"
    );
    assert!(!source.contains(abi::SHADE_SURFACE_FN), "{source}");
    assert!(!source.contains(abi::PACK_GBUFFER_FN), "{source}");
    assert!(
        source.contains("import package::wxsl::velocity::"),
        "the whole velocity ABI comes from its one module: {source}"
    );
}

#[test]
fn a_time_driven_displacement_answers_for_both_frames() {
    // The graph half M5 settled: the vertex partition is evaluated
    // twice — once against this frame's context, once against
    // `previous_vertex_context` — by the same emitted function, so a
    // displacement driven by `input.time` is where it *was* last
    // frame without the graph knowing there is a previous frame.
    let mut registry = registry();
    registry.register(abi::vertex_output_def());
    registry.register_all([NodeDefinition::builder("test.vec3", "To vec3")
        .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
        .output(Socket::new("out", ValueType::Vec3))
        .expr("vec3f({a})")]);
    let registry = registry;
    let mut graph = Graph::new("displaced");
    let term = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
    let widened = graph.add(Node::new("test.vec3"));
    let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
    graph
        .wire(&registry, (term, "out"), (out, "roughness"))
        .unwrap();
    graph
        .wire(&registry, (term, "out"), (widened, "a"))
        .unwrap();
    graph
        .wire(
            &registry,
            (widened, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .unwrap();
    let options = CodegenOptions {
        stage: abi::MaterialStage::VELOCITY,
        ..CodegenOptions::default()
    };
    let source = generate(&graph, &registry, &options)
        .expect("compiles")
        .source;
    assert!(
        source.contains(
            "transform_vertex_velocity(input, wxsl_vertex(vtx, attrs), \
             wxsl_vertex(previous_vertex_context(input), attrs))"
        ),
        "both offsets come from the one partition, twice-contexted: {source}"
    );
}

#[test]
fn the_previous_clip_rides_after_a_materials_own_extras() {
    // With declared attributes the velocity stage's extra varying
    // takes the first location the material's own did not, in both IO
    // structs — no interface location moves, and the fragment reads
    // it from `extra` like any other declared extra.
    let mut registry = registry();
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
    let registry = registry;
    let mut graph = Graph::new("with attributes");
    graph.declare_attribute(crate::graph::AttributeDecl::vertex(
        "weight",
        ValueType::F32,
    ));
    let weight = graph.add(Node::new("input.attribute").with_setting("name", "weight"));
    let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    graph
        .wire(&registry, (weight, "out"), (out, "roughness"))
        .unwrap();
    let options = CodegenOptions {
        stage: abi::MaterialStage::VELOCITY,
        ..CodegenOptions::default()
    };
    let source = generate(&graph, &registry, &options)
        .expect("compiles")
        .source;
    let first = abi::VERTEX_OUT_FIELDS.len() + 1;
    for (location, field) in [
        (first, abi::CURRENT_CLIP_FIELD),
        (first + 1, abi::PREVIOUS_CLIP_FIELD),
    ] {
        let line = format!("@location({location}) {field}: vec4f,");
        assert_eq!(
            source.matches(&line).count(),
            2,
            "`{field}` rides in the vertex output struct and in the \
             fragment's extras, at its own location:\n{source}"
        );
    }
    assert!(
        source.contains(&format!(
            "out.{} = base.{};",
            abi::CURRENT_CLIP_FIELD,
            abi::CURRENT_CLIP_FIELD
        )) && source.contains(&format!(
            "out.{} = base.{};",
            abi::PREVIOUS_CLIP_FIELD,
            abi::PREVIOUS_CLIP_FIELD
        )),
        "the vertex entry copies both like any other field: {source}"
    );
    assert!(
        source.contains(&format!(
            "return screen_motion(extra.{}, extra.{});",
            abi::CURRENT_CLIP_FIELD,
            abi::PREVIOUS_CLIP_FIELD
        )),
        "the fragment reads both from `extra`: {source}"
    );
}

#[test]
fn each_stage_emits_its_own_entry_point_and_imports_only_what_it_calls() {
    let registry = registry();
    let mut graph = Graph::new("stages");
    graph.add_node(abi::SURFACE_OUTPUT_ID);

    let module = |stage: abi::MaterialStage| {
        let options = CodegenOptions {
            stage,
            ..CodegenOptions::default()
        };
        generate(&graph, &registry, &options)
            .expect("compiles")
            .source
    };

    let forward = module(abi::MaterialStage::FORWARD_LIT);
    assert!(forward.contains("fn fs_forward_lit(vertex: VertexOut) -> @location(0) vec4f"));
    assert!(forward.contains("shade_surface"));
    assert!(!forward.contains("pack_gbuffer"));

    let gbuffer = module(abi::MaterialStage::GBUFFER);
    assert!(gbuffer.contains("fn fs_gbuffer(vertex: VertexOut) -> GBuffer"));
    assert!(gbuffer.contains("pack_gbuffer"));
    assert!(!gbuffer.contains("shade_surface"));

    // Depth only: a vertex entry and *nothing else*. Not the
    // shading function, not the G-buffer, and — since this graph
    // neither displaces nor discards — not the material function
    // either, nor any fragment stage at all
    // ([ADR 0025](../../../docs/adr/0025-a-material-graph-spans-shader-stages.md)).
    let depth = module(abi::MaterialStage::DEPTH_ONLY);
    assert!(depth.contains("fn vs_main(input: VertexIn) -> VertexOut"));
    assert!(!depth.contains("@fragment"));
    assert!(!depth.contains("shade_surface"));
    assert!(!depth.contains("pack_gbuffer"));
    assert!(!depth.contains("fn wxsl_material"));
    // The shadow stage is the same shape, from a light's point of view.
    let shadow = module(abi::MaterialStage::SHADOW);
    assert!(!shadow.contains("fn wxsl_material"));

    // Four stages, four different modules — which is what lets the
    // variant cache tell them apart by source hash alone. Depth and
    // shadow differ only in the stage comment their macros carry,
    // which is enough and is why the key holds the stage too.
    for pair in [(&forward, &gbuffer), (&forward, &depth), (&gbuffer, &depth)] {
        assert_ne!(pair.0, pair.1);
    }
}

#[test]
fn a_debug_view_replaces_the_forward_shading_and_leaves_the_gbuffer_alone() {
    let registry = registry();
    let mut graph = Graph::new("views");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let module = |stage: abi::MaterialStage, view: i32| {
        let mut options = CodegenOptions {
            stage,
            ..CodegenOptions::default()
        };
        options.material.macros.set(
            crate::lighting::DEBUG_VIEW_MACRO,
            crate::macros::MacroValue::Int(view),
        );
        generate(&graph, &registry, &options)
            .expect("compiles")
            .source
    };
    let views = crate::lighting::debug_views();
    let id = |label: &str| views.iter().find(|view| view.label == label).unwrap().id;

    let uv = module(abi::MaterialStage::FORWARD_LIT, id("uv"));
    assert!(uv.contains("The `uv` debug view in place of shading"));
    assert!(uv.contains("fract(ctx.uv)"));
    assert!(!uv.contains("light_count"), "no light loop: {uv}");
    // 0 shades, and a G-buffer channel has no forward expression.
    let shaded = module(abi::MaterialStage::FORWARD_LIT, 0);
    assert!(shaded.contains("light_count") && !shaded.contains("debug view"));
    let channel = views
        .iter()
        .find(|view| view.stage == crate::lighting::DebugStage::Deferred)
        .expect("a shipped feature channel");
    assert!(crate::lighting::debug_surface(channel.id, 0).is_none());
    // The G-buffer is the lighting pass's to read the view from.
    let gbuffer = module(abi::MaterialStage::GBUFFER, id("albedo"));
    assert!(!gbuffer.contains("debug view"));
    // The instance row rides in the uv varying, under that view only.
    let carried = "out.uv = vec2f(f32(input.instance), 0.0);";
    assert!(module(abi::MaterialStage::FORWARD_LIT, id("instance id")).contains(carried));
    assert!(!uv.contains(carried));
    // The material id is a constant colour per material.
    let material = |index: i32| {
        crate::lighting::debug_surface(id("material id"), index)
            .unwrap()
            .source
    };
    assert_ne!(material(1), material(2));
}

#[test]
fn nodes_are_emitted_in_dependency_order_with_typed_bindings() {
    let registry = registry();
    let mut graph = Graph::new("chain");
    let uv = graph.add_node("input.uv");
    let time = graph.add_node("input.time");
    let scaled = graph.add(Node::new("math.multiply.f32").with_param("b", Value::F32(3.0)));
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.wire(&registry, (time, "out"), (scaled, "a")).unwrap();
    graph
        .wire(&registry, (scaled, "out"), (out, "roughness"))
        .unwrap();
    // `uv` is wired to nothing: it must not appear in the output.
    let _ = uv;

    let shader = generate_default(&graph, &registry);
    assert!(
        shader.source.contains("let n3_out: f32 = ctx.time * 3.0f;"),
        "{}",
        shader.source
    );
    assert!(shader.source.contains("surface.roughness = n3_out;"));
    assert!(!shader.source.contains("ctx.uv"), "dead nodes are dropped");
}

#[test]
fn a_type_placeholder_emits_the_resolved_type_name() {
    // `{$T}` is how a template that has to *name* its type stays one
    // template: `convert.splat`'s constructor, or a `select` fallback of
    // the right width.
    let mut registry = registry();
    registry.register(
        NodeDefinition::builder("test.splat", "Splat")
            .generic_param(GenericParam::new("T", vec![ValueType::Vec3]))
            .input(Socket::new("value", ValueType::F32).with_splat_default(0.5))
            .output(Socket::new("out", ValueType::F32).generic("T"))
            .expr("{$T}({value})"),
    );
    let mut graph = Graph::new("splat");
    let splat = graph.add_node("test.splat");
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (splat, "out"), (out, "base_color"))
        .expect("T resolves to vec3f");

    let shader = generate_default(&graph, &registry);
    assert!(
        shader.source.contains("let n1_out: vec3f = vec3f(0.5f);"),
        "{}",
        shader.source
    );
}

#[test]
fn a_generic_function_node_writes_its_type_arguments() {
    // A generic `NodeBody::Call` node calls a WXSL *template*, and the
    // graph always knows the type exactly, so the type argument is
    // written rather than left to inference (ADR 0012).
    let mut registry = registry();
    registry.register(
        NodeDefinition::builder("test.generic_call", "Generic call")
            .generic_param(GenericParam::new(
                "T",
                vec![ValueType::Vec3, ValueType::Vec4],
            ))
            .call(WxslFunction::new(
                "package::test::identity",
                "identity",
                vec![Socket::new("v", ValueType::F32)
                    .generic("T")
                    .with_splat_default(1.0)],
                Socket::new("out", ValueType::F32).generic("T"),
            )),
    );
    let mut graph = Graph::new("call");
    let call = graph.add_node("test.generic_call");
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (call, "out"), (out, "base_color"))
        .expect("T resolves to vec3f");

    let shader = generate_default(&graph, &registry);
    assert!(
        shader
            .source
            .contains("let n1_call: vec3f = identity<vec3f>(vec3f(1.0, 1.0, 1.0));"),
        "{}",
        shader.source
    );
}

#[test]
fn a_generic_node_emits_its_resolved_type_not_the_placeholder() {
    let registry = registry();

    // Resolved to vec3f, via a connection.
    let mut vec_graph = Graph::new("vec");
    let color = vec_graph.add_node("input.world_position");
    let add =
        vec_graph.add(Node::new("test.generic_add").with_param("b", Value::Vec3([1.0, 2.0, 3.0])));
    let out = vec_graph.add_node(abi::SURFACE_OUTPUT_ID);
    vec_graph
        .wire(&registry, (color, "out"), (add, "a"))
        .unwrap();
    vec_graph
        .wire(&registry, (add, "out"), (out, "base_color"))
        .unwrap();
    let shader = generate_default(&vec_graph, &registry);
    assert!(
        shader.source.contains(": vec3f = ") && shader.source.contains(" + vec3f("),
        "expected a vec3f binding and a vec3f literal, got:\n{}",
        shader.source
    );
    assert!(!shader.source.contains(": f32 ="), "{}", shader.source);

    // The *same node definition*, resolved to f32 instead, in a graph
    // of its own — one registry entry serving two concrete types.
    let mut scalar_graph = Graph::new("scalar");
    let add = scalar_graph.add(
        Node::new("test.generic_add")
            .with_param("a", Value::F32(1.0))
            .with_param("b", Value::F32(2.0)),
    );
    scalar_graph
        .set_generic(&registry, add, "T", ValueType::F32)
        .expect("f32 is allowed");
    let out = scalar_graph.add_node(abi::SURFACE_OUTPUT_ID);
    scalar_graph
        .wire(&registry, (add, "out"), (out, "roughness"))
        .unwrap();
    let shader = generate_default(&scalar_graph, &registry);
    assert!(
        shader.source.contains(": f32 = 1.0f + 2.0f;"),
        "{}",
        shader.source
    );
}

#[test]
fn atoms_are_recognized_and_everything_else_is_wrapped() {
    for atom in [
        "n1_out",
        "ctx.world_normal",
        "0.5",
        "vec3f(1.0, 0.0, 0.0)",
        "f(g(x))",
    ] {
        assert_eq!(as_atom(atom), atom, "`{atom}` should not be wrapped");
    }
    for compound in ["-1.0", "a + b", "f(x) * 2.0", "(a)", ""] {
        assert_eq!(as_atom(compound), format!("({compound})"));
    }
}

#[test]
fn substituted_expressions_are_parenthesized_when_needed() {
    let registry = registry();
    let mut graph = Graph::new("negate");
    let negate = graph.add(Node::new("math.negate.f32").with_param("a", Value::F32(-1.0)));
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (negate, "out"), (out, "metallic"))
        .unwrap();

    let shader = generate_default(&graph, &registry);
    assert!(
        shader.source.contains("let n1_out: f32 = -(-1.0f);"),
        "{}",
        shader.source
    );
}

#[test]
fn struct_returning_functions_are_called_once_for_all_outputs() {
    let registry = registry();
    let mut graph = Graph::new("split");
    let split = graph.add(Node::new("test.split").with_param("value", Value::F32(0.5)));
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (split, "low"), (out, "metallic"))
        .unwrap();
    graph
        .wire(&registry, (split, "high"), (out, "roughness"))
        .unwrap();

    let shader = generate_default(&graph, &registry);
    assert_eq!(shader.source.matches("split_value(").count(), 1);
    assert!(shader.source.contains("surface.metallic = n1_call.low;"));
    assert!(shader.source.contains("surface.roughness = n1_call.high;"));
    assert!(shader
        .source
        .contains("import package::test::{SplitResult, split_value};"));
}

#[test]
fn macros_reach_the_shader_as_consts_and_features() {
    let registry = registry();
    let mut graph = Graph::new("macros");
    let user = graph.add_node("test.macro_user");
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (user, "out"), (out, "roughness"))
        .unwrap();

    // Declared defaults apply until the graph pins something else.
    let shader = generate_default(&graph, &registry);
    assert_eq!(
        shader.macros.get("WXSL_TEST_SCALE"),
        Some(MacroValue::Float(2.0))
    );
    assert_eq!(
        shader.macros.get("wxsl_test_flag"),
        Some(MacroValue::Flag(false))
    );
    // Both kinds are *declared* in the generated module now, with their
    // effective values as defaults: a WXSL module declares the knobs it
    // uses, so there is no shared macro module to import from
    // (ADR 0011). One concept covers flags and numbers alike.
    assert!(
        shader
            .source
            .contains("@macro const WXSL_TEST_SCALE: f32 = 2.0;"),
        "{}",
        shader.source
    );
    assert!(
        shader
            .source
            .contains("@macro const wxsl_test_flag: bool = false;"),
        "{}",
        shader.source
    );
    assert!(!shader.source.contains("package::wxsl::macros"));

    graph.set_macro("WXSL_TEST_SCALE", MacroValue::Float(8.0));
    graph.set_macro("wxsl_test_flag", MacroValue::Flag(true));
    let pinned = generate_default(&graph, &registry);
    assert!(
        pinned
            .source
            .contains("@macro const WXSL_TEST_SCALE: f32 = 8.0;"),
        "{}",
        pinned.source
    );
    assert_eq!(
        pinned.macros.get("wxsl_test_flag"),
        Some(MacroValue::Flag(true))
    );

    // Changing a macro now changes the root module itself, because the
    // values are declared in it: the header comment and one declaration
    // per changed macro. Under the old design the root was byte-identical
    // and only the separate macro module differed, which is why
    // `variant_key` had to fold in the macro set rather than hash the
    // source.
    let differing: Vec<(&str, &str)> = shader
        .source
        .lines()
        .zip(pinned.source.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(differing.len(), 3, "{differing:?}");
    assert!(differing[0].0.starts_with("// Macros:"));
    assert!(differing[1].0.contains("WXSL_TEST_SCALE: f32 = 2.0"));
    assert!(differing[2].0.contains("wxsl_test_flag: bool = false"));

    // It still folds in the macro set, and still has to: the bindings
    // also reach *imported* modules, whose own `@macro const` defaults
    // are nowhere in this source.
    assert_ne!(shader.variant_key(), pinned.variant_key());
}

#[test]
fn a_graph_with_no_output_node_is_an_error() {
    let registry = registry();
    let mut graph = Graph::new("headless");
    graph.add_node("input.uv");
    assert_eq!(
        generate(&graph, &registry, &CodegenOptions::default()),
        Err(CodegenError::NoOutputNode)
    );

    // Two of them is now reported by validation, which runs first
    // and reports every duplicated terminal under one rule.
    // `MultipleOutputNodes` stays as the belt-and-braces path for a
    // caller that reached codegen without validating.
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let Err(CodegenError::Invalid(errors)) =
        generate(&graph, &registry, &CodegenOptions::default())
    else {
        panic!("two surface outputs is an error");
    };
    assert!(
        errors.0.iter().any(
            |error| matches!(error, GraphError::DuplicateOutput { nodes, .. }
                if nodes.len() == 2)
        ),
        "{errors:?}"
    );
}

#[test]
fn generation_is_reproducible() {
    let registry = registry();
    let mut graph = Graph::new("repro");
    let out = graph.add_node(abi::SURFACE_OUTPUT_ID);
    let time = graph.add_node("input.time");
    graph
        .wire(&registry, (time, "out"), (out, "roughness"))
        .unwrap();
    let first = generate_default(&graph, &registry);
    let second = generate_default(&graph, &registry);
    assert_eq!(first, second);
    assert_eq!(first.variant_key(), second.variant_key());
}

#[test]
fn entry_points_can_be_left_out() {
    let registry = registry();
    let mut graph = Graph::new("material-only");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let options = CodegenOptions {
        emit_entry_points: false,
        ..CodegenOptions::default()
    };
    let shader = generate(&graph, &registry, &options).unwrap();
    assert!(!shader.source.contains("@vertex"));
    assert!(!shader.source.contains("@fragment"));
    assert!(shader.source.contains("fn wxsl_material"));
}

// -- bakes (ADR 0045) -----------------------------------------------

use crate::graph::BakeDecl;

/// A material whose roughness is an expensive term: two chained
/// multiplies the bake will stand in for. The cone is deliberately
/// constant — purity is about *what* the cone may read, and the
/// no-context case is the simplest one that exercises the machinery.
fn bake_graph(registry: &NodeRegistry) -> (Graph, NodeId) {
    let mut graph = Graph::new("baked");
    let expensive = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
    let more = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.5)));
    let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    graph
        .wire(registry, (expensive, "out"), (more, "b"))
        .unwrap();
    graph
        .wire(registry, (more, "out"), (out, "roughness"))
        .unwrap();
    graph.declare_bake(BakeDecl {
        node: more,
        socket: None,
        texture: "roughness_bake".to_string(),
        size: [64, 64],
        precision: crate::abi::GBufferPrecision::HighDynamicRange,
    });
    (graph, more)
}

#[test]
fn a_baked_term_is_one_sample_and_its_cone_leaves_the_module() {
    let registry = registry();
    let (graph, _) = bake_graph(&registry);

    // Baked: one filtered sample stands in for the term, and the cone
    // is not in the module at all — the cost change the toggle sells.
    let baked = generate_default(&graph, &registry);
    assert!(
        baked.source.contains(
            "surface.roughness = textureSample(roughness_bake, \
             roughness_bake_sampler, ctx.uv).x;",
        ),
        "{}",
        baked.source
    );
    // The cone's multiplies are gone from the fragment side: neither
    // the cone node's binding nor the baked node's own appears.
    assert!(
        !baked.source.contains("surface.roughness = (n"),
        "{}",
        baked.source
    );
    assert!(!baked.source.contains("let n1_out"), "{}", baked.source);
    assert!(!baked.source.contains("let n2_out"), "{}", baked.source);
    // The declaration turned into two bindings — the table and its
    // sampler — at material-group bindings.
    assert!(
        baked
            .source
            .contains("@group(1) @binding(1) var roughness_bake: texture_2d<f32>;"),
        "{}",
        baked.source
    );
    assert!(
        baked
            .source
            .contains("@group(1) @binding(2) var roughness_bake_sampler: sampler;"),
        "{}",
        baked.source
    );
    assert_eq!(baked.interface.resources.len(), 2);
    assert!(baked.interface.resources[0].bake.is_some());

    // Inline: the same graph with the toggle off emits the cone and no
    // sample — and the two module sources differ, which is what gives
    // the variant cache two shaders.
    let inline = generate(
        &graph,
        &registry,
        &CodegenOptions {
            material: crate::material::ResolvedMaterialConfig {
                bakes: false,
                ..crate::material::ResolvedMaterialConfig::default()
            },
            ..CodegenOptions::default()
        },
    )
    .unwrap();
    assert!(
        !inline.source.contains("textureSample("),
        "{}",
        inline.source
    );
    // The cone is back: both multiplies emitted into the material.
    assert!(inline.source.contains("let n1_out"), "{}", inline.source);
    assert!(inline.source.contains("let n2_out"), "{}", inline.source);
    assert_ne!(baked.source, inline.source);
}

#[test]
fn a_bake_generates_the_dispatch_that_fills_its_table() {
    let registry = registry();
    let (graph, more) = bake_graph(&registry);
    let decl = graph.bake("roughness_bake").unwrap();
    let generated = generate_bake(&graph, decl, &registry, &BakeOptions::default()).unwrap();

    assert_eq!(generated.entry, abi::BAKE_ENTRY);
    assert_eq!(generated.workgroups, [8, 8, 1]);
    assert_eq!(generated.size, [64, 64]);
    assert!(
        generated.source.contains(
            "@group(3) @binding(0) var wxsl_bake_target: \
                       texture_storage_2d<rgba16float, write>;"
        ),
        "{}",
        generated.source
    );
    assert!(
        generated
            .source
            .contains("fn wxsl_bake_value(uv: vec2f) -> vec4f {"),
        "{}",
        generated.source
    );
    // The cone, computed inside the value function...
    assert!(
        generated.source.contains("0.5f * n1_out"),
        "{}",
        generated.source
    );
    // ...padded to the table's four channels...
    assert!(
        generated
            .source
            .contains("return vec4f(n2_out, 0.0, 0.0, 1.0);"),
        "{}",
        generated.source
    );
    // ...and dispatched, guarded by the table's own extent.
    assert!(
        generated.source.contains("if (id.x >= 64u || id.y >= 64u)"),
        "{}",
        generated.source
    );
    assert!(
        generated
            .source
            .contains("textureStore(wxsl_bake_target, vec2i(id.xy), wxsl_bake_value(uv));"),
        "{}",
        generated.source
    );

    // The declaration is addressable by the table's name, which is how
    // `Effect::from_bake` finds it.
    assert_eq!(decl.node, more);
}

#[test]
fn a_cone_that_reads_more_than_the_domain_is_refused_by_name() {
    let mut registry = registry();
    registry.register_all([NodeDefinition::builder("test.param", "Parameter")
        .setting(crate::node::SettingDef::new(
            node::SETTING_NAME,
            "Name",
            "The uniform field.",
        ))
        .input(Socket::new("value", ValueType::F32).with_splat_default(0.0))
        .output(Socket::new("out", ValueType::F32))
        .declaration(NodeBody::Param)]);
    let mut graph = Graph::new("impure");
    let param = graph.add(Node::new("test.param").with_param("value", Value::F32(3.0)));
    graph.set_setting(param, node::SETTING_NAME, "wobble");
    let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    graph
        .wire(&registry, (param, "out"), (out, "roughness"))
        .unwrap();
    graph.declare_bake(BakeDecl {
        node: param,
        socket: None,
        texture: "roughness_bake".to_string(),
        size: [64, 64],
        precision: crate::abi::GBufferPrecision::HighDynamicRange,
    });
    let error = generate_bake(
        &graph,
        graph.bake("roughness_bake").unwrap(),
        &registry,
        &BakeOptions::default(),
    )
    .expect_err("a bake over a material parameter is not a function of the domain");
    let message = error.to_string();
    assert!(
        message.contains("roughness_bake") && message.contains("material parameter"),
        "{message}"
    );
}

#[test]
fn a_cone_shared_outside_the_bake_is_refused() {
    let registry = registry();
    let mut graph = Graph::new("shared");
    let expensive = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
    let baked = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.5)));
    let other = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.75)));
    let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    graph
        .wire(&registry, (expensive, "out"), (baked, "b"))
        .unwrap();
    graph
        .wire(&registry, (expensive, "out"), (other, "b"))
        .unwrap();
    graph
        .wire(&registry, (baked, "out"), (out, "roughness"))
        .unwrap();
    graph
        .wire(&registry, (other, "out"), (out, "metallic"))
        .unwrap();
    graph.declare_bake(BakeDecl {
        node: baked,
        socket: None,
        texture: "roughness_bake".to_string(),
        size: [64, 64],
        precision: crate::abi::GBufferPrecision::HighDynamicRange,
    });
    let error = generate(&graph, &registry, &CodegenOptions::default())
        .expect_err("the cone feeds `metallic` too, and the bake would go dark");
    let message = error.to_string();
    assert!(
        message.contains("roughness_bake") && message.contains("also feeds"),
        "{message}"
    );
}

#[test]
fn a_baked_node_the_vertex_stage_reaches_is_refused() {
    let mut registry = registry();
    registry.register(abi::vertex_output_def());
    registry.register_all([NodeDefinition::builder("test.vec3", "To vec3")
        .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
        .output(Socket::new("out", ValueType::Vec3))
        .expr("vec3f({a})")]);
    let registry = registry;
    let mut graph = Graph::new("vertex");
    let term = graph.add(Node::new("math.multiply.f32").with_param("a", Value::F32(0.25)));
    let widened = graph.add(Node::new("test.vec3"));
    let out = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
    graph
        .wire(&registry, (term, "out"), (out, "roughness"))
        .unwrap();
    graph
        .wire(&registry, (term, "out"), (widened, "a"))
        .unwrap();
    graph
        .wire(
            &registry,
            (widened, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .unwrap();
    graph.declare_bake(BakeDecl {
        node: term,
        socket: None,
        texture: "roughness_bake".to_string(),
        size: [64, 64],
        precision: crate::abi::GBufferPrecision::HighDynamicRange,
    });
    let error = generate(&graph, &registry, &CodegenOptions::default())
        .expect_err("textureSample does not run in the vertex stage");
    assert!(error.to_string().contains("vertex"), "{error}");
}

/// A fragment chain (ADR 0089): the colour stages return what the chain
/// makes of the shaded surface; the G-buffer stage shades forward through
/// the preshaded route, or says by name that the set cannot route it.
#[test]
fn a_fragment_chain_rewrites_the_shaded_colour() {
    let mut registry = registry();
    registry.register(abi::fragment_output_def());
    registry.register(
        NodeDefinition::builder("test.halve", "Halve")
            .input(Socket::new("a", ValueType::Vec4).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::Vec4))
            .expr("{a} * 0.5"),
    );
    let mut graph = Graph::new("chain");
    let surface = graph.add_node(abi::SURFACE_OUTPUT_ID);
    let fragment = graph.add_node(abi::FRAGMENT_OUTPUT_ID);

    // Placed but unwired, it changes nothing.
    let plain = generate_default(&graph, &registry).source;
    assert!(!plain.contains(abi::FRAGMENT_FN), "{plain}");

    let halve = graph.add_node("test.halve");
    graph
        .wire(&registry, (surface, abi::SOCKET_SHADED_COLOR), (halve, "a"))
        .expect("the shaded colour is a vec4");
    graph
        .wire(
            &registry,
            (halve, "out"),
            (fragment, abi::SOCKET_FRAGMENT_COLOR),
        )
        .expect("into the fragment output");
    let forward = generate_default(&graph, &registry).source;
    assert!(
        forward.contains("fn wxsl_fragment(ctx: SurfaceContext, wxsl_shaded: vec4f) -> vec4f {"),
        "{forward}"
    );
    assert!(forward.contains("wxsl_shaded * 0.5"), "{forward}");
    assert!(
        forward.contains("return wxsl_fragment(ctx, shade_surface(wxsl_material(ctx), ctx));"),
        "{forward}"
    );

    // Under deferred: no route in the default set, a named error.
    let gbuffer = CodegenOptions {
        stage: abi::MaterialStage::GBUFFER,
        ..CodegenOptions::default()
    };
    assert_eq!(
        generate(&graph, &registry, &gbuffer).unwrap_err(),
        CodegenError::FragmentNeedsPreshaded
    );
    // With the route: the stage shades, runs the chain, and packs it.
    let set = crate::lighting::default_set().unwrap();
    let routed = CodegenOptions {
        material: crate::material::MaterialConfig::default()
            .resolve(&set)
            .unwrap(),
        ..gbuffer
    };
    let packed = generate(&graph, &registry, &routed).unwrap().source;
    assert!(
        packed.contains(
            "return pack_gbuffer(surface, wxsl_fragment(ctx, shade_surface(surface, ctx)), 5u);"
        ),
        "{packed}"
    );
}
