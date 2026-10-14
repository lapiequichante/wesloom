//! glTF import, drawn (ADR 0080): a GLB built in the test, rendered through
//! both stock pipelines, and the Khronos sample models in `resources/glb/`
//! compiled material by material. The import itself is tested without a
//! device in `wxsl-gltf`.

#[path = "../../wxsl-gltf/tests/support/mod.rs"]
mod support;

use std::path::Path;

use serde_json::json;
use support::Glb;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::Material;
use wxsl::render::{Light, RenderRequest, Renderer, StockPipeline, TargetConfig};
use wxsl::scene::SceneResources;

const SIZE: u32 = 64;

fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(context) => Some(context),
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

#[test]
fn every_sample_model_material_compiles_under_its_lighting_set() {
    let registry = wxsl::stdlib::registry();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/glb");
    for file in [
        "DamagedHelmet/DamagedHelmet.glb",
        "CarConcept/CarConcept.glb",
        "GlamVelvetSofa/GlamVelvetSofa.glb",
    ] {
        let path = root.join(file);
        if !path.exists() {
            eprintln!("skipping {file}: not downloaded");
            continue;
        }
        let imported = wxsl::asset::import_gltf(&path).expect("the sample imports");
        assert!(imported.scene.validate().is_empty(), "{file}");
        let lighting = imported.lighting().expect("a lighting set");
        for material in &imported.scene.materials {
            Material::with_lighting(&material.graph, &registry, &material.config, &lighting)
                .unwrap_or_else(|error| panic!("{file}: `{}`: {error}", material.name));
        }
    }
}

/// A red-textured triangle filling the view, from a GLB with a camera.
fn red_glb() -> Vec<u8> {
    let mut glb = Glb::default();
    let red = glb.png([255, 0, 0, 255]);
    let texture = glb.texture(red);
    let material = glb.material(json!({
        "name": "red",
        "pbrMetallicRoughness": { "baseColorTexture": { "index": texture }, "metallicFactor": 0.0 }
    }));
    let mesh = glb.triangle(Some(material), false);
    glb.node(json!({ "mesh": mesh, "scale": [3.0, 3.0, 1.0] }), None);
    glb.json["cameras"] =
        json!([{ "type": "perspective", "perspective": { "yfov": 0.8, "znear": 0.1 } }]);
    glb.node(json!({ "camera": 0, "translation": [0.0, 0.0, 2.0] }), None);
    glb.finish()
}

#[test]
fn an_imported_glb_draws_its_texture_on_both_pipelines() {
    let Some(gpu) = gpu() else { return };
    let imported = wxsl::asset::import_gltf_slice(&red_glb()).expect("imports");
    let registry = wxsl::stdlib::registry();
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
        let mut renderer = Renderer::new(
            &gpu.device,
            wxsl::stdlib_library(),
            TargetConfig::new(SIZE, SIZE, target.format()),
        )
        .expect("a renderer");
        renderer.set_pipeline(pipeline);
        renderer
            .set_lighting(imported.lighting().expect("a set"))
            .expect("the set is shipped");
        let mut resources = SceneResources::load_imported(
            &gpu.device,
            &imported,
            &registry,
            renderer.lighting(),
            &[],
        )
        .expect("the import loads");
        resources
            .create_bindings(&gpu.device, &gpu.queue, &mut renderer)
            .expect("the import binds what it declares");
        resources
            .upload(&gpu.device, &gpu.queue)
            .expect("every slot is bound");

        let mut environment =
            wxsl::scene::environment(&imported.scene, 1.0).expect("the file has a camera");
        assert_eq!(
            environment.camera.eye,
            wxsl::render::glam::Vec3::new(0.0, 0.0, 2.0)
        );
        environment.lights = vec![Light::directional(
            wxsl::render::glam::Vec3::Z,
            wxsl::render::glam::Vec3::ONE,
            3.0,
        )];
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: &environment,
                    draws: &resources.draw_list(),
                },
            )
            .expect("renders");
        gpu.wait();
        let image = target.read_rgba8(&gpu.device, &gpu.queue);
        let centre = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
        let [r, g, b] = [image[centre], image[centre + 1], image[centre + 2]];
        assert!(
            r > 40 && r > g * 2 && r > b * 2,
            "{pipeline:?}: centre is {r},{g},{b}"
        );
    }
}
