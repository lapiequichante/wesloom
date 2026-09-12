//! End-to-end tests for the half of the pipeline that needs no GPU: graph in,
//! valid WGSL out.
//!
//! These are the tests that catch the failure this project is most exposed
//! to — a node definition whose descriptor and WXSL disagree — because they
//! run the real WXSL compiler over the real shader sources for both render
//! paths. `tests/render_cube.rs` covers the part that needs a device.

use std::borrow::Cow;
use std::collections::BTreeMap;

use wxsl::core::abi;
use wxsl::core::abi::MaterialStage;
use wxsl::core::codegen;
use wxsl::core::graph::{AttributeDecl, Graph, Node, NodeId, UserBlockDecl, UserField};
use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::core::node::{self, NodeBody, NodeDefinition, NodeRegistry, Value, ValueType};
use wxsl::render::material::Material;
use wxsl::render::variants;

/// The demo graph, as shipped.
fn demo_graph() -> Graph {
    let json = include_str!("../assets/pbr_cube.wxsl.json");
    serde_json::from_str(json).expect("the shipped demo graph parses")
}

/// Compile a material to WGSL for `stage`, or return the diagnostic.
fn compile(material: &Material, stage: MaterialStage) -> Result<String, String> {
    let library = wxsl::stdlib_library();
    let shader = material.shader(stage);
    // The generated module declares its own macros, so the material source
    // is the only thing to mount alongside the library (ADR 0011).
    let extra = [(
        codegen::MATERIAL_MODULE,
        Cow::Borrowed(shader.source.as_str()),
    )];
    variants::compile(&library, &extra, codegen::MATERIAL_MODULE, &shader.macros)
        .map_err(|error| error.to_string())
}

fn compile_lighting_pass(macros: &MacroSet) -> Result<String, String> {
    let library = wxsl::stdlib_library();
    // The pass is generated from the enabled lighting models now (ADR 0028);
    // the default set of one is what these tests exercise.
    let source =
        wxsl_core::lighting::lighting_pass_source(&wxsl_core::lighting::LightingSet::default());
    let extra = [(abi::LIGHTING_PASS_MODULE, Cow::Owned(source))];
    variants::compile(&library, &extra, abi::LIGHTING_PASS_MODULE, macros)
        .map_err(|error| error.to_string())
}

#[test]
fn the_demo_graph_is_valid_and_round_trips_through_the_node_format() {
    let graph = demo_graph();
    let registry = wxsl::stdlib::registry();
    graph.validate(&registry).expect("the demo graph validates");

    // The format is the thing users edit, so it has to survive a round trip
    // including the macro values and the editor's node positions.
    let json = serde_json::to_string_pretty(&graph).expect("serializes");
    let reloaded: Graph = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(reloaded, graph);
    assert_eq!(
        graph.macros().get("WXSL_FBM_OCTAVES"),
        Some(MacroValue::Int(5))
    );
    assert!(graph
        .nodes()
        .any(|(_, node)| node.position.is_some() && node.label.is_some()));
    assert!(
        graph.nodes().any(|(_, node)| node.color.is_some()),
        "a node's own colour is part of the format too"
    );
}

#[test]
fn every_stage_compiles_from_the_same_graph() {
    let registry = wxsl::stdlib::registry();
    let material = Material::from_graph(&demo_graph(), &registry).expect("codegen succeeds");

    let forward = compile(&material, MaterialStage::FORWARD_LIT).expect("forward_lit compiles");
    let deferred = compile(&material, MaterialStage::GBUFFER).expect("gbuffer compiles");
    let depth = compile(&material, MaterialStage::DEPTH_ONLY).expect("depth_only compiles");

    // Same vertex entry, one fragment entry each: one graph, several
    // pipeline shapes (ADR 0005, generalized by ADR 0022).
    for wgsl in [&forward, &deferred, &depth] {
        assert!(wgsl.contains("fn vs_main"), "{wgsl}");
    }
    // The graph's nodes reached the stages that need a surface — and
    // deliberately *not* the one that does not. A depth prepass wants the
    // vertex offset and the alpha test, and the demo graph has neither,
    // so its module is the vertex entry and nothing else (ADR 0025).
    // This is what partitioning bought.
    for wgsl in [&forward, &deferred] {
        assert!(wgsl.contains("fbm3"), "{wgsl}");
    }
    assert!(
        !depth.contains("fbm3"),
        "the prepass compiled the surface:\n{depth}"
    );
    assert!(
        !depth.contains("fn fs_"),
        "a material that does not discard needs no fragment stage:\n{depth}"
    );
    assert!(forward.contains("fn fs_forward_lit"), "{forward}");
    assert!(deferred.contains("fn fs_gbuffer"), "{deferred}");

    // WXSL mangles imported names, so these look for the distinguishing
    // feature rather than an exact identifier: the forward fragment returns
    // one colour, the G-buffer one returns the struct.
    assert!(
        forward.contains("@location(0) vec4f") && !forward.contains("GBuffer"),
        "forward_lit writes one colour: {forward}"
    );
    assert!(
        deferred.contains("GBuffer") && !deferred.contains("-> @location(0) vec4f"),
        "gbuffer writes a G-buffer: {deferred}"
    );
    // Neither stage drags in the other's half of the ABI.
    assert!(!forward.contains("pack_gbuffer"), "{forward}");
    assert!(!deferred.contains("sample_light"), "{deferred}");

    // A depth-only module has no fragment stage at all: nothing to run per
    // pixel is the entire point of a depth prepass.
    assert!(!depth.contains("@fragment"), "{depth}");
    assert!(!depth.contains("pack_gbuffer"), "{depth}");
    assert!(!depth.contains("sample_light"), "{depth}");

    // The lighting pass is where the deferred pipeline's shading happens.
    let lighting = compile_lighting_pass(material.macros()).expect("lighting pass compiles");
    assert!(lighting.contains("fn lighting_vs"), "{lighting}");
    assert!(lighting.contains("fn lighting_fs"), "{lighting}");
    assert!(lighting.contains("sample_light"), "{lighting}");
}

#[test]
fn macro_variables_change_the_compiled_shader() {
    let registry = wxsl::stdlib::registry();

    // A numeric macro: the octave count is a loop bound, so its value has to
    // arrive as a const declaration in the compiled WGSL.
    let mut graph = demo_graph();
    graph.set_macro("WXSL_FBM_OCTAVES", MacroValue::Int(2));
    let two = Material::from_graph(&graph, &registry).unwrap();
    let two_wgsl = compile(&two, MaterialStage::FORWARD_LIT).unwrap();
    assert!(two_wgsl.contains("i32 = 2;"), "{two_wgsl}");

    graph.set_macro("WXSL_FBM_OCTAVES", MacroValue::Int(7));
    let seven = Material::from_graph(&graph, &registry).unwrap();
    let seven_wgsl = compile(&seven, MaterialStage::FORWARD_LIT).unwrap();
    assert!(seven_wgsl.contains("i32 = 7;"), "{seven_wgsl}");
    assert_ne!(
        two.shader(MaterialStage::FORWARD_LIT).variant_key(),
        seven.shader(MaterialStage::FORWARD_LIT).variant_key()
    );

    // A flag macro: ridged noise adds a fold the smooth variant lacks.
    graph.set_macro("wxsl_fbm_ridged", MacroValue::Flag(true));
    let ridged = Material::from_graph(&graph, &registry).unwrap();
    let ridged_wgsl = compile(&ridged, MaterialStage::FORWARD_LIT).unwrap();
    assert!(ridged_wgsl.contains("abs("), "{ridged_wgsl}");
    assert_ne!(ridged_wgsl, seven_wgsl);

    // An ABI flag: the debug view replaces the whole light loop, in both
    // paths, because both call the same shading function.
    let mut overrides = MacroSet::new();
    overrides.set(abi::FEATURE_DEBUG_NORMALS, MacroValue::Flag(true));
    let debug = Material::from_graph_with_macros(&graph, &registry, &overrides).unwrap();
    let debug_forward = compile(&debug, MaterialStage::FORWARD_LIT).unwrap();
    assert!(!debug_forward.contains("sample_light"), "{debug_forward}");
    let debug_lighting = compile_lighting_pass(debug.macros()).unwrap();
    assert!(!debug_lighting.contains("sample_light"), "{debug_lighting}");

    // Turning the tonemap off drops the curve, and nothing else.
    overrides.set(abi::FEATURE_DEBUG_NORMALS, MacroValue::Flag(false));
    overrides.set(abi::FEATURE_TONEMAP, MacroValue::Flag(false));
    let raw = Material::from_graph_with_macros(&graph, &registry, &overrides).unwrap();
    let raw_wgsl = compile(&raw, MaterialStage::FORWARD_LIT).unwrap();
    assert!(!raw_wgsl.contains("tonemap_filmic"), "{raw_wgsl}");
}

#[test]
fn unused_nodes_do_not_reach_the_shader() {
    let registry = wxsl::stdlib::registry();
    let mut graph = demo_graph();
    // A branch left dangling on the editor canvas costs nothing.
    graph.add(Node::new("sdf.sphere"));
    let material = Material::from_graph(&graph, &registry).unwrap();
    let wgsl = compile(&material, MaterialStage::FORWARD_LIT).unwrap();
    assert!(!wgsl.contains("sdf_sphere"), "{wgsl}");
}

/// Every combination of type arguments worth compiling `def` at.
///
/// The uniform ones — every declared parameter resolved to the same type —
/// plus, for a two-parameter node, each parameter's own allowed types
/// against a scalar in the other. That is where the interesting cases live:
/// `f32 * vec3f`, `mat3x3f * vec3f`, `vec3f + f32`. A full cross product
/// would mostly enumerate combinations WGSL rejects, which
/// `wxsl_core::node::TypeRule` is unit-tested on directly.
///
/// Combinations a `Socket::combine` rule cannot derive a type from are left
/// out: those are the ones the graph is *supposed* to reject.
fn type_assignments(def: &NodeDefinition) -> Vec<BTreeMap<String, ValueType>> {
    if def.generics.is_empty() {
        return vec![BTreeMap::new()];
    }
    let uniform = |ty: ValueType| -> Option<BTreeMap<String, ValueType>> {
        def.generics
            .iter()
            .all(|param| param.allowed.contains(&ty))
            .then(|| {
                def.generics
                    .iter()
                    .map(|param| (param.name.to_string(), ty))
                    .collect()
            })
    };

    let mut combinations: Vec<BTreeMap<String, ValueType>> = ValueType::ALL
        .iter()
        .filter_map(|ty| uniform(*ty))
        .collect();
    if let [first, second] = def.generics.as_slice() {
        for (varying, fixed) in [(first, second), (second, first)] {
            let scalar = if fixed.allowed.contains(&ValueType::F32) {
                ValueType::F32
            } else {
                fixed.allowed[0]
            };
            for &ty in &varying.allowed {
                combinations.push(
                    [
                        (varying.name.to_string(), ty),
                        (fixed.name.to_string(), scalar),
                    ]
                    .into_iter()
                    .collect(),
                );
            }
        }
    }
    combinations.retain(|assignment| {
        def.inputs
            .iter()
            .chain(&def.outputs)
            .all(|socket| match &socket.combine {
                Some(combined) => {
                    let a = assignment[combined.a.as_str()];
                    let b = assignment[combined.b.as_str()];
                    combined.rule.apply(a, b).is_some()
                }
                None => true,
            })
    });
    combinations.dedup();
    combinations
}

/// Wire `(node, socket)` into the surface output, inserting whatever
/// conversion the type needs, and return the graph.
///
/// This is what makes the coverage test below possible: a node's output only
/// reaches the compiler if something downstream consumes it.
///
/// `assignment` resolves every type parameter the definition declares (see
/// `wxsl_core::node::GenericParam`), one entry per parameter, so a node
/// whose parameters resolve independently — `math.multiply`'s `A` and `B`,
/// `vector.transform`'s `M` and `V` — is covered at combinations of them
/// and not only at "everything the same".
fn graph_using(
    registry: &NodeRegistry,
    def: &NodeDefinition,
    socket: &str,
    assignment: &BTreeMap<String, ValueType>,
) -> Option<Graph> {
    let mut graph = Graph::new(format!("coverage: {}.{socket}", def.id));
    // Declared once for every graph, whether or not this node reads it:
    // `input.user`'s type comes from the document, so a graph that holds
    // one has to say what the block looks like.
    graph.set_user_block(UserBlockDecl {
        name: "app".to_string(),
        fields: ValueType::ALL
            .iter()
            .map(|ty| UserField::new(format!("value_{}", ty.suffix()), *ty))
            .collect(),
    });
    let node = graph.add(Node::new(def.id.clone()));
    for (param, &ty) in assignment {
        graph.set_generic(registry, node, param, ty).ok()?;
    }
    if matches!(def.body, NodeBody::UserRead) {
        let ty = graph.effective_type(node, def.outputs.first()?)?;
        graph.set_setting(node, "field", format!("value_{}", ty.suffix()));
    }
    // Same again for `input.attribute`, whose type also comes from the
    // document — and which the *frequency* also comes from, so this
    // covers both backings across the type set. Per-instance, because a
    // vertex buffer cannot carry every type and this loop wants them all;
    // the per-vertex half is covered on hardware in
    // `material_geometry.rs`.
    if matches!(def.body, NodeBody::AttributeRead) {
        let ty = graph.effective_type(node, def.outputs.first()?)?;
        let name = format!("value_{}", ty.suffix());
        graph.declare_attribute(AttributeDecl::instance(&name, ty));
        graph.set_setting(node, node::SETTING_NAME, name);
    }
    // A generic input has no *fixed* default (see `Socket::generic`), only
    // possibly a scalar to spread over the type just resolved; anything
    // still unfed gets a pinned value of whatever it turned out to be.
    // A texture or sampler input has no value to pin at all, so it gets a
    // declaring node wired into it — which is the only way to feed one.
    for input in &def.inputs {
        let ty = graph.effective_type(node, input)?;
        if ty.is_resource() {
            let source = graph.add_node(declaring_node(ty)?);
            graph
                .wire(registry, (source, "out"), (node, input.name.as_str()))
                .ok()?;
            continue;
        }
        if input.default_for(ty).is_none() {
            let value = ty
                .splat(1.0)
                .or_else(|| ty.zero())
                .expect("every value type has a splat or a zero");
            graph.set_param(node, input.name.as_str(), value);
        }
    }
    let output = def.output(socket).expect("an output of this node");
    let produced = graph.effective_type(node, output)?;
    // Always a surface output, because a graph without one does not
    // compile — even when what is under test belongs to the other stage.
    let surface = graph.add_node(abi::SURFACE_OUTPUT_ID);

    // Adapt whatever it produces to a surface field, since sockets are
    // matched by exact type and the surface only takes f32 and vec3f.
    let (source, source_socket, field) = match produced {
        ValueType::F32 => (node, socket.to_string(), "roughness"),
        ValueType::Vec3 => (node, socket.to_string(), "base_color"),
        ValueType::Vec2 | ValueType::Vec4 => {
            let split = graph.add_node(format!("convert.split.{}", produced.suffix()));
            graph.wire(registry, (node, socket), (split, "v")).ok()?;
            (split, "x".to_string(), "roughness")
        }
        ValueType::Bool => {
            let select = graph.add_node("logic.select");
            graph
                .set_generic(registry, select, "T", ValueType::F32)
                .ok()?;
            graph
                .wire(registry, (node, socket), (select, "condition"))
                .ok()?;
            (select, "out".to_string(), "roughness")
        }
        // A matrix reaches the surface through the one node that consumes
        // one: multiplied by a vector of its own size.
        ValueType::Mat3 | ValueType::Mat4 => {
            let vector = if produced == ValueType::Mat3 {
                ValueType::Vec3
            } else {
                ValueType::Vec4
            };
            let transform = graph.add_node("vector.transform");
            graph.set_generic(registry, transform, "M", produced).ok()?;
            graph.set_generic(registry, transform, "V", vector).ok()?;
            graph.set_param(transform, "v", vector.splat(1.0)?);
            graph
                .wire(registry, (node, socket), (transform, "m"))
                .ok()?;
            if vector == ValueType::Vec3 {
                (transform, "out".to_string(), "base_color")
            } else {
                let split = graph.add_node("convert.split.vec4f");
                graph
                    .wire(registry, (transform, "out"), (split, "v"))
                    .ok()?;
                (split, "x".to_string(), "roughness")
            }
        }
        ValueType::I32 | ValueType::U32 => {
            let to_float = graph.add_node("convert.to_float");
            graph
                .wire(registry, (node, socket), (to_float, "value"))
                .ok()?;
            (to_float, "out".to_string(), "roughness")
        }
        // A texture reaches the surface through the node that samples
        // one, with a sampler declared alongside it.
        ValueType::Texture2d => {
            let sampler = graph.add_node("texture.sampler");
            let sample = graph.add_node("sample.texture_2d");
            graph.wire(registry, (node, socket), (sample, "tex")).ok()?;
            graph
                .wire(registry, (sampler, "out"), (sample, "samp"))
                .ok()?;
            let split = graph.add_node("convert.split.vec4f");
            graph.wire(registry, (sample, "out"), (split, "v")).ok()?;
            (split, "x".to_string(), "roughness")
        }
        // Nothing in the library samples a cube map yet, and a sampler on
        // its own reaches nothing: both are covered as *inputs* of
        // `sample.texture_2d` above rather than as outputs here.
        ValueType::TextureCube | ValueType::Sampler => return None,
    };
    // A vertex-only node reaches the *vertex* terminal instead: it reads
    // object space, which the fragment stage has not got, and
    // `Graph::validate` says so if it is wired the other way (ADR 0025).
    // That also gets these nodes compiled into the depth and shadow
    // stages, which is where a displacement most needs to be right.
    if def.is_vertex_only() {
        let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
        let (source, source_socket) = if field == "base_color" {
            (source, source_socket)
        } else {
            let splat = graph.add_node("convert.splat");
            graph
                .set_generic(registry, splat, "T", ValueType::Vec3)
                .ok()?;
            graph
                .wire(registry, (source, source_socket.as_str()), (splat, "value"))
                .ok()?;
            (splat, "out".to_string())
        };
        graph
            .wire(
                registry,
                (source, source_socket.as_str()),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .ok()?;
        return Some(graph);
    }
    graph
        .wire(registry, (source, source_socket.as_str()), (surface, field))
        .ok()?;
    Some(graph)
}

/// The node that declares a resource of type `ty`, for feeding a socket
/// that takes one. There is no literal texture to pin, so this is the
/// only way.
fn declaring_node(ty: ValueType) -> Option<&'static str> {
    match ty {
        ValueType::Texture2d => Some("texture.texture_2d"),
        ValueType::TextureCube => Some("texture.texture_cube"),
        ValueType::Sampler => Some("texture.sampler"),
        _ => None,
    }
}

#[test]
fn every_node_in_the_library_compiles_for_every_stage() {
    let registry = wxsl::stdlib::registry();
    let mut checked = 0usize;
    let mut checked_resource_inputs = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for def in registry.iter() {
        // The output node is the sink every graph below already has, and a
        // context reader is covered through whatever consumes it.
        if def.is_surface_output() {
            continue;
        }
        // A generic node's socket `ty` is only a placeholder (see
        // `wxsl_core::node::Socket::generic`) — every type its parameters
        // allow needs its own graph, or genericizing a family would lose
        // the per-type coverage this test exists to give. A non-generic
        // node gets exactly one graph, at the types it declares.
        for assignment in type_assignments(def) {
            for socket in &def.outputs {
                let types = assignment
                    .iter()
                    .map(|(param, ty)| format!("{param}={ty}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let label = if types.is_empty() {
                    format!("{}.{}", def.id, socket.name)
                } else {
                    format!("{}.{} ({types})", def.id, socket.name)
                };
                let Some(graph) = graph_using(&registry, def, socket.name.as_str(), &assignment)
                else {
                    skipped.push(label);
                    continue;
                };
                let material = match Material::from_graph(&graph, &registry) {
                    Ok(material) => material,
                    Err(error) => {
                        failures.push(format!("{label}: codegen: {error}"));
                        continue;
                    }
                };
                for stage in MaterialStage::ALL {
                    match compile(&material, *stage) {
                        Ok(wgsl) => {
                            // A node that compiled but got stripped would
                            // make this test vacuous — in the stages that
                            // compile the surface at all. A depth or
                            // shadow stage is *supposed* to leave it out
                            // (ADR 0025), and the test above asserts that
                            // it does.
                            if let (NodeBody::Call(func), true) = (&def.body, stage.needs_surface())
                            {
                                assert!(
                                    wgsl.contains(func.name.as_str()),
                                    "{label} compiled without calling {}:\n{wgsl}",
                                    func.name
                                );
                            }
                            checked += 1;
                            if def.inputs.iter().any(|input| input.ty.is_resource()) {
                                checked_resource_inputs += 1;
                            }
                        }
                        Err(diagnostic) => {
                            failures.push(format!("{label} on the {stage} stage:\n{diagnostic}"))
                        }
                    }
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
    assert!(checked > 400, "only {checked} compilations ran");
    assert!(
        skipped
            .iter()
            .all(|s| s.starts_with("input.") || s.starts_with("texture.")),
        "unexpected skips: {skipped:?}"
    );
    // A cube texture and a sampler are skipped as *outputs* — nothing in
    // the library consumes a cube map yet — but both are covered as
    // inputs of `sample.texture_2d`, so the resource types are not
    // silently untested.
    assert!(
        checked_resource_inputs > 0,
        "no node with a texture or sampler input was compiled"
    );
}

#[test]
fn a_struct_returning_function_is_called_once_for_all_its_outputs() {
    // `lighting.pbr_direct_split` is the library's struct-returning node: it
    // exists to be read twice without being evaluated twice.
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("split");
    let split = graph.add(Node::new("lighting.pbr_direct_split"));
    // Generic, resolved to vec3f as soon as the first wire below connects.
    let add = graph.add_node("math.add");
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
    // Checked on the generated WXSL, since that is what codegen decides;
    // the compiler then mangles the name on its way to WGSL.
    assert_eq!(
        material
            .wxsl(MaterialStage::FORWARD_LIT)
            .matches("pbr_direct_split(")
            .count(),
        1,
        "{}",
        material.wxsl(MaterialStage::FORWARD_LIT)
    );
    let wgsl = compile(&material, MaterialStage::FORWARD_LIT).unwrap();
    assert!(wgsl.contains(".diffuse + "), "{wgsl}");
    assert!(wgsl.contains(".specular"), "{wgsl}");
}

#[test]
fn an_invalid_graph_is_rejected_before_the_shader_compiler_sees_it() {
    let registry = wxsl::stdlib::registry();
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
    graph.remove_node(&registry, NodeId(3));
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

/// The UI pass compiles like anything else in the library.
///
/// The editor draws itself through this module (ADR 0013), so a break here
/// is a break in the editor's chrome, not in a material — and it is worth
/// catching without a GPU, since CI has none.
#[test]
fn the_ui_pass_compiles_to_wgsl() {
    let library = wxsl::stdlib_library();
    let extra: [(&str, Cow<'_, str>); 0] = [];
    let wgsl = variants::compile(&library, &extra, abi::UI_MODULE, &MacroSet::new())
        .expect("the UI pass compiles");
    assert!(
        wgsl.contains(&format!("fn {}", abi::UI_VERTEX_ENTRY)),
        "{wgsl}"
    );
    assert!(
        wgsl.contains(&format!("fn {}", abi::UI_FRAGMENT_ENTRY)),
        "{wgsl}"
    );
    // Integer vertex outputs have to stay flat-interpolated through the
    // compiler, or `wgpu` rejects the module.
    assert!(wgsl.contains("interpolate(flat)"), "{wgsl}");
    // The UI pass has no material graph and no camera: it must not pull the
    // frame group's uniforms in behind our back.
    assert!(!wgsl.contains("var<uniform> camera"), "{wgsl}");
}

/// The MSDF compute pass compiles, and declares what the ABI says it does.
///
/// The GPU half of the text stack (ADR 0014) is a compute shader, which is a
/// pipeline shape nothing else in the workspace uses — worth compiling in the
/// GPU-less suite so a break is a test failure rather than a blank editor.
#[test]
fn the_msdf_compute_pass_compiles_to_wgsl() {
    let library = wxsl::stdlib_library();
    let extra: [(&str, Cow<'_, str>); 0] = [];
    let wgsl = variants::compile(&library, &extra, abi::MSDF_MODULE, &MacroSet::new())
        .expect("the MSDF pass compiles");
    assert!(wgsl.contains(&format!("fn {}", abi::MSDF_ENTRY)), "{wgsl}");
    let [x, y] = abi::MSDF_WORKGROUP;
    assert!(
        wgsl.contains(&format!("@workgroup_size({x}, {y}, 1)")),
        "{wgsl}"
    );
    // The edge kinds are shared with `wxsl_render::ui::msdf_gpu`'s
    // `#[repr(C)]` buffers; a silent renumbering would shade every glyph as
    // the wrong curve.
    for (name, value) in [
        ("MSDF_LINE", abi::MSDF_EDGE_LINE),
        ("MSDF_QUAD", abi::MSDF_EDGE_QUAD),
        ("MSDF_CUBIC", abi::MSDF_EDGE_CUBIC),
    ] {
        let source = wxsl::stdlib::shaders::module(abi::MSDF_MODULE).expect("msdf module");
        assert!(
            source.contains(&format!("const {name}: u32 = {value}u;")),
            "msdf.wxsl disagrees with abi::MSDF_EDGE_* on {name}"
        );
    }
}
