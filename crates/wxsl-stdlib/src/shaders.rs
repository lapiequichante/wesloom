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
macro_rules! module_source {
    ("wxsl/bindings.wxsl") => {
        include_str!(concat!(env!("OUT_DIR"), "/bindings.wxsl"))
    };
    ("wxsl/vertex.wxsl") => {
        include_str!(concat!(env!("OUT_DIR"), "/vertex.wxsl"))
    };
    ("wxsl/ui.wxsl") => {
        include_str!(concat!(env!("OUT_DIR"), "/ui.wxsl"))
    };
    ("wxsl/msdf.wxsl") => {
        include_str!(concat!(env!("OUT_DIR"), "/msdf.wxsl"))
    };
    ($file:tt) => {
        include_str!(concat!("../shaders/", $file))
    };
}

macro_rules! modules {
    ($($path:literal => $file:tt,)*) => {
        /// Every WXSL module in this library, as `(module path, source)`.
        pub const MODULES: &[(&str, &str)] = &[
            $(($path, module_source!($file)),)*
        ];
    };
}

modules! {
    // The shader ABI: the vocabulary a generated material module is written
    // against, plus the two render paths' plumbing. See `wxsl_core::abi`.
    "package::wxsl::bindings" => "wxsl/bindings.wxsl",
    "package::wxsl::surface" => "wxsl/surface.wxsl",
    "package::wxsl::vertex" => "wxsl/vertex.wxsl",
    "package::wxsl::velocity" => "wxsl/velocity.wxsl",
    "package::wxsl::screen" => "wxsl/screen.wxsl",
    "package::wxsl::shadow" => "wxsl/shadow.wxsl",
    // The shading function and the lighting pass are *generated* now, from
    // the enabled lighting models (wxsl_core::lighting, ADR 0028), so
    // neither ships as a file. The model functions the generated shaders
    // call do.
    // The 2D UI pass the editor draws itself with (ADR 0013). Part of the
    // ABI because `wxsl_core::abi` names its entry points, bindings and
    // vertex layout, and because `wxsl-render` ships no shaders (ADR 0009).
    "package::wxsl::ui" => "wxsl/ui.wxsl",
    // Glyph distance-field generation as a compute pass (ADR 0014), the GPU
    // half of `wxsl_render::ui::msdf`.
    "package::wxsl::msdf" => "wxsl/msdf.wxsl",
    // Material features: the second source of G-buffer channels (plan2
    // P12). A feature's module declares its macro knob and its pack, and
    // travels with the ABI modules because `wxsl_core::lighting::FEATURES`
    // names its path.
    "package::wxsl::features::subsurface" => "wxsl/features/subsurface.wxsl",

    // Granular functions, one per file, grouped by category.
    "package::animation::ease_in_out_cubic" => "animation/ease_in_out_cubic.wxsl",
    "package::animation::pulse" => "animation/pulse.wxsl",

    "package::color::bloom_threshold" => "color/bloom_threshold.wxsl",
    "package::color::hsv_to_rgb" => "color/hsv_to_rgb.wxsl",
    "package::color::linear_to_oklab" => "color/linear_to_oklab.wxsl",
    "package::color::linear_to_srgb" => "color/linear_to_srgb.wxsl",
    "package::color::luminance" => "color/luminance.wxsl",
    "package::color::oklab_to_linear" => "color/oklab_to_linear.wxsl",
    "package::color::oklab_to_oklch" => "color/oklab_to_oklch.wxsl",
    "package::color::oklch_to_oklab" => "color/oklch_to_oklab.wxsl",
    "package::color::rgb_to_hsv" => "color/rgb_to_hsv.wxsl",
    "package::color::srgb_to_linear" => "color/srgb_to_linear.wxsl",
    "package::color::tonemap_aces" => "color/tonemap_aces.wxsl",
    "package::color::tonemap_filmic" => "color/tonemap_filmic.wxsl",
    "package::color::tonemap_reinhard" => "color/tonemap_reinhard.wxsl",
    "package::color::tonemap_reinhard_jodie" => "color/tonemap_reinhard_jodie.wxsl",

    "package::distort::swirl_uv" => "distort/swirl_uv.wxsl",

    "package::filter::blur_box" => "filter/blur_box.wxsl",
    "package::filter::blur_gaussian" => "filter/blur_gaussian.wxsl",
    "package::filter::blur_kawase" => "filter/blur_kawase.wxsl",
    "package::filter::chromatic_aberration" => "filter/chromatic_aberration.wxsl",
    "package::filter::circle_of_confusion" => "filter/circle_of_confusion.wxsl",
    "package::filter::downsample2" => "filter/downsample2.wxsl",
    "package::filter::disk_sample" => "filter/disk_sample.wxsl",
    "package::filter::fxaa" => "filter/fxaa.wxsl",
    "package::filter::film_grain" => "filter/film_grain.wxsl",
    "package::filter::linearize_depth" => "filter/linearize_depth.wxsl",
    "package::filter::vignette" => "filter/vignette.wxsl",

    "package::generative::brick_mask" => "generative/brick_mask.wxsl",
    "package::generative::checker" => "generative/checker.wxsl",
    "package::generative::checker_aa" => "generative/checker_aa.wxsl",
    "package::generative::fbm3" => "generative/fbm3.wxsl",
    "package::generative::grid_mask" => "generative/grid_mask.wxsl",
    "package::generative::grid_mask_aa" => "generative/grid_mask_aa.wxsl",
    "package::generative::hash13" => "generative/hash13.wxsl",
    "package::generative::simplex2" => "generative/simplex2.wxsl",
    "package::generative::simplex3" => "generative/simplex3.wxsl",
    "package::generative::truchet_arcs" => "generative/truchet_arcs.wxsl",
    "package::generative::value_noise3" => "generative/value_noise3.wxsl",
    "package::generative::worley3" => "generative/worley3.wxsl",

    "package::lighting::ambient_environment" => "lighting/ambient_environment.wxsl",
    "package::lighting::anisotropic_widths" => "lighting/anisotropic_widths.wxsl",
    "package::lighting::charlie_distribution" => "lighting/charlie_distribution.wxsl",
    "package::lighting::diffuse_lambert" => "lighting/diffuse_lambert.wxsl",
    "package::lighting::distribution_ggx" => "lighting/distribution_ggx.wxsl",
    "package::lighting::distribution_ggx_anisotropic" => "lighting/distribution_ggx_anisotropic.wxsl",
    "package::lighting::f0_to_ior" => "lighting/f0_to_ior.wxsl",
    "package::lighting::fog_composite" => "lighting/fog_composite.wxsl",
    "package::lighting::fog_transmittance" => "lighting/fog_transmittance.wxsl",
    "package::lighting::fresnel_schlick" => "lighting/fresnel_schlick.wxsl",
    "package::lighting::height_fog_depth" => "lighting/height_fog_depth.wxsl",
    "package::lighting::hg_phase" => "lighting/hg_phase.wxsl",
    "package::lighting::ior_to_f0" => "lighting/ior_to_f0.wxsl",
    "package::lighting::iridescence" => "lighting/iridescence.wxsl",
    "package::lighting::pbr_direct" => "lighting/pbr_direct.wxsl",
    "package::lighting::pbr_direct_split" => "lighting/pbr_direct_split.wxsl",
    "package::lighting::rayleigh_phase" => "lighting/rayleigh_phase.wxsl",
    "package::lighting::roughness_aa" => "lighting/roughness_aa.wxsl",
    "package::lighting::sheen_ibl_response" => "lighting/sheen_ibl_response.wxsl",
    "package::lighting::sky_single_scattering" => "lighting/sky_single_scattering.wxsl",
    "package::lighting::thin_film_phase" => "lighting/thin_film_phase.wxsl",
    "package::lighting::visibility_ggx_anisotropic" => "lighting/visibility_ggx_anisotropic.wxsl",
    "package::lighting::visibility_neubelt" => "lighting/visibility_neubelt.wxsl",
    "package::lighting::visibility_smith" => "lighting/visibility_smith.wxsl",

    // The lighting models (`wxsl_core::lighting::DEFAULT_MODELS`). Kept out
    // of the node derivation on purpose: they are shaded through the
    // registry's contract, not placed on a canvas.
    "package::lighting::models::lambert" => "lighting/models/lambert.wxsl",
    "package::lighting::models::phong" => "lighting/models/phong.wxsl",
    "package::lighting::models::pbr" => "lighting/models/pbr.wxsl",
    "package::lighting::models::clearcoat" => "lighting/models/clearcoat.wxsl",
    "package::lighting::models::cloth" => "lighting/models/cloth.wxsl",
    "package::lighting::models::preshaded" => "lighting/models/preshaded.wxsl",
    "package::lighting::models::iridescent" => "lighting/models/iridescent.wxsl",
    "package::lighting::models::sheen" => "lighting/models/sheen.wxsl",

    "package::math::ray_sphere" => "math/ray_sphere.wxsl",
    "package::math::safe_normalize" => "math/safe_normalize.wxsl",
    "package::math::screen_width" => "math/screen_width.wxsl",
    "package::math::smootherstep" => "math/smootherstep.wxsl",

    "package::sample::load_2d" => "sample/load_2d.wxsl",
    "package::sample::texture_2d" => "sample/texture_2d.wxsl",

    "package::sdf::box" => "sdf/box.wxsl",
    "package::sdf::capsule" => "sdf/capsule.wxsl",
    "package::sdf::chamfer_union" => "sdf/chamfer_union.wxsl",
    "package::sdf::circle" => "sdf/circle.wxsl",
    "package::sdf::coverage" => "sdf/coverage.wxsl",
    "package::sdf::cylinder" => "sdf/cylinder.wxsl",
    "package::sdf::equilateral_triangle" => "sdf/equilateral_triangle.wxsl",
    "package::sdf::hexagon" => "sdf/hexagon.wxsl",
    "package::sdf::intersection" => "sdf/intersection.wxsl",
    "package::sdf::onion" => "sdf/onion.wxsl",
    "package::sdf::round" => "sdf/round.wxsl",
    "package::sdf::rounded_box" => "sdf/rounded_box.wxsl",
    "package::sdf::smooth_intersection" => "sdf/smooth_intersection.wxsl",
    "package::sdf::smooth_union" => "sdf/smooth_union.wxsl",
    "package::sdf::sphere" => "sdf/sphere.wxsl",
    "package::sdf::subtract" => "sdf/subtract.wxsl",
    "package::sdf::torus" => "sdf/torus.wxsl",
    "package::sdf::union" => "sdf/union.wxsl",

    "package::space::apply_normal_map" => "space/apply_normal_map.wxsl",
    "package::space::direction_to_equirect" => "space/direction_to_equirect.wxsl",
    "package::space::equirect_to_direction" => "space/equirect_to_direction.wxsl",
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
        assert_eq!(
            frame,
            vec![abi::GROUP_FRAME; 8],
            "camera, scene, object, the previous-frame object, and the \
             environment-BRDF table, its sampler and two environment cubes (ADR 0063)"
        );

        // The lighting pass is generated now (wxsl_core::lighting), so its
        // bindings are checked against the generated source in
        // `tests/lighting_models.rs` — one per G-buffer target plus depth,
        // all in the pass group.
    }

    #[test]
    fn the_frame_group_declares_the_bindings_the_abi_numbers() {
        let source = module("package::wxsl::bindings").expect("bindings module");
        for (binding, declaration) in [
            (abi::BINDING_CAMERA, "var<uniform> camera"),
            (abi::BINDING_SCENE, "var<uniform> scene"),
            (abi::BINDING_ENVIRONMENT_DIFFUSE, "var environment_diffuse"),
            (
                abi::BINDING_ENVIRONMENT_SPECULAR,
                "var environment_specular",
            ),
            // A storage buffer, not a uniform: one binding serves every
            // draw in the frame, indexed by `@builtin(instance_index)`.
            (abi::BINDING_INSTANCES, "var<storage, read> instances"),
            // The same rows last frame, beside them — the velocity
            // stage's other half (plan3 N2).
            (
                abi::BINDING_PREVIOUS_INSTANCES,
                "var<storage, read> previous_instances",
            ),
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
