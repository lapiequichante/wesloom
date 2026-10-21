//! A bake of a small scene with a known answer, and the graph rewrite
//! over what the importer generates. No device: the shading is checked
//! in `crates/wxsl/tests/prt.rs`.
//!
//! The scene is a red floor (a 3×3 vertex grid, 4 wide, facing up) under
//! a grey 2-wide roof half a unit above its centre, written as a `.gltf`
//! with an external `.bin` in a temporary directory. A ground grid laid
//! under it is baked as a receiver.

const GREY_ROOF: &str =
    r#"{"pbrMetallicRoughness": {"baseColorFactor": [0.5, 0.5, 0.5, 1], "metallicFactor": 0}}"#;

use std::path::PathBuf;

use glam::Vec3;
use wxsl_prt::{sh, Albedo, Bake, BakeOptions, GroundGrid, Transfer, STRENGTH_PARAM};

fn scene_file(roof_material: &str) -> PathBuf {
    let floor: Vec<[f32; 3]> = (0..9)
        .map(|index| {
            [
                (index % 3) as f32 * 2.0 - 2.0,
                0.0,
                (index / 3) as f32 * 2.0 - 2.0,
            ]
        })
        .collect();
    let floor_indices: Vec<u32> = (0..2)
        .flat_map(|row| (0..2).map(move |column| row * 3 + column))
        .flat_map(|corner| {
            [
                corner,
                corner + 3,
                corner + 1,
                corner + 1,
                corner + 3,
                corner + 4,
            ]
        })
        .collect();
    let roof = [
        [-1.0, 0.5, -1.0],
        [1.0, 0.5, -1.0],
        [1.0, 0.5, 1.0],
        [-1.0, 0.5, 1.0],
    ];
    let roof_indices = [0u32, 1, 2, 0, 2, 3];

    let mut bin = Vec::new();
    let mut views = Vec::new();
    let mut push = |bytes: Vec<u8>| {
        views.push(serde_like_view(bin.len(), bytes.len()));
        bin.extend(bytes);
    };
    let floats = |points: &[[f32; 3]]| {
        points
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    };
    let words = |indices: &[u32]| indices.iter().flat_map(|v| v.to_le_bytes()).collect();
    push(floats(&floor));
    push(floats(&vec![[0.0, 1.0, 0.0]; floor.len()]));
    push(words(&floor_indices));
    push(floats(&roof));
    push(floats(&[[0.0, -1.0, 0.0]; 4]));
    push(words(&roof_indices));

    let accessor = |view: usize, count: usize, kind: &str, component: u32, bounds: &str| {
        format!(
            r#"{{"bufferView":{view},"count":{count},"type":"{kind}","componentType":{component}{bounds}}}"#
        )
    };
    let accessors = [
        accessor(
            0,
            floor.len(),
            "VEC3",
            5126,
            r#","min":[-2,0,-2],"max":[2,0,2]"#,
        ),
        accessor(1, floor.len(), "VEC3", 5126, ""),
        accessor(2, floor_indices.len(), "SCALAR", 5125, ""),
        accessor(3, 4, "VEC3", 5126, r#","min":[-1,0.5,-1],"max":[1,0.5,1]"#),
        accessor(4, 4, "VEC3", 5126, ""),
        accessor(5, 6, "SCALAR", 5125, ""),
    ];
    let gltf = format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0, 1]}}],
  "nodes": [{{"mesh": 0}}, {{"mesh": 1}}],
  "meshes": [
    {{"primitives": [{{"attributes": {{"POSITION": 0, "NORMAL": 1}}, "indices": 2, "material": 0}}]}},
    {{"primitives": [{{"attributes": {{"POSITION": 3, "NORMAL": 4}}, "indices": 5, "material": 1}}]}}
  ],
  "materials": [
    {{"pbrMetallicRoughness": {{"baseColorFactor": [1, 0, 0, 1], "metallicFactor": 0}}}},
    {roof_material}
  ],
  "buffers": [{{"uri": "scene.bin", "byteLength": {}}}],
  "bufferViews": [{}],
  "accessors": [{}]
}}"#,
        bin.len(),
        views.join(","),
        accessors.join(",")
    );
    // One directory per call: the tests run in parallel.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("wxsl-prt-bake-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("scene.bin"), bin).unwrap();
    let path = directory.join("scene.gltf");
    std::fs::write(&path, gltf).unwrap();
    path
}

fn serde_like_view(offset: usize, length: usize) -> String {
    format!(r#"{{"buffer":0,"byteOffset":{offset},"byteLength":{length}}}"#)
}

fn occlusion(vertex: &[f32]) -> f32 {
    Vec3::new(vertex[0], vertex[1], vertex[2]).length()
}

/// Vertex `index` of mesh `mesh` in `bake`.
fn vertex(bake: &Bake, mesh: usize, index: usize) -> &[f32] {
    let stride = bake.transfer.values_per_vertex();
    &bake.meshes[mesh][index * stride..(index + 1) * stride]
}

fn bake(path: &PathBuf, options: BakeOptions) -> Vec<Bake> {
    let registry = wxsl_stdlib::registry();
    let imported = wxsl_gltf::import(path, &registry).unwrap();
    let albedo = Albedo::load(path, &imported).unwrap();
    let bakes = wxsl_prt::bake(&imported, &albedo, &options).unwrap();
    for bake in &bakes {
        bake.check(&imported).unwrap();
    }
    bakes
}

#[test]
fn the_roof_shadows_the_floor_beneath_it_and_not_beyond() {
    let path = scene_file(GREY_ROOF);
    let shadows = BakeOptions {
        bounces: 0,
        transfers: vec![Transfer::Gray],
        ..BakeOptions::default()
    };
    let gray = &bake(&path, shadows.clone())[0];
    let (centre, corner) = (vertex(gray, 0, 4), vertex(gray, 0, 0));
    assert!(
        occlusion(centre) < 0.3,
        "under the roof: {}",
        occlusion(centre)
    );
    assert!(
        occlusion(corner) > 0.85,
        "out in the open: {}",
        occlusion(corner)
    );
    // The open corner's bent normal is nearly straight up: the roof is
    // low on its horizon, where the cosine weighs little.
    let bent = Vec3::new(corner[0], corner[1], corner[2]).normalize();
    assert!(bent.y > 0.99, "{bent}");

    // Under a uniform sky, the shadowed irradiance over the open one is
    // exactly the cosine-weighted occlusion.
    let (width, height) = (32, 16);
    let sky = sh::project_equirect(width, height, &vec![[1.0; 3]; (width * height) as usize]);
    for vertex in [centre, corner] {
        let transfer: [f32; 9] = vertex[3..].try_into().unwrap();
        let ratio = sh::transfer(&sky, &transfer)[0] / sh::irradiance(&sky, Vec3::Y)[0];
        assert!(
            (ratio - occlusion(vertex)).abs() < 1e-3,
            "{ratio} vs {}",
            occlusion(vertex)
        );
    }

    // With a floor under everything, the roof's downward face sees nothing.
    let floored = &bake(
        &path,
        BakeOptions {
            floor: true,
            ..shadows
        },
    )[0];
    assert!((0..4).all(|index| occlusion(vertex(floored, 1, index)) == 0.0));

    // A bake for another file is refused by name.
    let registry = wxsl_stdlib::registry();
    let imported = wxsl_gltf::import(&path, &registry).unwrap();
    let mut short = gray.clone();
    short.meshes[0].truncate(12 * 8);
    assert!(short
        .check(&imported)
        .unwrap_err()
        .to_string()
        .contains("mesh 0"));
    assert!(Bake::default().check(&imported).is_err());
}

#[test]
fn a_ground_under_the_scene_is_shadowed_by_it_and_shadows_nothing() {
    let path = scene_file(GREY_ROOF);
    let registry = wxsl_stdlib::registry();
    let imported = wxsl_gltf::import(&path, &registry).unwrap();
    let albedo = Albedo::load(&path, &imported).unwrap();
    let shadows = BakeOptions {
        bounces: 0,
        transfers: vec![Transfer::Gray],
        ..BakeOptions::default()
    };
    let grid = GroundGrid {
        margin: 1.0,
        spacing: 0.5,
        lift: 0.01,
    };
    let ground = path.with_file_name("ground.glb");
    std::fs::write(
        &ground,
        wxsl_prt::ground(&imported, &shadows, &grid).unwrap(),
    )
    .unwrap();
    let receiver = wxsl_gltf::import(&ground, &registry).unwrap();
    // The floor's 4 m and a metre either side, every half metre.
    assert_eq!(receiver.meshes.len(), 1);
    assert_eq!(receiver.meshes[0].vertices.len(), 13 * 13);
    let centre = receiver.meshes[0].vertices[6 * 13 + 6].position;
    assert!(Vec3::from(centre).distance(Vec3::new(0.0, 0.01, 0.0)) < 1e-5);

    let gray = &wxsl_prt::bake_receiver(&imported, &albedo, &receiver, &shadows).unwrap()[0];
    gray.check(&receiver).unwrap();
    let floor = &bake(&path, shadows.clone())[0];
    let (under, open) = (vertex(gray, 0, 6 * 13 + 6), vertex(gray, 0, 0));
    // Under the roof as the floor itself is, and open past it.
    assert!(
        (occlusion(under) - occlusion(vertex(floor, 0, 4))).abs() < 0.05,
        "{} vs the floor's {}",
        occlusion(under),
        occlusion(vertex(floor, 0, 4))
    );
    assert!(
        occlusion(open) > 0.9,
        "out in the open: {}",
        occlusion(open)
    );
    // The ground shadows nothing: the scene's own bake is unchanged.
    assert_eq!(bake(&path, shadows.clone())[0], *floor);

    // Bounces trace the scene too, for the light it sends down.
    let bounced = &wxsl_prt::bake_receiver(
        &imported,
        &albedo,
        &receiver,
        &BakeOptions {
            bounces: 1,
            ..shadows
        },
    )
    .unwrap()[0];
    bounced.check(&receiver).unwrap();
    assert!(vertex(bounced, 0, 6 * 13 + 6)[3] > under[3]);
}

#[test]
fn a_bounce_off_the_red_floor_lights_the_roof_red_and_grayscale_by_its_luminance() {
    let path = scene_file(GREY_ROOF);
    let none = bake(
        &path,
        BakeOptions {
            bounces: 0,
            ..BakeOptions::default()
        },
    );
    let one = bake(
        &path,
        BakeOptions {
            bounces: 1,
            ..BakeOptions::default()
        },
    );
    assert_eq!(
        one.iter().map(|bake| bake.transfer).collect::<Vec<_>>(),
        Transfer::ALL
    );
    // The roof's centre, facing down at the floor.
    let roof = |bakes: &[Bake], transfer: usize| vertex(&bakes[transfer], 1, 0).to_vec();
    let (gray_before, color_before) = (roof(&none, 0), roof(&none, 1));
    let (gray_after, color_after) = (roof(&one, 0), roof(&one, 1));

    // Without bounces the modes agree: three equal channels.
    for channel in 0..3 {
        assert_eq!(
            color_before[3 + channel * 9..12 + channel * 9],
            gray_before[3..]
        );
    }
    // The visibility does not bounce.
    assert_eq!(gray_after[..3], gray_before[..3]);

    // Band 0 is the transfer's average. The red floor bounces its red,
    // plus a dielectric's 4% specular reflectance in every channel.
    let band0 = |values: &[f32], channel: usize| values[3 + channel * 9];
    let gain = |channel: usize| band0(&color_after, channel) - band0(&color_before, channel);
    let red = gain(0);
    assert!(red > 0.05, "the floor bounces red: {red}");
    for channel in [1, 2] {
        let expected = red * 0.04 / 1.04;
        assert!(
            (gain(channel) - expected).abs() < 1e-5,
            "{} vs {expected}",
            gain(channel)
        );
    }
    // One bounce off one colour: grayscale is the luminance of it.
    let gray = band0(&gray_after, 0) - band0(&gray_before, 0);
    let luminance = (0.2126 * 1.04 + (0.7152 + 0.0722) * 0.04) / 1.04 * red;
    assert!((gray - luminance).abs() < 1e-4, "{gray} vs {luminance}");
}
#[test]
fn a_roof_cut_out_by_its_alpha_mask_lets_the_light_through() {
    let shadows = BakeOptions {
        bounces: 0,
        transfers: vec![Transfer::Gray],
        ..BakeOptions::default()
    };
    let roof = |alpha: f32| {
        format!(
            r#"{{"alphaMode": "MASK", "alphaCutoff": 0.5, "pbrMetallicRoughness": {{"baseColorFactor": [0.5, 0.5, 0.5, {alpha}], "metallicFactor": 0}}}}"#
        )
    };
    let solid = &bake(&scene_file(&roof(0.8)), shadows.clone())[0];
    assert!(occlusion(vertex(solid, 0, 4)) < 0.3);
    let cut = &bake(&scene_file(&roof(0.2)), shadows)[0];
    assert!(
        occlusion(vertex(cut, 0, 4)) > 0.99,
        "{}",
        occlusion(vertex(cut, 0, 4))
    );
}

#[test]
fn applying_wires_the_streams_and_parameters_into_every_imported_material() {
    let registry = wxsl_stdlib::registry();
    let mut imported = wxsl_gltf::import(scene_file(GREY_ROOF), &registry).unwrap();
    let sky = [[0.5; 3]; 9];
    for (material, transfer) in imported.scene.materials.iter_mut().zip(Transfer::ALL) {
        wxsl_prt::apply(&mut material.graph, &registry, &sky, transfer).unwrap();
        material.graph.validate(&registry).unwrap();
        let graph = &material.graph;
        for stream in wxsl_prt::STREAMS {
            let declared = graph.attribute(stream).is_some();
            assert_eq!(declared, transfer.streams().contains(&stream), "{stream}");
        }
        let params: Vec<String> = graph
            .interface(&registry)
            .params
            .fields()
            .iter()
            .map(|field| field.name.to_string())
            .collect();
        assert!(
            params.iter().any(|name| name == STRENGTH_PARAM),
            "{params:?}"
        );
        assert!(params.iter().any(|name| name == "prt_sh8"), "{params:?}");
        // Twice is refused rather than layered.
        let mut again = graph.clone();
        assert!(wxsl_prt::apply(&mut again, &registry, &sky, transfer).is_err());
    }
}
