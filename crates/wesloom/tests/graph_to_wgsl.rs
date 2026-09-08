//! End-to-end tests for the half of the pipeline that needs no GPU: graph in,
//! valid WGSL out.
//!
//! These are the tests that catch the failure this project is most exposed
//! to — a node definition whose descriptor and WESL disagree — because they
//! run the real `wesl` compiler over the real shader sources for both render
//! paths. `tests/render_cube.rs` covers the part that needs a device.

use std::borrow::Cow;

use wesloom::core::abi;
use wesloom::core::codegen;
use wesloom::core::graph::{Graph, Node, NodeId};
use wesloom::core::macros::{MacroSet, MacroValue};
use wesloom::core::node::{NodeBody, NodeRegistry, Value, ValueType};
use wesloom::render::material::Material;
use wesloom::render::variants;
use wesloom::render::RenderPath;

/// The demo graph, as shipped.
fn demo_graph() -> Graph {
    let json = include_str!("../assets/pbr_cube.wesloom.json");
    serde_json::from_str(json).expect("the shipped demo graph parses")
}

/// Compile a material to WGSL for `path`, or return the diagnostic.
fn compile(material: &Material, path: RenderPath) -> Result<String, String> {
    let library = wesloom::stdlib_library();
    let mut macros = material.macros().clone();
    path.apply_to(&mut macros);
    let extra = [
        (
            codegen::MATERIAL_MODULE,
            Cow::Borrowed(material.shader.source.as_str()),
        ),
        (
            abi::MACROS_MODULE,
            Cow::Owned(codegen::macro_module(&macros)),
        ),
    ];
    variants::compile(&library, &extra, codegen::MATERIAL_MODULE, &macros)
        .map_err(|error| error.to_string())
}

fn compile_lighting_pass(macros: &MacroSet) -> Result<String, String> {
    let library = wesloom::stdlib_library();
    let mut macros = macros.clone();
    RenderPath::Deferred.apply_to(&mut macros);
    let extra = [(
        abi::MACROS_MODULE,
        Cow::Owned(codegen::macro_module(&macros)),
    )];
    variants::compile(&library, &extra, abi::LIGHTING_PASS_MODULE, &macros)
        .map_err(|error| error.to_string())
}

#[test]
fn the_demo_graph_is_valid_and_round_trips_through_the_node_format() {
    let graph = demo_graph();
    let registry = wesloom::stdlib::registry();
    graph.validate(&registry).expect("the demo graph validates");

    // The format is the thing users edit, so it has to survive a round trip
    // including the macro values and the editor's node positions.
    let json = serde_json::to_string_pretty(&graph).expect("serializes");
    let reloaded: Graph = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(reloaded, graph);
    assert_eq!(
        graph.macros().get("WESLOOM_FBM_OCTAVES"),
        Some(MacroValue::Int(5))
    );
    assert!(graph
        .nodes()
        .any(|(_, node)| node.position.is_some() && node.label.is_some()));
}

#[test]
fn both_render_paths_compile_from_the_same_graph() {
    let registry = wesloom::stdlib::registry();
    let material = Material::from_graph(&demo_graph(), &registry).expect("codegen succeeds");

    let forward = compile(&material, RenderPath::Forward).expect("forward compiles");
    let deferred = compile(&material, RenderPath::Deferred).expect("deferred compiles");

    // Same entry point names, different fragment output: that is the whole
    // point of the conditional-translation approach in ADR 0005.
    for wgsl in [&forward, &deferred] {
        assert!(wgsl.contains("fn vs_main"), "{wgsl}");
        assert!(wgsl.contains("fn fs_main"), "{wgsl}");
        // The graph's nodes reached the shader.
        assert!(wgsl.contains("fbm3"), "{wgsl}");
    }
    // WESL mangles imported names, so these look for the distinguishing
    // feature rather than an exact identifier: the forward fragment returns
    // one colour, the deferred one returns the G-buffer struct.
    assert!(
        forward.contains("@location(0) vec4f") && !forward.contains("GBuffer"),
        "the forward path writes one colour: {forward}"
    );
    assert!(
        deferred.contains("GBuffer") && !deferred.contains("-> @location(0) vec4f"),
        "the deferred path writes a G-buffer: {deferred}"
    );
    // The forward path must not drag the G-buffer packing in, and the
    // deferred material pass must not drag the light loop in.
    assert!(!forward.contains("pack_gbuffer"), "{forward}");
    assert!(!deferred.contains("sample_light"), "{deferred}");

    // The lighting pass is where the deferred path's shading happens.
    let lighting = compile_lighting_pass(material.macros()).expect("lighting pass compiles");
    assert!(lighting.contains("fn lighting_vs"), "{lighting}");
    assert!(lighting.contains("fn lighting_fs"), "{lighting}");
    assert!(lighting.contains("sample_light"), "{lighting}");
}

#[test]
fn macro_variables_change_the_compiled_shader() {
    let registry = wesloom::stdlib::registry();

    // A numeric macro: the octave count is a loop bound, so its value has to
    // arrive as a const declaration in the compiled WGSL.
    let mut graph = demo_graph();
    graph.set_macro("WESLOOM_FBM_OCTAVES", MacroValue::Int(2));
    let two = Material::from_graph(&graph, &registry).unwrap();
    let two_wgsl = compile(&two, RenderPath::Forward).unwrap();
    assert!(two_wgsl.contains("i32 = 2;"), "{two_wgsl}");

    graph.set_macro("WESLOOM_FBM_OCTAVES", MacroValue::Int(7));
    let seven = Material::from_graph(&graph, &registry).unwrap();
    let seven_wgsl = compile(&seven, RenderPath::Forward).unwrap();
    assert!(seven_wgsl.contains("i32 = 7;"), "{seven_wgsl}");
    assert_ne!(two.shader.variant_key(), seven.shader.variant_key());

    // A flag macro: ridged noise adds a fold the smooth variant lacks.
    graph.set_macro("wesloom_fbm_ridged", MacroValue::Flag(true));
    let ridged = Material::from_graph(&graph, &registry).unwrap();
    let ridged_wgsl = compile(&ridged, RenderPath::Forward).unwrap();
    assert!(ridged_wgsl.contains("abs("), "{ridged_wgsl}");
    assert_ne!(ridged_wgsl, seven_wgsl);

    // An ABI flag: the debug view replaces the whole light loop, in both
    // paths, because both call the same shading function.
    let mut overrides = MacroSet::new();
    overrides.set(abi::FEATURE_DEBUG_NORMALS, MacroValue::Flag(true));
    let debug = Material::from_graph_with_macros(&graph, &registry, &overrides).unwrap();
    let debug_forward = compile(&debug, RenderPath::Forward).unwrap();
    assert!(!debug_forward.contains("sample_light"), "{debug_forward}");
    let debug_lighting = compile_lighting_pass(debug.macros()).unwrap();
    assert!(!debug_lighting.contains("sample_light"), "{debug_lighting}");

    // Turning the tonemap off drops the curve, and nothing else.
    overrides.set(abi::FEATURE_DEBUG_NORMALS, MacroValue::Flag(false));
    overrides.set(abi::FEATURE_TONEMAP, MacroValue::Flag(false));
    let raw = Material::from_graph_with_macros(&graph, &registry, &overrides).unwrap();
    let raw_wgsl = compile(&raw, RenderPath::Forward).unwrap();
    assert!(!raw_wgsl.contains("tonemap_filmic"), "{raw_wgsl}");
}

#[test]
fn unused_nodes_do_not_reach_the_shader() {
    let registry = wesloom::stdlib::registry();
    let mut graph = demo_graph();
    // A branch left dangling on the editor canvas costs nothing.
    graph.add(Node::new("sdf.sphere"));
    let material = Material::from_graph(&graph, &registry).unwrap();
    let wgsl = compile(&material, RenderPath::Forward).unwrap();
    assert!(!wgsl.contains("sdf_sphere"), "{wgsl}");
}

/// Wire `(node, socket)` into the surface output, inserting whatever
/// conversion the type needs, and return the graph.
///
/// This is what makes the coverage test below possible: a node's output only
/// reaches the compiler if something downstream consumes it.
fn graph_using(
    registry: &NodeRegistry,
    def_id: &str,
    socket: &str,
    ty: ValueType,
) -> Option<Graph> {
    let mut graph = Graph::new(format!("coverage: {def_id}.{socket}"));
    let node = graph.add(Node::new(def_id));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);

    // Adapt the output type to a surface field, since sockets are matched by
    // exact type and the surface only takes f32 and vec3f.
    let (source, source_socket, field) = match ty {
        ValueType::F32 => (node, socket.to_string(), "roughness"),
        ValueType::Vec3 => (node, socket.to_string(), "base_color"),
        ValueType::Vec2 | ValueType::Vec4 => {
            let split = graph.add_node(format!("convert.split.{}", ty.suffix()));
            graph.wire(registry, (node, socket), (split, "v")).ok()?;
            (split, "x".to_string(), "roughness")
        }
        ValueType::Bool => {
            let select = graph.add_node("logic.select.f32");
            graph
                .wire(registry, (node, socket), (select, "condition"))
                .ok()?;
            (select, "out".to_string(), "roughness")
        }
        ValueType::Mat3 => {
            let transform = graph.add_node("vector.transform.mat3");
            graph
                .wire(registry, (node, socket), (transform, "m"))
                .ok()?;
            (transform, "out".to_string(), "base_color")
        }
        // No node in the library produces these yet.
        ValueType::I32 | ValueType::U32 | ValueType::Mat4 => return None,
    };
    graph
        .wire(registry, (source, source_socket.as_str()), (output, field))
        .ok()?;
    Some(graph)
}

#[test]
fn every_node_in_the_library_compiles_on_both_paths() {
    let registry = wesloom::stdlib::registry();
    let mut checked = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for def in registry.iter() {
        // The output node is the sink every graph below already has, and a
        // context reader is covered through whatever consumes it.
        if def.is_surface_output() {
            continue;
        }
        for socket in &def.outputs {
            let Some(graph) = graph_using(&registry, &def.id, socket.name.as_str(), socket.ty)
            else {
                skipped.push(format!("{}.{}", def.id, socket.name));
                continue;
            };
            let material = match Material::from_graph(&graph, &registry) {
                Ok(material) => material,
                Err(error) => {
                    failures.push(format!("{}.{}: codegen: {error}", def.id, socket.name));
                    continue;
                }
            };
            for path in RenderPath::ALL {
                match compile(&material, *path) {
                    Ok(wgsl) => {
                        // A node that compiled but got stripped would make
                        // this test vacuous.
                        if let NodeBody::Call(func) = &def.body {
                            assert!(
                                wgsl.contains(func.name.as_str()),
                                "{} compiled without calling {}:\n{wgsl}",
                                def.id,
                                func.name
                            );
                        }
                        checked += 1;
                    }
                    Err(diagnostic) => failures.push(format!(
                        "{}.{} on the {path} path:\n{diagnostic}",
                        def.id, socket.name
                    )),
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} node compilations failed:\n\n{}",
        failures.len(),
        checked + failures.len(),
        failures.join("\n\n")
    );
    // Guard against the test silently covering nothing.
    assert!(checked > 200, "only {checked} compilations ran");
    assert!(
        skipped.iter().all(|s| s.starts_with("input.")),
        "unexpected skips: {skipped:?}"
    );
}

#[test]
fn a_struct_returning_function_is_called_once_for_all_its_outputs() {
    // `lighting.pbr_direct_split` is the library's struct-returning node: it
    // exists to be read twice without being evaluated twice.
    let registry = wesloom::stdlib::registry();
    let mut graph = Graph::new("split");
    let split = graph.add(Node::new("lighting.pbr_direct_split"));
    let add = graph.add_node("math.add.vec3f");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&registry, (split, "diffuse"), (add, "a"))
        .unwrap();
    graph
        .wire(&registry, (split, "specular"), (add, "b"))
        .unwrap();
    graph
        .wire(&registry, (add, "out"), (output, "emissive"))
        .unwrap();

    let material = Material::from_graph(&graph, &registry).unwrap();
    // Checked on the generated WESL, since that is what codegen decides;
    // the compiler then mangles the name on its way to WGSL.
    assert_eq!(
        material.wesl().matches("pbr_direct_split(").count(),
        1,
        "{}",
        material.wesl()
    );
    let wgsl = compile(&material, RenderPath::Forward).unwrap();
    assert!(wgsl.contains(".diffuse + "), "{wgsl}");
    assert!(wgsl.contains(".specular"), "{wgsl}");
}

#[test]
fn an_invalid_graph_is_rejected_before_the_shader_compiler_sees_it() {
    let registry = wesloom::stdlib::registry();
    let mut graph = Graph::new("broken");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    // A vec3 socket fed a float parameter: caught by validation, with a
    // message naming the socket rather than a WGSL type error.
    graph.set_param(output, "base_color", Value::F32(1.0));
    let error = Material::from_graph(&graph, &registry).expect_err("invalid graph");
    let message = error.to_string();
    assert!(message.contains("base_color"), "{message}");
    assert!(message.contains("f32"), "{message}");

    // Removing a node through the API takes its edges with it, so a
    // dangling edge can only arrive from a hand-edited file. Validation
    // still catches it, and names the node that is missing.
    let mut graph = demo_graph();
    graph.remove_node(NodeId(3));
    Material::from_graph(&graph, &registry).expect("removing a node leaves the graph valid");

    let dangling: Graph = serde_json::from_str(
        r#"{
            "name": "dangling",
            "nodes": [{ "id": 1, "def": "output.surface" }],
            "edges": [
                {
                    "from": { "node": 99, "socket": "out" },
                    "to": { "node": 1, "socket": "base_color" }
                }
            ]
        }"#,
    )
    .expect("parses");
    let error = Material::from_graph(&dangling, &registry).expect_err("dangling edge");
    assert!(error.to_string().contains("no such node"), "{error}");
}
