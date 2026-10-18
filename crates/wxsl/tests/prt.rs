//! Baked environment visibility on a device (ADR 0092): the surface's
//! `diffuse_occlusion` and `specular_occlusion`, and a graph that
//! `wxsl_prt::apply` rewired to read a bake from vertex streams.
//!
//! The bake itself is checked without a device in `wxsl-prt`. What needs
//! one is the loop closing: the fields scale the environment maps on the
//! forward path, the deferred path draws without them, and in both modes
//! the packed streams, the parameters and the PRT nodes compile and agree
//! with the fields.
//!
//! Every frame is lit by the environment maps alone, with no lamps, and
//! goes through the stock pipeline's tonemap.

// Shared with `ibl.rs`, which uses the rest of it.
#[allow(dead_code)]
#[path = "probe/ibl.rs"]
mod fixture;
mod probe;

use glam::Mat4;
use wxsl::core::graph::Graph;
use wxsl::core::node::Value;
use wxsl::render::{
    AttributeValues, DrawItem, Material, MaterialBindings, Mesh, MeshData, MeshKind, StockPipeline,
};
use wxsl_prt::{Bake, Transfer};

/// A grey dielectric: a diffuse and a specular half, both from the maps.
fn surface(name: &str) -> Graph {
    let mut graph = Graph::new(name);
    let output = graph.add_node("output.surface");
    graph.set_param(output, "base_color", Value::Vec3([0.1; 3]));
    graph.set_param(output, "metallic", Value::F32(0.0));
    graph.set_param(output, "roughness", Value::F32(0.45));
    graph
}

/// A harness under `stock`, lit by baked environment maps.
fn harness(stock: StockPipeline) -> Option<probe::Harness> {
    let mut harness = probe::Harness::new(probe::gpu()?);
    for effect in fixture::effects() {
        harness.renderer.add_effect(effect);
    }
    harness.renderer.set_pipeline(stock);
    let mut plan = harness.renderer.render_graph().clone();
    fixture::append_lighting_bake(&mut plan, false);
    harness.renderer.set_graph(plan).unwrap();
    Some(harness)
}

/// The centre pixel of `mesh` drawn standing up, facing the camera.
fn shade(
    harness: &mut probe::Harness,
    mesh: &Mesh,
    material: &Material,
    bindings: Option<&MaterialBindings>,
) -> [u8; 4] {
    let mut item = DrawItem::new(mesh, material)
        .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2));
    if let Some(bindings) = bindings {
        item = item.with_bindings(bindings);
    }
    let draws = wxsl::render::single_draw(item);
    let image = probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect("the frame renders");
    probe::pixel(&image, probe::SIZE / 2, probe::SIZE / 2)
}

fn near(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter()
        .zip(&b)
        .take(3)
        .all(|(a, b)| a.abs_diff(*b) <= tolerance)
}

#[test]
fn the_occlusion_fields_scale_the_maps_forward_and_are_dropped_deferred() {
    for stock in StockPipeline::ALL {
        let Some(mut harness) = harness(*stock) else {
            return;
        };
        let occluded = |harness: &probe::Harness, amount: f32| {
            let mut graph = surface(&format!("occluded {amount}"));
            let output = graph.surface_outputs(&harness.registry)[0];
            graph.set_param(output, "diffuse_occlusion", Value::Vec3([amount; 3]));
            graph.set_param(output, "specular_occlusion", Value::F32(amount));
            harness.material(&graph)
        };
        let plain = harness.material(&surface("plain"));
        let (half, none) = (occluded(&harness, 0.5), occluded(&harness, 0.0));
        let mesh = Mesh::plane(&harness.gpu.device, 2.0);
        let lit = shade(&mut harness, &mesh, &plain, None);
        let halved = shade(&mut harness, &mesh, &half, None);
        let dark = shade(&mut harness, &mesh, &none, None);
        assert!(
            lit[0] > 60,
            "{stock:?}: the maps must light the surface: {lit:?}"
        );
        match stock {
            // The stock pipelines tonemap, so half the light is not half
            // the value; it is strictly between, and none is black.
            StockPipeline::Forward => {
                assert!(
                    dark[..3].iter().all(|value| *value <= 1),
                    "forward: {dark:?}"
                );
                assert!(
                    (0..3)
                        .all(|channel| halved[channel] + 10 < lit[channel] && halved[channel] > 10),
                    "forward: {halved:?} between {dark:?} and {lit:?}"
                );
            }
            // No room in the G-buffer: the fields never reach the
            // lighting pass.
            _ => {
                assert!(near(halved, lit, 1), "{stock:?}: {halved:?} vs {lit:?}");
                assert!(near(dark, lit, 1), "{stock:?}: {dark:?} vs {lit:?}");
            }
        }
    }
}

/// One plane whose every vertex carries `values`, as `transfer` packs
/// them.
fn baked_plane(harness: &probe::Harness, transfer: Transfer, values: &[f32]) -> Mesh {
    let mut data = MeshData::from_kind(MeshKind::Plane);
    let bake = Bake {
        transfer,
        meshes: vec![values.repeat(data.vertices.len())],
    };
    for (name, stream) in transfer.streams().iter().zip(bake.streams(0).unwrap()) {
        data = data.with_attribute(*name, AttributeValues::Vec4(stream));
    }
    Mesh::upload(&harness.gpu.device, "baked plane", &data)
}

#[test]
fn an_applied_bake_reads_its_streams_and_its_strength_parameter_in_both_modes() {
    let Some(mut harness) = harness(StockPipeline::Forward) else {
        return;
    };
    let plain = harness.material(&surface("plain"));
    let plain_mesh = Mesh::upload(
        &harness.gpu.device,
        "plane",
        &MeshData::from_kind(MeshKind::Plane),
    );
    let lit = shade(&mut harness, &plain_mesh, &plain, None);
    assert!(lit[0] > 60, "{lit:?}");
    // A uniform white sky: an open bake's ratio is 1 whatever the sky.
    let mut sky = [[0.0; 3]; 9];
    sky[0] = [(4.0 * std::f32::consts::PI).sqrt(); 3];

    // The plane faces +y and stands up to face +z: an open vertex's
    // transfer is the clamped cosine about +z, and its visibility is the
    // whole hemisphere.
    let toward = glam::Vec3::Z;
    let cosine = [
        std::f32::consts::PI,
        2.0 * std::f32::consts::PI / 3.0,
        std::f32::consts::FRAC_PI_4,
    ];
    let band = [0, 1, 1, 1, 2, 2, 2, 2, 2];
    let open: Vec<f32> = wxsl_prt::sh::basis(toward)
        .iter()
        .zip(band)
        .map(|(y, l)| y * cosine[l])
        .collect();

    for transfer in Transfer::ALL {
        let mut graph = surface(&format!("baked {transfer:?}"));
        wxsl_prt::apply(&mut graph, &harness.registry, &sky, transfer).unwrap();
        let baked = harness.material(&graph);
        let open_values: Vec<f32> = toward
            .to_array()
            .into_iter()
            .chain(open.repeat(transfer.coefficients() / 9))
            .collect();
        let open_mesh = baked_plane(&harness, transfer, &open_values);
        let blocked_mesh = baked_plane(&harness, transfer, &vec![0.0; open_values.len()]);

        let mut bindings = harness.bindings(&baked);
        bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .unwrap();
        let open_lit = shade(&mut harness, &open_mesh, &baked, Some(&bindings));
        let blocked = shade(&mut harness, &blocked_mesh, &baked, Some(&bindings));
        assert!(
            near(open_lit, lit, 2),
            "{transfer:?}: an open sky changes nothing: {open_lit:?} vs {lit:?}"
        );
        assert!(
            blocked[..3].iter().all(|value| *value <= 2),
            "{transfer:?}: every ray blocked: {blocked:?}"
        );

        if transfer == Transfer::Color {
            // Only red arrives, and no reflection: the channels are where
            // the colour node reads them.
            let red: Vec<f32> = [0.0; 3]
                .into_iter()
                .chain(open.iter().copied())
                .chain([0.0; 18])
                .collect();
            let red_mesh = baked_plane(&harness, transfer, &red);
            let reddish = shade(&mut harness, &red_mesh, &baked, Some(&bindings));
            assert!(
                reddish[0] > 20 && reddish[1] <= 2 && reddish[2] <= 2,
                "red only: {reddish:?}"
            );
        }

        // Strength 0 is PRT off, without a recompile.
        bindings
            .set(wxsl_prt::STRENGTH_PARAM, Value::F32(0.0))
            .unwrap();
        bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .unwrap();
        let off = shade(&mut harness, &blocked_mesh, &baked, Some(&bindings));
        assert!(
            near(off, lit, 1),
            "{transfer:?}: strength 0: {off:?} vs {lit:?}"
        );
    }
}
