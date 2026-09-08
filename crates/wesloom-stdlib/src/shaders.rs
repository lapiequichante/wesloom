//! The WESL sources of this library, embedded and addressed by module path.
//!
//! Every `.wesl` file under `shaders/` is compiled into the binary with
//! `include_str!` and listed in [`MODULES`], keyed by the WESL module path it
//! is imported as. Embedding rather than reading from disk keeps a shipped
//! application from needing the shader tree next to its executable, and keeps
//! the mapping from file to module path in one reviewable table instead of
//! implicit in a directory walk.
//!
//! `wesloom-render` never sees this crate (the dependency arrow only points
//! into `wesloom-core`, see
//! [ADR 0002](../../../docs/adr/0002-cargo-workspace-crate-boundaries.md)), so
//! the application hands these modules to the renderer's shader library —
//! `wesloom::stdlib_library()` does exactly that when both features are on.

/// Build the module table, checking at compile time that every named file
/// exists.
macro_rules! modules {
    ($($path:literal => $file:literal,)*) => {
        /// Every WESL module in this library, as `(module path, source)`.
        pub const MODULES: &[(&str, &str)] = &[
            $(($path, include_str!(concat!("../shaders/", $file))),)*
        ];
    };
}

modules! {
    // The shader ABI: the vocabulary a generated material module is written
    // against, plus the two render paths' plumbing. See `wesloom_core::abi`.
    "package::wesloom::bindings" => "wesloom/bindings.wesl",
    "package::wesloom::surface" => "wesloom/surface.wesl",
    "package::wesloom::vertex" => "wesloom/vertex.wesl",
    "package::wesloom::shading" => "wesloom/shading.wesl",
    "package::wesloom::deferred" => "wesloom/deferred.wesl",
    "package::wesloom::lighting_pass" => "wesloom/lighting_pass.wesl",

    // Granular functions, one per file, grouped by category.
    "package::animation::ease_in_out_cubic" => "animation/ease_in_out_cubic.wesl",
    "package::animation::pulse" => "animation/pulse.wesl",

    "package::color::hsv_to_rgb" => "color/hsv_to_rgb.wesl",
    "package::color::linear_to_srgb" => "color/linear_to_srgb.wesl",
    "package::color::luminance" => "color/luminance.wesl",
    "package::color::rgb_to_hsv" => "color/rgb_to_hsv.wesl",
    "package::color::srgb_to_linear" => "color/srgb_to_linear.wesl",
    "package::color::tonemap_filmic" => "color/tonemap_filmic.wesl",
    "package::color::tonemap_reinhard" => "color/tonemap_reinhard.wesl",

    "package::distort::swirl_uv" => "distort/swirl_uv.wesl",

    "package::generative::fbm3" => "generative/fbm3.wesl",
    "package::generative::hash13" => "generative/hash13.wesl",
    "package::generative::value_noise3" => "generative/value_noise3.wesl",

    "package::lighting::ambient_environment" => "lighting/ambient_environment.wesl",
    "package::lighting::diffuse_lambert" => "lighting/diffuse_lambert.wesl",
    "package::lighting::distribution_ggx" => "lighting/distribution_ggx.wesl",
    "package::lighting::fresnel_schlick" => "lighting/fresnel_schlick.wesl",
    "package::lighting::pbr_direct" => "lighting/pbr_direct.wesl",
    "package::lighting::pbr_direct_split" => "lighting/pbr_direct_split.wesl",
    "package::lighting::visibility_smith" => "lighting/visibility_smith.wesl",

    "package::math::inverse_lerp" => "math/inverse_lerp.wesl",
    "package::math::remap" => "math/remap.wesl",
    "package::math::safe_normalize" => "math/safe_normalize.wesl",
    "package::math::smootherstep" => "math/smootherstep.wesl",
    "package::math::wrap" => "math/wrap.wesl",

    "package::sdf::box" => "sdf/box.wesl",
    "package::sdf::smooth_union" => "sdf/smooth_union.wesl",
    "package::sdf::sphere" => "sdf/sphere.wesl",

    "package::space::apply_normal_map" => "space/apply_normal_map.wesl",
    "package::space::rotate_uv" => "space/rotate_uv.wesl",
    "package::space::tangent_basis" => "space/tangent_basis.wesl",
}

/// The source of one module, by path.
pub fn module(path: &str) -> Option<&'static str> {
    MODULES
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, source)| *source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn module_paths_are_unique_and_non_empty() {
        let mut seen = BTreeSet::new();
        for (path, source) in MODULES {
            assert!(seen.insert(*path), "duplicate module path `{path}`");
            assert!(!source.trim().is_empty(), "`{path}` is empty");
        }
        assert_eq!(seen.len(), MODULES.len());
    }

    #[test]
    fn every_shader_file_is_listed() {
        // A file nobody lists is a file nobody can import: catch it here
        // rather than as a missing-module error at shader compile time.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/shaders");
        let mut found = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("shaders/ is readable") {
                let path = entry.expect("readable entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "wesl") {
                    let relative = path
                        .strip_prefix(root)
                        .expect("under shaders/")
                        .to_string_lossy()
                        .replace('\\', "/");
                    found.push(relative);
                }
            }
        }
        found.sort();

        let listed: BTreeSet<String> = MODULES
            .iter()
            .map(|(path, _)| path.trim_start_matches("package::").replace("::", "/") + ".wesl")
            .collect();
        let missing: Vec<&String> = found.iter().filter(|f| !listed.contains(*f)).collect();
        assert!(missing.is_empty(), "unlisted shader files: {missing:?}");
    }

    #[test]
    fn module_lookup_works() {
        assert!(module("package::math::remap").is_some());
        assert!(module("package::math::nonexistent").is_none());
    }
}
