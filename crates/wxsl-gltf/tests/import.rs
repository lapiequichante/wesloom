//! glTF import, device-free: each mapping on a GLB built in the test
//! (ADR 0080). The GPU side is `crates/wxsl/tests/gltf_import.rs`.

mod support;

use serde_json::json;
use support::Glb;
use wxsl_core::graph::Graph;
use wxsl_core::node::{Value, SETTING_NAME};
use wxsl_core::scene::{
    ColorSpace, MeshSource, SceneLightKind, TextureSource, TAG_OPAQUE, TAG_TRANSPARENT,
};
use wxsl_gltf::{import_slice, ImportWarning, ImportedScene, GLTF_PBR_GRAPH, UV1_ATTRIBUTE};

fn import(glb: Glb) -> ImportedScene {
    import_slice(&glb.finish(), &wxsl_stdlib::registry()).expect("the file imports")
}

/// The starting value of parameter `name`, if the graph declares it.
fn param(graph: &Graph, name: &str) -> Option<Value> {
    graph.nodes().find_map(|(_, node)| {
        (node.def == "param.value"
            && node.settings.get(SETTING_NAME).map(String::as_str) == Some(name))
        .then(|| node.params["value"])
    })
}

/// Every generated graph must validate and generate a surface module.
fn assert_compiles(imported: &ImportedScene) {
    let registry = wxsl_stdlib::registry();
    for material in &imported.scene.materials {
        assert_eq!(material.graph.name(), GLTF_PBR_GRAPH);
        material
            .graph
            .validate(&registry)
            .unwrap_or_else(|error| panic!("`{}`: {error}", material.name));
    }
}

/// A material using every core texture, a texture transform, a cutout and
/// emissive strength.
fn textured() -> Glb {
    let mut glb = Glb::default();
    glb.uses("KHR_texture_transform");
    glb.uses("KHR_materials_emissive_strength");
    let red = glb.png([255, 0, 0, 255]);
    let grey = glb.png([128, 128, 128, 255]);
    let albedo = glb.texture(red);
    let data = glb.texture(grey);
    let material = glb.material(json!({
        "name": "everything",
        "pbrMetallicRoughness": {
            "baseColorFactor": [1.0, 0.5, 0.5, 1.0],
            "baseColorTexture": {
                "index": albedo,
                "extensions": { "KHR_texture_transform": { "scale": [2.0, 2.0], "rotation": 0.5 } }
            },
            "metallicFactor": 0.25,
            "roughnessFactor": 0.75,
            "metallicRoughnessTexture": { "index": data }
        },
        "normalTexture": { "index": data, "scale": 0.5 },
        "occlusionTexture": { "index": data, "strength": 0.8 },
        "emissiveTexture": { "index": albedo },
        "emissiveFactor": [1.0, 1.0, 1.0],
        "extensions": { "KHR_materials_emissive_strength": { "emissiveStrength": 4.0 } },
        "alphaMode": "MASK",
        "alphaCutoff": 0.3
    }));
    let mesh = glb.triangle(Some(material), false);
    glb.node(json!({ "mesh": mesh }), None);
    glb
}

#[test]
fn a_material_becomes_a_gltf_pbr_graph_with_its_factors_and_bindings() {
    let imported = import(textured());
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    assert_compiles(&imported);
    let material = &imported.scene.materials[0];
    let graph = &material.graph;
    assert_eq!(material.name, "everything");
    assert_eq!(imported.material_sources, [Some(0)]);
    assert_eq!(
        param(graph, "base_color_factor"),
        Some(Value::Vec4([1.0, 0.5, 0.5, 1.0]))
    );
    assert_eq!(param(graph, "metallic_factor"), Some(Value::F32(0.25)));
    assert_eq!(param(graph, "roughness_factor"), Some(Value::F32(0.75)));
    assert_eq!(param(graph, "normal_scale"), Some(Value::F32(0.5)));
    assert_eq!(param(graph, "occlusion_strength"), Some(Value::F32(0.8)));
    assert_eq!(param(graph, "emissive_factor"), Some(Value::Vec3([4.0; 3])));
    assert_eq!(param(graph, "alpha_cutoff"), Some(Value::F32(0.3)));
    assert!(graph.nodes().any(|(_, node)| node.def == "output.discard"));
    assert!(graph.nodes().any(|(_, node)| node.def == "vector.dot"));
    assert!(material.config.tags.contains(TAG_OPAQUE));

    // Each slot and its sampler name one scene texture.
    let slots = &material.bindings.textures;
    for slot in [
        "base_color_texture",
        "metallic_roughness_texture",
        "normal_texture",
        "occlusion_texture",
        "emissive_texture",
    ] {
        assert_eq!(slots[slot], slots[&format!("{slot}_sampler")], "{slot}");
    }
    // A texture is one entry per colour space it is read in: the albedo
    // twice as colour, the data texture three times as data.
    let space = |slot: &str| imported.scene.textures[slots[slot]].color_space;
    assert_eq!(space("base_color_texture"), ColorSpace::Srgb);
    assert_eq!(space("emissive_texture"), ColorSpace::Srgb);
    assert_eq!(space("normal_texture"), ColorSpace::Linear);
    assert_eq!(slots["normal_texture"], slots["occlusion_texture"]);
    assert_eq!(imported.scene.textures.len(), 2);
    assert_eq!(imported.images.len(), 2);
    assert!(matches!(
        imported.scene.textures[0].source,
        TextureSource::Embedded { .. }
    ));
}

#[test]
fn nodes_keep_their_hierarchy_and_a_shared_mesh_is_read_once() {
    let mut glb = Glb::default();
    let mesh = glb.triangle(None, false);
    let root = glb.node(
        json!({ "name": "root", "translation": [0.0, 2.0, 0.0] }),
        None,
    );
    glb.node(
        json!({ "name": "left", "mesh": mesh, "translation": [-1.0, 0.0, 0.0] }),
        Some(root),
    );
    glb.node(
        json!({ "name": "right", "mesh": mesh, "translation": [1.0, 0.0, 0.0] }),
        Some(root),
    );
    let imported = import(glb);
    let scene = &imported.scene;
    assert!(scene.validate().is_empty(), "{:?}", scene.validate());
    assert_eq!(scene.nodes.len(), 3);
    assert_eq!(scene.nodes[1].parent, Some(0));
    assert_eq!(imported.meshes.len(), 1);
    assert!(matches!(
        scene.meshes[0].source,
        MeshSource::Embedded { index: 0 }
    ));
    assert_eq!(scene.instances.len(), 2);
    // The spec's default material: white, metallic 1, roughness 1.
    let material = &scene.materials[0];
    assert_eq!(material.name, "default");
    assert_eq!(imported.material_sources, [None]);
    assert_eq!(
        param(&material.graph, "metallic_factor"),
        Some(Value::F32(1.0))
    );
    // Transforms compose, not baked into the vertices.
    let world = scene.instance_world(&scene.instances[1]);
    assert_eq!([world[12], world[13], world[14]], [1.0, 2.0, 0.0]);
    assert_eq!(imported.meshes[0].vertices[0].position, [-1.0, -1.0, 0.0]);
}

#[test]
fn lights_and_cameras_become_the_environment_block() {
    let mut glb = Glb::default();
    glb.uses("KHR_lights_punctual");
    glb.json["extensions"] = json!({ "KHR_lights_punctual": { "lights": [
        { "name": "sun", "type": "directional", "intensity": 3.0, "color": [1.0, 0.9, 0.8] },
        { "type": "spot", "range": 5.0, "spot": { "innerConeAngle": 0.2, "outerConeAngle": 0.5 } }
    ] } });
    glb.json["cameras"] = json!([
        { "type": "perspective", "perspective": { "yfov": 0.8, "znear": 0.05, "zfar": 50.0 } },
        { "type": "orthographic", "orthographic": { "xmag": 1.0, "ymag": 1.0, "znear": 0.1, "zfar": 10.0 } }
    ]);
    glb.node(
        json!({ "extensions": { "KHR_lights_punctual": { "light": 0 } } }),
        None,
    );
    glb.node(
        json!({ "extensions": { "KHR_lights_punctual": { "light": 1 } } }),
        None,
    );
    glb.node(
        json!({ "name": "eye", "camera": 0, "translation": [0.0, 0.0, 3.0] }),
        None,
    );
    glb.node(json!({ "name": "plan", "camera": 1 }), None);
    let imported = import(glb);
    let environment = imported.scene.environment.as_ref().expect("an environment");
    assert_eq!(environment.lights.len(), 2);
    assert_eq!(environment.lights[0].name, "sun");
    assert_eq!(environment.lights[0].intensity, 3.0);
    assert_eq!(environment.lights[0].node, Some(0));
    assert_eq!(environment.lights[1].range, 5.0);
    assert!(matches!(
        environment.lights[1].kind,
        SceneLightKind::Spot { inner_angle, outer_angle } if inner_angle == 0.2 && outer_angle == 0.5
    ));
    assert_eq!(environment.cameras.len(), 1);
    assert_eq!(environment.cameras[0].fov_y, 0.8);
    assert_eq!(environment.cameras[0].far, 50.0);
    assert!(imported.warnings.iter().any(|warning| matches!(
        warning,
        ImportWarning::Node { node, .. } if node == "plan"
    )));
}

#[test]
fn what_cannot_be_mapped_is_reported_by_name() {
    let mut glb = Glb::default();
    glb.uses("KHR_materials_specular");
    let compressed = glb.ktx2();
    let texture = glb.texture(compressed);
    let material = glb.material(json!({
        "name": "packed",
        "pbrMetallicRoughness": { "baseColorTexture": { "index": texture } }
    }));
    let mesh = glb.triangle(Some(material), false);
    glb.node(json!({ "mesh": mesh }), None);
    let imported = import(glb);
    assert_compiles(&imported);
    let has = |predicate: &dyn Fn(&ImportWarning) -> bool| imported.warnings.iter().any(predicate);
    assert!(has(&|warning| matches!(
        warning,
        ImportWarning::UnsupportedExtension { name } if name == "KHR_materials_specular"
    )));
    assert!(has(&|warning| matches!(
        warning,
        ImportWarning::Image { image, .. } if image == "compressed"
    )));
    assert!(has(&|warning| matches!(
        warning,
        ImportWarning::Material { material, message }
            if material == "packed" && message.starts_with("baseColorTexture")
    )));
    // The factor still drives the colour, with no texture to bind.
    assert!(imported.scene.materials[0].bindings.textures.is_empty());
    assert!(imported.scene.textures.is_empty());
}

#[test]
fn layered_extensions_pick_a_model_and_report_the_rest() {
    let mut glb = Glb::default();
    for extension in [
        "KHR_materials_sheen",
        "KHR_materials_iridescence",
        "KHR_materials_clearcoat",
        "KHR_materials_transmission",
        "KHR_materials_volume",
        "KHR_materials_ior",
    ] {
        glb.uses(extension);
    }
    let velvet = glb.material(json!({
        "name": "velvet",
        "extensions": { "KHR_materials_sheen": {
            "sheenColorFactor": [0.5, 0.1, 0.1], "sheenRoughnessFactor": 0.6
        } }
    }));
    let film = glb.material(json!({
        "name": "film",
        "extensions": {
            "KHR_materials_iridescence": { "iridescenceFactor": 1.0, "iridescenceIor": 1.5 },
            "KHR_materials_clearcoat": { "clearcoatFactor": 1.0 }
        }
    }));
    let paint = glb.material(json!({
        "name": "paint",
        "extensions": { "KHR_materials_clearcoat": { "clearcoatFactor": 1.0 } }
    }));
    let glass = glb.material(json!({
        "name": "glass",
        "alphaMode": "BLEND",
        "doubleSided": true,
        "extensions": {
            "KHR_materials_transmission": { "transmissionFactor": 1.0 },
            "KHR_materials_volume": { "attenuationDistance": 0.5, "attenuationColor": [0.5, 1.0, 1.0] },
            "KHR_materials_ior": { "ior": 1.4 }
        }
    }));
    let veil = glb.material(json!({ "name": "veil", "alphaMode": "BLEND" }));
    for material in [velvet, film, paint, glass, veil] {
        let mesh = glb.triangle(Some(material), false);
        glb.node(json!({ "mesh": mesh }), None);
    }
    let imported = import(glb);
    assert_compiles(&imported);
    let materials = &imported.scene.materials;
    let model = |index: usize| materials[index].config.model.as_deref();
    assert_eq!(model(0), Some("wxsl.sheen"));
    assert_eq!(
        param(&materials[0].graph, "sheen_color_factor"),
        Some(Value::Vec3([0.5, 0.1, 0.1]))
    );
    assert_eq!(model(1), Some("wxsl.iridescent"));
    assert!(imported.warnings.iter().any(|warning| matches!(
        warning,
        ImportWarning::Material { material, message }
            if material == "film" && message.starts_with("KHR_materials_clearcoat ignored")
    )));
    assert_eq!(model(2), Some("wxsl.gltf_clearcoat"));
    assert_eq!(
        param(&materials[2].graph, "clearcoat_factor"),
        Some(Value::F32(1.0))
    );

    let glass = &materials[3];
    assert!(glass.config.tags.contains(wxsl_gltf::TAG_REFRACTIVE));
    assert!(glass.config.tags.contains(wxsl_gltf::TAG_DOUBLE_SIDED));
    // Drawn from both sides by the stock passes (ADR 0090): no warning.
    assert!(glass.config.double_sided);
    assert!(!materials[4].config.double_sided);
    assert!(!imported
        .warnings
        .iter()
        .any(|warning| warning.to_string().contains("double-sided")));
    assert!(!glass.config.tags.contains(TAG_TRANSPARENT));
    assert_eq!(param(&glass.graph, "ior"), Some(Value::F32(1.4)));
    assert_eq!(
        param(&glass.graph, "attenuation_distance"),
        Some(Value::F32(0.5))
    );
    assert_eq!(
        param(&glass.graph, "base_color_factor"),
        Some(Value::Vec4([0.5, 1.0, 1.0, 1.0]))
    );
    assert!(materials[4].config.tags.contains(TAG_TRANSPARENT));

    let lighting = imported.lighting().expect("the models make a set");
    for name in [
        "wxsl.pbr",
        "wxsl.sheen",
        "wxsl.iridescent",
        "wxsl.gltf_clearcoat",
    ] {
        assert!(lighting.by_name(name).is_some(), "{name}");
    }
}

/// `KHR_materials_clearcoat` in full (ADR 0091): both factors, both
/// textures in their channels, and the coat's own normal map.
#[test]
fn clearcoat_maps_its_factors_textures_and_normal_onto_the_gltf_model() {
    let mut glb = Glb::default();
    glb.uses("KHR_materials_clearcoat");
    let grey = glb.png([128, 128, 128, 255]);
    let strength = glb.texture(grey);
    let roughness = glb.texture(grey);
    let normal = glb.texture(grey);
    let material = glb.material(json!({
        "name": "lacquer",
        "extensions": { "KHR_materials_clearcoat": {
            "clearcoatFactor": 0.8,
            "clearcoatTexture": { "index": strength },
            "clearcoatRoughnessFactor": 0.3,
            "clearcoatRoughnessTexture": { "index": roughness },
            "clearcoatNormalTexture": { "index": normal, "scale": 0.5 }
        } }
    }));
    let mesh = glb.triangle(Some(material), false);
    glb.node(json!({ "mesh": mesh }), None);
    let imported = import(glb);
    assert_compiles(&imported);
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let lacquer = &imported.scene.materials[0];
    assert_eq!(lacquer.config.model.as_deref(), Some("wxsl.gltf_clearcoat"));
    for (name, value) in [
        ("clearcoat_factor", 0.8),
        ("clearcoat_roughness_factor", 0.3),
        ("clearcoat_normal_scale", 0.5),
    ] {
        assert_eq!(
            param(&lacquer.graph, name),
            Some(Value::F32(value)),
            "{name}"
        );
    }
    for slot in [
        "clearcoat_texture",
        "clearcoat_roughness_texture",
        "clearcoat_normal_texture",
    ] {
        assert!(lacquer.bindings.textures.contains_key(slot), "{slot}");
    }
}

#[test]
fn a_second_uv_set_is_declared_as_an_attribute() {
    let mut glb = Glb::default();
    let white = glb.png([255; 4]);
    let texture = glb.texture(white);
    let material = glb.material(json!({
        "occlusionTexture": { "index": texture, "texCoord": 1 }
    }));
    let mesh = glb.triangle(Some(material), true);
    glb.node(json!({ "mesh": mesh }), None);
    let imported = import(glb);
    assert_compiles(&imported);
    assert!(imported.scene.materials[0]
        .graph
        .attribute(UV1_ATTRIBUTE)
        .is_some());
    assert!(imported.meshes[0].uv1.is_some());
}

#[test]
fn a_document_survives_a_json_round_trip() {
    let imported = import(textured());
    let text = serde_json::to_string(&imported.scene).expect("serializes");
    let back: wxsl_core::scene::Scene = serde_json::from_str(&text).expect("parses");
    assert_eq!(back.materials.len(), 1);
    assert_eq!(
        back.materials[0].bindings.textures,
        imported.scene.materials[0].bindings.textures
    );
    assert!(matches!(
        back.meshes[0].source,
        MeshSource::Embedded { index: 0 }
    ));
}

#[test]
fn plain_materials_take_the_layered_model_when_both_overflow_the_g_buffer() {
    let mut glb = Glb::default();
    glb.uses("KHR_materials_sheen");
    let velvet = glb.material(json!({
        "name": "velvet",
        "extensions": { "KHR_materials_sheen": { "sheenColorFactor": [0.2, 0.2, 0.6] } }
    }));
    let legs = glb.material(json!({ "name": "legs" }));
    for material in [velvet, legs] {
        let mesh = glb.triangle(Some(material), false);
        glb.node(json!({ "mesh": mesh }), None);
    }
    let imported = import(glb);
    assert_compiles(&imported);
    assert_eq!(
        imported.scene.materials[1].config.model.as_deref(),
        Some("wxsl.sheen")
    );
    assert!(imported.warnings.iter().any(|warning| matches!(
        warning,
        ImportWarning::Material { material, .. } if material == "legs"
    )));
    let lighting = imported.lighting().expect("a set");
    assert_eq!(lighting.len(), 1);
    assert!(
        wxsl_frame::pipeline::gbuffer_bytes_per_sample(&lighting)
            <= wxsl_frame::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE
    );
}
