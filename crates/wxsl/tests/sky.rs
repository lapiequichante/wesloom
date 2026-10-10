//! Optical integration and degenerate inputs on the device, not just phases.
use wxsl::core::{abi, graph::Graph, macros::MacroValue, node::Value};

mod probe;
use probe::{gpu, Harness};

fn evaluate(harness: &mut Harness, values: &[(&str, Value)], steps: (i32, i32)) -> [u8; 4] {
    let mut graph = Graph::new("sky probe");
    let sky = graph.add_node("lighting.sky_single_scattering");
    graph.set_param(sky, "sun_irradiance", Value::Vec3([1.0; 3]));
    for (name, value) in values {
        graph.set_param(sky, *name, *value);
    }
    graph.set_macro("WXSL_SKY_VIEW_STEPS", MacroValue::Int(steps.0));
    graph.set_macro("WXSL_SKY_SUN_STEPS", MacroValue::Int(steps.1));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&harness.registry, (sky, "out"), (output, "emissive"))
        .unwrap();
    let material = harness.material(&graph);
    harness.shade(&material, None, None)
}

#[test]
fn sky_matches_uniform_medium_transport_not_just_the_phase() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    // Uniform density, coincident zenith view/sun rays: view+solar optical
    // distance is exactly 99 km at EVERY midpoint. The integral is closed-form.
    let result = evaluate(
        &mut harness,
        &[
            ("origin", Value::Vec3([0.0, 6361.0, 0.0])),
            ("rayleigh_height", Value::F32(1e20)),
            ("mie_height", Value::F32(1e20)),
            ("rayleigh_scattering", Value::Vec3([0.003, 0.006, 0.009])),
            ("mie_scattering", Value::F32(0.0)),
            ("mie_absorption", Value::F32(0.0)),
        ],
        (32, 8),
    );
    for (channel, beta) in [0.003_f64, 0.006, 0.009].into_iter().enumerate() {
        let expected = beta * 99.0 * (-beta * 99.0).exp() * 3.0 / (8.0 * std::f64::consts::PI);
        assert!(
            (f64::from(result[channel]) / 255.0 - expected).abs() <= 1.5 / 255.0,
            "channel {channel}: {result:?}, expected {expected}"
        );
    }
}

#[test]
fn sky_handles_day_night_horizon_altitude_and_invalid_rays() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let day = evaluate(
        &mut harness,
        &[("sun_direction", Value::Vec3([1.0, 1.0, 0.0]))],
        (128, 64),
    );
    assert!(day[2] > day[0] && day[2] > 2, "Rayleigh-blue day: {day:?}");
    for values in [
        vec![("sun_direction", Value::Vec3([0.0, -1.0, 0.0]))],
        vec![("direction", Value::Vec3([0.0; 3]))],
        vec![("sun_direction", Value::Vec3([0.0; 3]))],
        vec![("origin", Value::Vec3([0.0, 6359.0, 0.0]))],
        vec![("origin", Value::Vec3([0.0, 6500.0, 0.0]))], // looking away
        vec![("atmosphere_radius", Value::F32(6360.0))],
        vec![
            ("rayleigh_scattering", Value::Vec3([0.0; 3])),
            ("mie_scattering", Value::F32(0.0)),
        ],
    ] {
        let result = evaluate(&mut harness, &values, (32, 8));
        assert_eq!(&result[..3], &[0; 3], "black for {values:?}: {result:?}");
    }
    let horizon = evaluate(
        &mut harness,
        &[("direction", Value::Vec3([1.0, 0.0, 0.0]))],
        (128, 64),
    );
    assert!(
        horizon[..3].iter().any(|v| *v > 2),
        "horizon optical path: {horizon:?}"
    );
    let altitude = evaluate(
        &mut harness,
        &[
            ("origin", Value::Vec3([0.0, 6400.0, 0.0])),
            ("sun_direction", Value::Vec3([1.0, 1.0, 0.0])),
        ],
        (128, 64),
    );
    assert!(
        altitude[2] < day[2],
        "less atmosphere overhead: {day:?} vs {altitude:?}"
    );
    let space = evaluate(
        &mut harness,
        &[
            ("origin", Value::Vec3([0.0, 6500.0, 0.0])),
            ("direction", Value::Vec3([0.0, -1.0, 0.0])),
            ("mie_scattering", Value::F32(0.0)),
        ],
        (128, 64),
    );
    assert!(
        space[..3].iter().any(|v| *v > 0),
        "atmosphere viewed from space: {space:?}"
    );
    let clamped = evaluate(&mut harness, &[], (0, -1));
    assert_eq!(
        clamped,
        evaluate(&mut harness, &[], (1, 1)),
        "macro budget clamps"
    );
    let clear = evaluate(&mut harness, &[], (128, 64));
    let absorbing = evaluate(
        &mut harness,
        &[("mie_absorption", Value::F32(0.5))],
        (128, 64),
    );
    assert!(
        absorbing[..3].iter().copied().map(u32::from).sum::<u32>()
            < clear[..3].iter().copied().map(u32::from).sum::<u32>(),
        "absorption extinguishes, never scatters: {clear:?} vs {absorbing:?}"
    );
}

#[test]
fn equirectangular_direction_has_the_same_seam_and_poles_as_the_reader() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    for (uv, expected) in [
        ([0.5, 0.5], [255, 128, 128]),  // +x
        ([0.75, 0.5], [128, 128, 255]), // +z
        ([0.0, 0.5], [0, 128, 128]),    // seam -x
        ([0.5, 0.0], [128, 255, 128]),  // north
        ([0.5, 1.0], [128, 0, 128]),    // south
    ] {
        let mut graph = Graph::new("equirectangular orientation");
        let direction = graph.add_node("space.equirect_to_direction");
        graph.set_param(direction, "uv", Value::Vec2(uv));
        let encode = graph.add_node("math.remap");
        graph
            .set_generic(
                &harness.registry,
                encode,
                "T",
                wxsl::core::node::ValueType::Vec3,
            )
            .unwrap();
        graph.set_param(encode, "in_min", Value::Vec3([-1.0; 3]));
        graph.set_param(encode, "in_max", Value::Vec3([1.0; 3]));
        let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&harness.registry, (direction, "out"), (encode, "value"))
            .unwrap();
        graph
            .wire(&harness.registry, (encode, "out"), (output, "emissive"))
            .unwrap();
        let material = harness.material(&graph);
        let result = harness.shade(&material, None, None);
        for channel in 0..3 {
            assert!(
                result[channel].abs_diff(expected[channel]) <= 1,
                "uv {uv:?}: {result:?}"
            );
        }
    }
}
