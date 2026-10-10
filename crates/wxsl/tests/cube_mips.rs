//! The neutral mip/face descriptors actually allocate, record and sample
//! distinct subresources. The same fixture is exported to Dawn.
#[path = "probe/cube_mips.rs"]
mod cube_mips;
mod probe;

#[test]
fn every_cube_face_and_mip_survives_a_skipped_bake() {
    let Some(gpu) = probe::gpu() else { return };
    let mut harness = probe::Harness::new(gpu);
    harness.renderer.add_effect(cube_mips::VIEW);
    harness
        .renderer
        .set_graph(cube_mips::plan(harness.target.format()))
        .unwrap();
    let draws = wxsl::render::DrawList::new();
    let first =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    let second =
        probe::render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws).unwrap();
    assert_eq!(first, second, "once writers retain all subresources");
    for mip in 0..4 {
        for face in 0..6 {
            let expected = cube_mips::color(face, mip);
            let pixel = probe::pixel(
                &first,
                (2 * face + 1) * probe::SIZE / 12,
                (2 * mip + 1) * probe::SIZE / 8,
            );
            for (channel, value) in [expected.r, expected.g, expected.b, expected.a]
                .into_iter()
                .enumerate()
            {
                assert!(
                    (f64::from(pixel[channel]) / 255.0 - value).abs() <= 1.5 / 255.0,
                    "face {face} mip {mip}: {pixel:?}, expected {expected:?}"
                );
            }
            assert_eq!(
                harness
                    .renderer
                    .pass_run_count(&format!("cube face {face} mip {mip}")),
                Some(1)
            );
        }
    }
}
