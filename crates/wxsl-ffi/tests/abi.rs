//! The public C surface must agree exactly with the native Rust planning/variant path.

use std::borrow::Cow;

use serde_json::{json, Value as Json};
use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::codegen;
use wxsl_core::graph::{AttributeDecl, AttributeFrequency, Graph, Node, UserBlockDecl, UserField};
use wxsl_core::macros::{MacroSet, MacroValue};
use wxsl_core::material::MaterialConfig;
use wxsl_core::node::{Value, ValueType};
use wxsl_ffi::ffi::*;
use wxsl_frame::effect::{EffectRegistry, BLOOM};
use wxsl_frame::pipeline::{PipelineConfig, StockPipeline, TargetConfig};
use wxsl_frame::types::TextureFormat;
use wxsl_render::library::ShaderLibrary;
use wxsl_render::material::Material;
use wxsl_render::variants::{self, EffectRequest};

type Operation = unsafe extern "C" fn(u32, WxslBytes) -> *mut WxslResult;

struct ResultOwner(*mut WxslResult);

impl Drop for ResultOwner {
    fn drop(&mut self) {
        // SAFETY: this guard owns the result and is its sole freeing owner.
        unsafe { wxsl_result_free(self.0) }
    }
}

impl ResultOwner {
    fn call(operation: Operation, request: Json) -> Self {
        let bytes = serde_json::to_vec(&request).unwrap();
        // SAFETY: input stays readable during the call. Dropping it proves output ownership.
        Self(unsafe {
            operation(
                wxsl_abi_version(),
                WxslBytes {
                    data: bytes.as_ptr(),
                    len: bytes.len(),
                },
            )
        })
    }

    fn status(&self) -> WxslStatus {
        // SAFETY: the guard keeps the result live.
        unsafe { wxsl_result_status(self.0) }
    }

    fn json(&self) -> Json {
        // SAFETY: the result owns the nonempty JSON span until Drop.
        unsafe {
            let span = wxsl_result_json(self.0);
            serde_json::from_slice(std::slice::from_raw_parts(span.data, span.len)).unwrap()
        }
    }

    fn success(&self) {
        assert_eq!(self.status(), WxslStatus::Success, "{}", self.json());
    }

    fn bytes(&self, accessor: unsafe extern "C" fn(*const WxslResult) -> WxslBytes) -> Vec<u8> {
        // SAFETY: nonempty spans belong to the live result; empty pointers are never dereferenced.
        unsafe {
            let span = accessor(self.0);
            if span.len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(span.data, span.len).to_vec()
            }
        }
    }

    fn wgsl(&self) -> String {
        String::from_utf8(self.bytes(wxsl_result_wgsl)).unwrap()
    }

    fn fields(&self) -> &[WxslField] {
        // SAFETY: table belongs to this live result. Empty tables may have a null pointer.
        unsafe {
            let len = wxsl_result_field_count(self.0);
            if len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(wxsl_result_fields(self.0), len)
            }
        }
    }
}

fn field_text(span: WxslBytes) -> String {
    // SAFETY: callers only pass nonempty spans of a table kept live by ResultOwner.
    unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(span.data, span.len))
            .unwrap()
            .to_string()
    }
}

fn library() -> ShaderLibrary {
    let mut library = ShaderLibrary::new();
    library.insert_all(wxsl_stdlib::MODULES.iter().copied());
    library
}

#[test]
fn header_is_generated_from_the_exported_declarations() {
    assert_eq!(wxsl_ffi::C_HEADER, include_str!("../include/wxsl.h"),
        "regenerate with cargo run -p wxsl-ffi --example export_header -- crates/wxsl-ffi/include/wxsl.h");
}

#[test]
fn both_presets_export_the_actual_shared_plan_and_schedule() {
    let config = PipelineConfig::new(TargetConfig::new(800, 600, TextureFormat::Rgba8Unorm));
    for stock in StockPipeline::ALL {
        let output =
            ResultOwner::call(wxsl_compile_pipeline, json!({"pipeline": stock.document()}));
        output.success();
        let graph = stock.graph(&config);
        let payload = &output.json()["data"];
        assert_eq!(payload["graph"], serde_json::to_value(&graph).unwrap());
        assert_eq!(
            payload["schedule"],
            serde_json::to_value(graph.schedule().unwrap()).unwrap()
        );
        assert_eq!(
            payload["target"],
            serde_json::to_value(config.target).unwrap()
        );
        assert_eq!(payload["graph"]["passes"].as_array().unwrap().len(), 7);
        assert!(payload["effects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effect| effect["id"] == "wxsl.tonemap"));
    }
}

#[test]
fn demo_wgsl_is_byte_identical_for_every_stage_and_macro_binding() {
    let graph: Graph =
        serde_json::from_str(include_str!("../../wxsl/assets/pbr_cube.wxsl.json")).unwrap();
    let registry = wxsl_stdlib::registry();
    for enabled in [false, true] {
        let mut macros = MacroSet::new();
        macros.set(abi::FEATURE_DEBUG_NORMALS, MacroValue::Flag(enabled));
        macros.set("WXSL_FBM_OCTAVES", MacroValue::Int(3));
        let config = MaterialConfig::with_macros(macros);
        let material = Material::with_config(&graph, &registry, &config).unwrap();
        for stage in MaterialStage::ALL {
            let output = ResultOwner::call(
                wxsl_compile_material,
                json!({
                    "graph": graph, "stage": stage.name(), "material": config,
                }),
            );
            output.success();
            let shader = material.shader(*stage);
            let expected = variants::compile(
                &library(),
                &[(codegen::MATERIAL_MODULE, Cow::Borrowed(&shader.source))],
                codegen::MATERIAL_MODULE,
                &shader.macros,
            )
            .unwrap();
            assert_eq!(output.wgsl(), expected, "{stage}, debug={enabled}");
            assert_eq!(
                output.json()["data"]["interface"],
                serde_json::to_value(&shader.interface).unwrap()
            );
            assert_eq!(
                output.json()["data"]["fragment_entry"],
                json!(shader.fragment_entry)
            );
            assert_eq!(
                output.bytes(wxsl_result_params),
                shader.interface.params.filled(&shader.interface.defaults)
            );
        }
    }
}

#[test]
fn raw_shader_bindings_and_diagnostics_match_variants() {
    let root = "package::test";
    let source = "@macro const LEVEL: i32 = 2;\n@macro const SCALE: f32 = 1.0;\n@macro const ON: bool = false;\n@compute @workgroup_size(1) fn cs() { @if(ON) let n = f32(LEVEL) * SCALE; }";
    let mut macros = MacroSet::new();
    macros.set("ON", MacroValue::Flag(true));
    macros.set("LEVEL", MacroValue::Int(7));
    macros.set("SCALE", MacroValue::Float(0.375));
    let output = ResultOwner::call(
        wxsl_compile_shader,
        json!({
            "root": root, "macros": macros, "library": {"modules": {root: source}},
        }),
    );
    output.success();
    let expected =
        variants::compile(&library(), &[(root, Cow::Borrowed(source))], root, &macros).unwrap();
    assert_eq!(output.wgsl(), expected);
    assert!(expected.contains("LEVEL: i32 = 7"));
    assert!(expected.contains("0.375"));
    let bad = ResultOwner::call(
        wxsl_compile_shader,
        json!({"root": root, "library": {"modules": {root: "fn bad( {"}}}),
    );
    assert_eq!(bad.status(), WxslStatus::CompileError);
    assert!(bad.json()["data"]["message"]
        .as_str()
        .unwrap()
        .contains(root));
    assert!(bad.wgsl().is_empty());
}

#[test]
fn shipped_effects_share_the_native_source_and_parameter_layout() {
    let set = wxsl_core::lighting::LightingSet::default();
    for effect in EffectRegistry::shipped().iter() {
        let output = ResultOwner::call(wxsl_compile_shader, json!({"effect": effect.id}));
        output.success();
        let request = EffectRequest::new(effect.clone(), &MacroSet::new(), &set, &[]);
        assert_eq!(
            output.wgsl(),
            request.compile(&library()).1.unwrap(),
            "{}",
            effect.id
        );
        assert_eq!(output.fields().len(), effect.param_layout().fields().len());
    }
    let bloom = ResultOwner::call(wxsl_compile_shader, json!({"effect": "bloom"}));
    bloom.success();
    let layout = BLOOM.param_layout();
    for (actual, expected) in bloom.fields().iter().zip(layout.fields()) {
        assert_eq!(field_text(actual.name), expected.name.as_str());
        assert_eq!(actual.offset, expected.offset);
        assert_eq!(actual.buffer, 3);
        assert_eq!(actual.group, abi::GROUP_PASS);
        assert_eq!(actual.binding, 1);
    }
}

#[test]
fn computed_field_tables_cover_every_type_and_all_three_material_buffers() {
    for ty in ValueType::ALL {
        let expression = match ty {
            ValueType::Bool => "select(0.0, 1.0, value)",
            ValueType::I32 | ValueType::U32 => "f32(value)",
            ValueType::F32 => "value",
            ValueType::Vec2 | ValueType::Vec3 | ValueType::Vec4 => "value.x",
            ValueType::Mat3 | ValueType::Mat4 => "value[0].x",
            _ => unreachable!(),
        };
        let path = "package::test::probe";
        let source = format!("// Probe\n//\n// Read a typed buffer field.\nfn probe(value: {}) -> f32 {{ return {expression}; }}", ty.wxsl_type());
        let mut registry = wxsl_stdlib::registry();
        registry.register(wxsl_lang::node_from_source(&source, path).unwrap());
        for buffer in 0..3 {
            let mut graph = Graph::new("layout probe");
            let value = ty.zero().unwrap();
            let reader = match buffer {
                0 => graph.add(
                    Node::new("param.value")
                        .with_setting("name", "value")
                        .with_param("value", value),
                ),
                1 => {
                    graph.set_user_block(UserBlockDecl {
                        name: "user".into(),
                        fields: vec![UserField {
                            name: "value".into(),
                            ty: *ty,
                        }],
                    });
                    graph.add(Node::new("input.user").with_setting("field", "value"))
                }
                _ => {
                    graph.declare_attribute(AttributeDecl {
                        name: "value".into(),
                        ty: *ty,
                        frequency: AttributeFrequency::Instance,
                    });
                    graph.add(Node::new("input.attribute").with_setting("name", "value"))
                }
            };
            if buffer == 0 {
                graph.set_generic(&registry, reader, "T", *ty).unwrap();
            }
            let probe = graph.add_node("test.probe");
            let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
            graph
                .wire(&registry, (reader, "out"), (probe, "value"))
                .unwrap();
            graph
                .wire(&registry, (probe, "out"), (output, "roughness"))
                .unwrap();
            let result = ResultOwner::call(
                wxsl_compile_material,
                json!({
                    "graph": graph, "stage": "forward_lit",
                    "library": {"modules": {path: source}, "node_modules": [path]},
                }),
            );
            result.success();
            let native = Material::from_graph(&graph, &registry).unwrap();
            let interface = native.interface();
            let layout = match buffer {
                0 => &interface.params,
                1 => &interface.user.as_ref().unwrap().layout,
                _ => interface.geometry.instance(),
            };
            let fields = result
                .fields()
                .iter()
                .filter(|field| field.buffer == buffer)
                .collect::<Vec<_>>();
            assert_eq!(fields.len(), layout.fields().len());
            for (actual, expected) in fields.into_iter().zip(layout.fields()) {
                assert_eq!(field_text(actual.name), expected.name.as_str());
                assert_eq!(field_text(actual.ty), ty.buffer_type().unwrap());
                assert_eq!(actual.offset, expected.offset);
                assert_eq!(actual.size, expected.size);
                assert_eq!(actual.align, ty.buffer_align().unwrap());
                assert_eq!(actual.buffer_size, layout.size());
                assert_eq!(actual.buffer_align, layout.align());
            }
        }
    }
}

#[test]
fn capability_refusal_and_enabled_feature_match_the_shared_check() {
    let scene: wxsl_core::scene::Scene =
        serde_json::from_str(include_str!("../../wxsl/assets/scene_check.scene.json")).unwrap();
    for enabled in [false, true] {
        let features = if enabled { vec!["subsurface"] } else { vec![] };
        let result = ResultOwner::call(
            wxsl_check_setup,
            json!({"pipeline": StockPipeline::Deferred.document(), "scene": scene, "config": {"features": features}}),
        );
        assert_eq!(
            result.status(),
            if enabled {
                WxslStatus::Success
            } else {
                WxslStatus::Incompatible
            }
        );
        let mut config =
            PipelineConfig::new(TargetConfig::new(800, 600, TextureFormat::Rgba8Unorm));
        config.features = wxsl_core::lighting::feature_requests(&features).unwrap();
        let setup = wxsl_frame::setup::RenderSetup::for_stock(
            StockPipeline::Deferred,
            EffectRegistry::shipped(),
            config,
        );
        let expected = setup
            .check(&scene, &wxsl_stdlib::registry())
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(result.json()["data"]["incompatibilities"], json!(expected));
    }
}

#[test]
fn packed_fields_keep_nonzero_offsets_matrix_padding_and_boolean_storage() {
    let path = "package::test::packed";
    let source = "// Packed\n//\n// Read several host fields.\nfn packed(matrix: mat3x3f, vector: vec3f, scalar: f32, enabled: bool) -> f32 { return matrix[0].x + vector.x + scalar + select(0.0, 1.0, enabled); }";
    let mut registry = wxsl_stdlib::registry();
    registry.register(wxsl_lang::node_from_source(source, path).unwrap());
    let mut graph = Graph::new("packed fields");
    let packed = graph.add_node("test.packed");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    for (name, value) in [
        ("matrix", Value::Mat3([0.25; 9])),
        ("vector", Value::Vec3([0.2, 0.3, 0.4])),
        ("scalar", Value::F32(0.75)),
        ("enabled", Value::Bool(true)),
    ] {
        let node = graph.add(
            Node::new("param.value")
                .with_setting("name", name)
                .with_param("value", value),
        );
        graph.set_generic(&registry, node, "T", value.ty()).unwrap();
        graph
            .wire(&registry, (node, "out"), (packed, name))
            .unwrap();
    }
    graph
        .wire(&registry, (packed, "out"), (output, "roughness"))
        .unwrap();
    let result = ResultOwner::call(
        wxsl_compile_material,
        json!({"graph": graph, "stage": "forward_lit", "library": {"modules": {path: source}, "node_modules": [path]}}),
    );
    result.success();
    let native = Material::from_graph(&graph, &registry).unwrap();
    assert_eq!(
        result.bytes(wxsl_result_params),
        native
            .interface()
            .params
            .filled(&native.interface().defaults)
    );
    let table = result
        .fields()
        .iter()
        .map(|field| (field_text(field.name), field.offset, field.size))
        .collect::<Vec<_>>();
    assert_eq!(
        table,
        vec![
            ("matrix".into(), 0, 48),
            ("vector".into(), 48, 12),
            ("enabled".into(), 60, 4),
            ("scalar".into(), 64, 4)
        ]
    );
    assert_eq!(result.bytes(wxsl_result_params).len(), 80);
    assert_eq!(
        &result.bytes(wxsl_result_params)[60..64],
        &1u32.to_le_bytes()
    );
}

#[test]
fn setup_refuses_invalid_pipelines_and_collects_all_scene_mismatches() {
    let scene = json!({"materials": [
        {"name": "first", "model": "unavailable", "graph": {"nodes": [{"id": 1, "def": "output.surface"}], "edges": []}},
        {"name": "second", "graph": {"nodes": [{"id": 1, "def": "missing.node"}], "edges": []}},
    ]});
    let result = ResultOwner::call(
        wxsl_check_setup,
        json!({"pipeline": StockPipeline::Forward.document(), "scene": scene}),
    );
    assert_eq!(result.status(), WxslStatus::Incompatible);
    let errors = result.json()["data"]["incompatibilities"].to_string();
    assert!(errors.contains("first") && errors.contains("unavailable"));
    assert!(errors.contains("second") && errors.contains("missing.node"));
    let result = ResultOwner::call(
        wxsl_check_setup,
        json!({"pipeline": {"domain": "document", "nodes": [], "edges": []}, "scene": {}}),
    );
    assert_eq!(result.status(), WxslStatus::Incompatible);
    assert!(!result.json()["data"]["incompatibilities"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn foreign_and_document_versions_are_refused_by_name() {
    // SAFETY: version mismatch is checked before the deliberately invalid input is read.
    let result = ResultOwner(unsafe {
        wxsl_compile_pipeline(
            wxsl_abi_version() + 1,
            WxslBytes {
                data: std::ptr::null(),
                len: usize::MAX,
            },
        )
    });
    assert_eq!(result.status(), WxslStatus::AbiMismatch);
    assert!(result.json()["data"]["message"]
        .as_str()
        .unwrap()
        .contains("C ABI version 2"));
    for key in ["abi", "version"] {
        let mut graph = serde_json::to_value(StockPipeline::Forward.document()).unwrap();
        graph[key] = json!(999);
        let result = ResultOwner::call(wxsl_compile_pipeline, json!({"pipeline": graph}));
        assert_eq!(result.status(), WxslStatus::InvalidDocument);
        assert!(result.json()["data"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains(if key == "abi" { "abi" } else { "version" }));
    }
}

#[test]
fn nulls_lengths_utf8_json_and_request_validation_are_recoverable() {
    // SAFETY: all invalid spans are rejected before reading; empty spans borrow no memory.
    unsafe {
        for span in [
            WxslBytes {
                data: std::ptr::null(),
                len: 1,
            },
            WxslBytes {
                data: 1usize as *const u8,
                len: usize::MAX,
            },
        ] {
            let result = ResultOwner(wxsl_compile_pipeline(wxsl_abi_version(), span));
            assert_eq!(result.status(), WxslStatus::InvalidArgument);
        }
        assert_eq!(
            wxsl_result_status(std::ptr::null()),
            WxslStatus::InvalidArgument
        );
        assert_eq!(wxsl_result_field_count(std::ptr::null()), 0);
        assert!(wxsl_result_fields(std::ptr::null()).is_null());
        assert!(wxsl_result_json(std::ptr::null()).data.is_null());
        wxsl_result_free(std::ptr::null_mut());
        for bytes in [&b""[..], &b"{"[..], &[0xff][..]] {
            let result = ResultOwner(wxsl_compile_pipeline(
                wxsl_abi_version(),
                WxslBytes {
                    data: bytes.as_ptr(),
                    len: bytes.len(),
                },
            ));
            assert_eq!(result.status(), WxslStatus::InvalidDocument);
        }
    }
    for request in [
        json!({"root": "bad path"}),
        json!({"root": "package::a", "effect": "bloom"}),
        json!({"root": "package::a", "version": 999}),
        json!({"root": "package::a", "abi": 999}),
        json!({"effect": "unknown"}),
    ] {
        let result = ResultOwner::call(wxsl_compile_shader, request);
        assert_ne!(result.status(), WxslStatus::Success);
        assert!(!result.json()["data"]["message"]
            .as_str()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn independent_calls_can_run_concurrently() {
    let threads = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..8 {
                    let result =
                        ResultOwner::call(wxsl_compile_shader, json!({"effect": "tonemap"}));
                    result.success();
                    assert!(result.wgsl().contains("tonemap_fs"));
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
}
