//! The GLSL ES 3.00 column of the corpus gate (ADR 0075): WGSL in, through
//! the translator wgpu's GL backend runs, one entry point at a time.
//!
//! Not a test binary — Cargo compiles only top-level files in `tests/` as
//! those — but the check `graph_to_wgsl.rs` and `downlevel_corpus.rs` share.
//! WebGL2 is GLSL ES 3.00, and desktop GL 3.3 is a superset of what it can
//! say, so a shader that translates here runs on both.
#![allow(dead_code)]

use wxsl::core::macros::MacroSet;
use wxsl::render::Profile;

/// The macros a WebGL2 renderer pins over everything it compiles.
pub fn webgl2_macros() -> MacroSet {
    Profile::WEBGL2.macros()
}

/// Translate every entry point of `wgsl` to GLSL ES 3.00, as WebGL2 needs
/// it, and say what failed — empty when everything translated.
pub fn translate(label: &str, wgsl: &str) -> Vec<String> {
    // naga translates a depth texture to a `sampler2DShadow` whatever reads
    // it, so a pass reading depth *values* translates cleanly and then fails
    // in the browser's GLSL compiler. The pass group reads depth as
    // `texture_2d<f32>`; only the frame's shadow atlas is a depth texture.
    if let Some(line) = wgsl
        .lines()
        .find(|line| line.contains("@group(3)") && line.contains("texture_depth"))
    {
        return vec![format!(
            "{label}: `{}` — read pass-group depth as texture_2d<f32> (ADR 0075)",
            line.trim()
        )];
    }
    let module = match naga::front::wgsl::parse_str(wgsl) {
        Ok(module) => module,
        Err(error) => return vec![format!("{label}: {}", error.emit_to_string(wgsl))],
    };
    // No optional capabilities: a GLES 3.0 device has none of them.
    let info = match naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    {
        Ok(info) => info,
        Err(error) => return vec![format!("{label}: {}", error.emit_to_string(wgsl))],
    };
    let options = naga::back::glsl::Options {
        version: naga::back::glsl::Version::Embedded {
            version: 300,
            is_webgl: true,
        },
        writer_flags: naga::back::glsl::WriterFlags::ADJUST_COORDINATE_SPACE,
        binding_map: Default::default(),
        zero_initialize_workgroup_memory: true,
    };
    let mut failures = Vec::new();
    for entry in &module.entry_points {
        let pipeline = naga::back::glsl::PipelineOptions {
            shader_stage: entry.stage,
            entry_point: entry.name.clone(),
            multiview: None,
        };
        let mut glsl = String::new();
        let written = naga::back::glsl::Writer::new(
            &mut glsl,
            &module,
            &info,
            &options,
            &pipeline,
            naga::proc::BoundsCheckPolicies::default(),
        )
        .and_then(|mut writer| writer.write().map(drop));
        if let Err(error) = written {
            failures.push(format!("{label} `{}`: {error}", entry.name));
        }
    }
    failures
}
