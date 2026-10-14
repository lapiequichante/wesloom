//! The release gate (plan3 N8, ADR 0077): every shader a shipped
//! application can be asked to compile, compiled through the shipped
//! surface and validated, with no device and no renderer linked.
//!
//! The corpus gate (`wxsl-stdlib`'s `lighting_models`) checks the
//! generators; this checks what a *release* runs. Every shipped preset,
//! under every configuration a host can request — both device profiles,
//! the default and the widest lighting set, with and without each feature
//! channel — is compiled to a plan by `api::pipeline`. Every effect its
//! passes name goes through `api::shader`, and every material stage its
//! geometry passes name goes through `api::material` for every shipped
//! material document and every enabled model. Each result is then
//! validated by naga with no optional capabilities, and translated to
//! GLSL ES 3.00 under the WebGL2 profile. `wxsl-lang` emits WGSL without
//! validating it, so before this gate an invalid generator surfaced only
//! on a device, as a driver's complaint.
//!
//! Only `wxsl_ffi::api` is reached: the same crates a Dawn or C++ host
//! loads (ADR 0049), never `wxsl-render` or `wgpu`.

use serde_json::{json, Value};
use wxsl_core::lighting::{DEFAULT_MODELS, FEATURES};
use wxsl_ffi::api::{self, Response};
use wxsl_ffi::ffi::WxslStatus;

const PRESETS: &[(&str, &str)] = &[
    (
        "forward",
        include_str!("../../wxsl-frame/assets/presets/forward.pipeline.json"),
    ),
    (
        "deferred",
        include_str!("../../wxsl-frame/assets/presets/deferred.pipeline.json"),
    ),
];

/// The shipped material documents.
const MATERIALS: &[(&str, &str)] = &[
    (
        "pbr_cube",
        include_str!("../../wxsl/assets/pbr_cube.wxsl.json"),
    ),
    (
        "bake_term",
        include_str!("../../wxsl/assets/bake_term.wxsl.json"),
    ),
];

/// One configuration a host can request, as the ABI's `config` object.
struct Config {
    label: String,
    models: Vec<&'static str>,
    webgl2: bool,
    json: Value,
}

fn configs() -> Vec<Config> {
    let every_model: Vec<&str> = DEFAULT_MODELS.iter().map(|model| model.name).collect();
    let mut configs = Vec::new();
    for webgl2 in [false, true] {
        let profile = if webgl2 {
            json!({"compute": false, "storage_buffers": false})
        } else {
            json!({"compute": true, "storage_buffers": true})
        };
        for models in [vec!["wxsl.pbr"], every_model.clone()] {
            let feature_sets = std::iter::once(Vec::new())
                .chain(FEATURES.iter().map(|feature| vec![feature.name]));
            for features in feature_sets {
                configs.push(Config {
                    label: format!(
                        "{} [{}] features [{}]",
                        if webgl2 { "webgl2" } else { "full" },
                        models.join(","),
                        features.join(",")
                    ),
                    models: models.clone(),
                    webgl2,
                    json: json!({
                        "profile": profile,
                        "lighting_models": models,
                        "features": features,
                    }),
                });
            }
        }
    }
    configs
}

fn call(operation: fn(&[u8]) -> Response, request: Value) -> Result<(Value, String), String> {
    let response = operation(&serde_json::to_vec(&request).unwrap());
    let json: Value = serde_json::from_slice(&response.json).unwrap();
    if response.status != WxslStatus::Success {
        return Err(json["data"]["message"].to_string());
    }
    Ok((
        json["data"].clone(),
        String::from_utf8(response.wgsl).unwrap(),
    ))
}

/// Every string value under `key`, anywhere in `value`, in first-seen order.
fn strings_under(value: &Value, key: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                match child {
                    Value::String(text) if name == key => {
                        if !found.contains(text) {
                            found.push(text.clone());
                        }
                    }
                    _ => strings_under(child, key, found),
                }
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| strings_under(item, key, found)),
        _ => {}
    }
}

/// Validate `wgsl` with no optional capabilities and, for WebGL2, translate
/// every entry point to GLSL ES 3.00; say what failed.
fn validate(label: &str, wgsl: &str, webgl2: bool) -> Vec<String> {
    // naga translates any depth texture to `sampler2DShadow`, which the
    // browser then refuses to read values from (`glsl_es` in `wxsl`'s tests).
    if let Some(line) = wgsl
        .lines()
        .find(|line| webgl2 && line.contains("@group(3)") && line.contains("texture_depth"))
    {
        return vec![format!("{label}: `{}` reads pass-group depth", line.trim())];
    }
    let module = match naga::front::wgsl::parse_str(wgsl) {
        Ok(module) => module,
        Err(error) => return vec![format!("{label}: {}", error.emit_to_string(wgsl))],
    };
    let info = match naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    {
        Ok(info) => info,
        Err(error) => return vec![format!("{label}: {}", error.emit_to_string(wgsl))],
    };
    if !webgl2 {
        return Vec::new();
    }
    let options = naga::back::glsl::Options {
        version: naga::back::glsl::Version::Embedded {
            version: 300,
            is_webgl: true,
        },
        writer_flags: naga::back::glsl::WriterFlags::ADJUST_COORDINATE_SPACE,
        binding_map: Default::default(),
        zero_initialize_workgroup_memory: true,
    };
    module
        .entry_points
        .iter()
        .filter_map(|entry| {
            let pipeline = naga::back::glsl::PipelineOptions {
                shader_stage: entry.stage,
                entry_point: entry.name.clone(),
                multiview: None,
            };
            let mut glsl = String::new();
            naga::back::glsl::Writer::new(
                &mut glsl,
                &module,
                &info,
                &options,
                &pipeline,
                naga::proc::BoundsCheckPolicies::default(),
            )
            .and_then(|mut writer| writer.write().map(drop))
            .err()
            .map(|error| format!("{label} `{}` (GLSL ES): {error}", entry.name))
        })
        .collect()
}

#[test]
fn every_shipped_shader_compiles_and_validates_under_every_config() {
    let mut failures = Vec::new();
    let mut compiled = 0;
    for config in configs() {
        for (preset, document) in PRESETS {
            let pipeline: Value = serde_json::from_str(document).unwrap();
            let label = format!("{preset} {}", config.label);
            let plan = match call(
                api::pipeline,
                json!({"pipeline": pipeline, "config": config.json}),
            ) {
                Ok((plan, _)) => plan,
                Err(message) => {
                    failures.push(format!("{label}: {message}"));
                    continue;
                }
            };
            let passes = &plan["graph"]["passes"];
            let (mut effects, mut stages) = (Vec::new(), Vec::new());
            strings_under(passes, "effect", &mut effects);
            strings_under(passes, "stage", &mut stages);
            assert!(!stages.is_empty(), "{label}: the plan draws no geometry");

            for effect in &effects {
                let label = format!("{label} effect {effect}");
                match call(
                    api::shader,
                    json!({"effect": effect, "config": config.json}),
                ) {
                    Ok((_, wgsl)) => failures.extend(validate(&label, &wgsl, config.webgl2)),
                    Err(message) => failures.push(format!("{label}: {message}")),
                }
                compiled += 1;
            }
            for (name, material) in MATERIALS {
                let graph: Value = serde_json::from_str(material).unwrap();
                for model in &config.models {
                    for stage in &stages {
                        let label = format!("{label} {name} ({model}) {stage}");
                        match call(
                            api::material,
                            json!({
                                "graph": graph,
                                "stage": stage,
                                "material": {"model": model},
                                "config": config.json,
                            }),
                        ) {
                            Ok((_, wgsl)) => {
                                failures.extend(validate(&label, &wgsl, config.webgl2))
                            }
                            Err(message) => failures.push(format!("{label}: {message}")),
                        }
                        compiled += 1;
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {compiled} shaders failed the release gate:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    eprintln!("release gate: {compiled} shaders compiled and validated");
}
