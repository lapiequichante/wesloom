//! The WXSL sources of this library, embedded and addressed by module path.
//!
//! Every `.wxsl` file under `shaders/` is compiled into the binary with
//! `include_str!` and listed in [`MODULES`], keyed by the WXSL module path it
//! is imported as. Embedding rather than reading from disk keeps a shipped
//! application from needing the shader tree next to its executable, and keeps
//! the mapping from file to module path in one reviewable table instead of
//! implicit in a directory walk.
//!
//! `wxsl-render` never sees this crate (the dependency arrow only points
//! into `wxsl-core`, see
//! [ADR 0002](../../../docs/adr/0002-cargo-workspace-crate-boundaries.md)), so
//! the application hands these modules to the renderer's shader library —
//! `wxsl::stdlib_library()` does exactly that when both features are on.

/// Build the module table, checking at compile time that every named file
/// exists.
macro_rules! modules {
    ($($path:literal => $file:literal,)*) => {
        /// Every WXSL module in this library, as `(module path, source)`.
        pub const MODULES: &[(&str, &str)] = &[
            $(($path, include_str!(concat!("../shaders/", $file))),)*
        ];
    };
}

modules! {
    // The shader ABI: the vocabulary a generated material module is written
    // against, plus the two render paths' plumbing. See `wxsl_core::abi`.
    "package::wxsl::bindings" => "wxsl/bindings.wxsl",
    "package::wxsl::surface" => "wxsl/surface.wxsl",
    "package::wxsl::vertex" => "wxsl/vertex.wxsl",
    "package::wxsl::shading" => "wxsl/shading.wxsl",
    "package::wxsl::deferred" => "wxsl/deferred.wxsl",
    "package::wxsl::lighting_pass" => "wxsl/lighting_pass.wxsl",
    // The 2D UI pass the editor draws itself with (ADR 0013). Part of the
    // ABI because `wxsl_core::abi` names its entry points, bindings and
    // vertex layout, and because `wxsl-render` ships no shaders (ADR 0009).
    "package::wxsl::ui" => "wxsl/ui.wxsl",
    // Glyph distance-field generation as a compute pass (ADR 0014), the GPU
    // half of `wxsl_render::ui::msdf`.
    "package::wxsl::msdf" => "wxsl/msdf.wxsl",

    // Granular functions, one per file, grouped by category.
    "package::animation::ease_in_out_cubic" => "animation/ease_in_out_cubic.wxsl",
    "package::animation::pulse" => "animation/pulse.wxsl",

    "package::color::hsv_to_rgb" => "color/hsv_to_rgb.wxsl",
    "package::color::linear_to_srgb" => "color/linear_to_srgb.wxsl",
    "package::color::luminance" => "color/luminance.wxsl",
    "package::color::rgb_to_hsv" => "color/rgb_to_hsv.wxsl",
    "package::color::srgb_to_linear" => "color/srgb_to_linear.wxsl",
    "package::color::tonemap_filmic" => "color/tonemap_filmic.wxsl",
    "package::color::tonemap_reinhard" => "color/tonemap_reinhard.wxsl",

    "package::distort::swirl_uv" => "distort/swirl_uv.wxsl",

    "package::generative::fbm3" => "generative/fbm3.wxsl",
    "package::generative::hash13" => "generative/hash13.wxsl",
    "package::generative::value_noise3" => "generative/value_noise3.wxsl",

    "package::lighting::ambient_environment" => "lighting/ambient_environment.wxsl",
    "package::lighting::diffuse_lambert" => "lighting/diffuse_lambert.wxsl",
    "package::lighting::distribution_ggx" => "lighting/distribution_ggx.wxsl",
    "package::lighting::fresnel_schlick" => "lighting/fresnel_schlick.wxsl",
    "package::lighting::pbr_direct" => "lighting/pbr_direct.wxsl",
    "package::lighting::pbr_direct_split" => "lighting/pbr_direct_split.wxsl",
    "package::lighting::visibility_smith" => "lighting/visibility_smith.wxsl",

    "package::math::safe_normalize" => "math/safe_normalize.wxsl",
    "package::math::smootherstep" => "math/smootherstep.wxsl",

    "package::sample::texture_2d" => "sample/texture_2d.wxsl",

    "package::sdf::box" => "sdf/box.wxsl",
    "package::sdf::smooth_union" => "sdf/smooth_union.wxsl",
    "package::sdf::sphere" => "sdf/sphere.wxsl",

    "package::space::apply_normal_map" => "space/apply_normal_map.wxsl",
    "package::space::rotate_uv" => "space/rotate_uv.wxsl",
    "package::space::tangent_basis" => "space/tangent_basis.wxsl",
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
    use wxsl_core::abi;

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
                } else if path.extension().is_some_and(|e| e == "wxsl") {
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
            .map(|(path, _)| path.trim_start_matches("package::").replace("::", "/") + ".wxsl")
            .collect();
        let missing: Vec<&String> = found.iter().filter(|f| !listed.contains(*f)).collect();
        assert!(missing.is_empty(), "unlisted shader files: {missing:?}");
    }

    /// The `name: type` members of `struct` in `module`, in order, with
    /// attributes and comments stripped.
    fn struct_fields(module: &str, name: &str) -> Vec<(String, String)> {
        let source = MODULES
            .iter()
            .find(|(path, _)| *path == module)
            .map(|(_, source)| *source)
            .unwrap_or_else(|| panic!("`{module}` is in the table"));
        let start = source
            .find(&format!("struct {name} {{"))
            .unwrap_or_else(|| panic!("`{module}` declares `struct {name}`"));
        let body = &source[start..];
        let end = body.find('}').expect("the struct is closed");
        body[..end]
            .lines()
            .skip(1)
            .filter_map(|line| {
                let line = line.split("//").next().unwrap_or("").trim();
                // Attributes come first and are not part of the layout
                // agreement; the location *order* is, and that is the
                // order of the lines.
                let line = line.rsplit('>').next().unwrap_or(line);
                let line = match line.rfind(')') {
                    Some(at) => &line[at + 1..],
                    None => line,
                };
                let (field, ty) = line.trim().trim_end_matches(',').split_once(':')?;
                Some((field.trim().to_string(), ty.trim().to_string()))
            })
            .collect()
    }

    #[test]
    fn the_shipped_vertex_structs_are_what_the_abi_tables_say() {
        // Three views of one layout: these tables, the `.wxsl` below, and
        // `wxsl_render::mesh`. Codegen writes the *extended* IO structs
        // from the tables (ADR 0024), so a `.wxsl` that drifted would put
        // a material's declared attribute at a location the base half had
        // already taken — and nothing else would notice.
        let vertex_in = struct_fields(abi::VERTEX_MODULE, abi::VERTEX_IN_STRUCT);
        for (index, field) in abi::VERTEX_IN_FIELDS.iter().enumerate() {
            assert_eq!(
                vertex_in
                    .get(index)
                    .map(|(name, ty)| (name.as_str(), ty.as_str())),
                Some((field.name, field.ty.wxsl_type())),
                "`{}` field {index} is not `{}`",
                abi::VERTEX_IN_STRUCT,
                field.name,
            );
        }
        // One more than the table: `@builtin(instance_index)`, which has
        // no location and so is not one.
        assert_eq!(vertex_in.len(), abi::VERTEX_IN_FIELDS.len() + 1);

        let vertex_out = struct_fields(abi::VERTEX_MODULE, abi::VERTEX_OUT_STRUCT);
        assert_eq!(
            vertex_out.first().map(|(name, _)| name.as_str()),
            Some(abi::CLIP_POSITION_FIELD),
            "`{}` starts with the clip position",
            abi::VERTEX_OUT_STRUCT,
        );
        let located: Vec<(String, String)> = vertex_out.into_iter().skip(1).collect();
        assert_eq!(located.len(), abi::VERTEX_OUT_FIELDS.len());
        for (index, field) in abi::VERTEX_OUT_FIELDS.iter().enumerate() {
            assert_eq!(
                (located[index].0.as_str(), located[index].1.as_str()),
                (field.name, field.ty.wxsl_type()),
                "`{}` location {index} is not `{}`",
                abi::VERTEX_OUT_STRUCT,
                field.name,
            );
        }
    }

    #[test]
    fn the_shipped_instance_row_starts_with_the_abi_prefix() {
        // The vertex stage reads the row through this narrow struct while
        // a material's generated module may read a wider one at the same
        // binding. The two only agree because the prefix is pinned, so
        // this is the test that keeps a declared attribute from sliding
        // in front of the model matrix.
        let fields = struct_fields("package::wxsl::bindings", "Instance");
        assert_eq!(fields.len(), abi::INSTANCE_BASE_FIELDS.len());
        for (index, field) in abi::INSTANCE_BASE_FIELDS.iter().enumerate() {
            assert_eq!(
                (fields[index].0.as_str(), fields[index].1.as_str()),
                (field.name, field.ty.wxsl_type()),
            );
        }
    }

    /// Every `@group(N)` a module declares, as `(module path, N)`.
    fn declared_groups() -> Vec<(&'static str, u32)> {
        let mut found = Vec::new();
        for (path, source) in MODULES {
            let mut rest = *source;
            while let Some(at) = rest.find("@group(") {
                rest = &rest[at + "@group(".len()..];
                let end = rest.find(')').expect("@group( is closed");
                let index: u32 = rest[..end]
                    .trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("`{path}` has a non-numeric @group"));
                found.push((*path, index));
                rest = &rest[end..];
            }
        }
        found
    }

    #[test]
    fn shipped_shaders_bind_the_groups_the_abi_names() {
        // Group indices are part of the ABI (ADR 0010) and are written twice:
        // as constants in `wxsl_core::abi`, and by hand in the `.wxsl`
        // below. This is the test that keeps the two from drifting.
        let groups = declared_groups();
        assert!(!groups.is_empty(), "no @group declarations found at all");

        for (path, index) in &groups {
            assert!(
                abi::BIND_GROUPS.iter().any(|slot| slot.index == *index),
                "`{path}` binds @group({index}), which is not one of the four slots"
            );
            let slot = abi::BIND_GROUPS
                .iter()
                .find(|slot| slot.index == *index)
                .expect("checked above");
            assert!(
                !slot.application_owned,
                "`{path}` binds @group({index}), the application's own slot"
            );
        }

        let frame: Vec<u32> = groups
            .iter()
            .filter(|(path, _)| *path == "package::wxsl::bindings")
            .map(|(_, index)| *index)
            .collect();
        assert_eq!(frame, vec![abi::GROUP_FRAME; 3], "camera, scene, object");

        let pass: Vec<u32> = groups
            .iter()
            .filter(|(path, _)| *path == abi::LIGHTING_PASS_MODULE)
            .map(|(_, index)| *index)
            .collect();
        assert_eq!(
            pass,
            vec![abi::GROUP_PASS; abi::GBUFFER_TARGETS.len() + 1],
            "one binding per G-buffer target, plus depth"
        );
    }

    #[test]
    fn the_frame_group_declares_the_bindings_the_abi_numbers() {
        let source = module("package::wxsl::bindings").expect("bindings module");
        for (binding, declaration) in [
            (abi::BINDING_CAMERA, "var<uniform> camera"),
            (abi::BINDING_SCENE, "var<uniform> scene"),
            // A storage buffer, not a uniform: one binding serves every
            // draw in the frame, indexed by `@builtin(instance_index)`.
            (abi::BINDING_INSTANCES, "var<storage, read> instances"),
        ] {
            let declaration = format!(
                "@group({}) @binding({binding}) {declaration}",
                abi::GROUP_FRAME
            );
            assert!(
                source.contains(&declaration),
                "bindings.wxsl is missing `{declaration}`"
            );
        }
    }

    #[test]
    fn the_ui_pass_declares_the_bindings_the_abi_numbers() {
        let source = module(abi::UI_MODULE).expect("ui module");
        for (binding, declaration) in [
            (abi::BINDING_UI_VIEWPORT, "var<uniform> ui_viewport"),
            (abi::BINDING_UI_TEXTURE, "var ui_texture"),
            (abi::BINDING_UI_SAMPLER, "var ui_sampler"),
        ] {
            let expected = format!(
                "@group({}) @binding({binding}) {declaration}",
                abi::GROUP_PASS
            );
            assert!(
                source.contains(&expected),
                "ui.wxsl is missing `{expected}`"
            );
        }
        for entry in [abi::UI_VERTEX_ENTRY, abi::UI_FRAGMENT_ENTRY] {
            assert!(
                source.contains(&format!("fn {entry}(")),
                "ui.wxsl is missing the `{entry}` entry point"
            );
        }
    }

    #[test]
    fn the_ui_shader_agrees_with_the_abi_on_kinds_and_attributes() {
        // Three views of one layout (ADR 0013): this file, the ABI tables,
        // and `wxsl_render::ui::draw::UiVertex`. Two of them can be checked
        // against each other here; the third is checked in `wxsl-render`.
        let source = module(abi::UI_MODULE).expect("ui module");
        for kind in abi::UI_KINDS {
            let declaration = format!("const {}: u32 = {}u;", kind.name, kind.value);
            assert!(
                source.contains(&declaration),
                "ui.wxsl is missing `{declaration}`"
            );
        }
        for (location, attribute) in abi::UI_ATTRIBUTES.iter().enumerate() {
            let declaration = format!("@location({location}) {}: {}", attribute.name, attribute.ty);
            assert!(
                source.contains(&declaration),
                "ui.wxsl is missing `{declaration}`"
            );
        }
    }

    #[test]
    fn module_lookup_works() {
        assert!(module("package::math::safe_normalize").is_some());
        assert!(module("package::math::nonexistent").is_none());
    }
}
