//! Buffers as graph resources, on the GPU (plan2 P11, ADR 0036).
//!
//! The device-free half — ordering, the never-alias rule, the named
//! attachment-shape error — lives in `wxsl-render`'s graph tests. This is
//! the proof that needs a device: a compute effect fills a storage
//! buffer, a screen effect reads it as storage and draws it, and the
//! picture answers "did the data arrive through the pass group?" by its
//! direction.

use wxsl::render::effect::{RAMP_FILL, RAMP_VIEW};
use wxsl::render::gpu::OffscreenTarget;
use wxsl::render::wgpu;
use wxsl::render::{Attachment, DrawList, PassDesc, Read, RenderGraph, ResourceDesc, TargetConfig};

mod probe;

use probe::{gpu, render_list_in, unlit};

#[test]
fn a_compute_written_buffer_reaches_a_screen_pass_through_the_pass_group() {
    let Some(gpu) = gpu() else { return };
    let size = 256;
    let target = OffscreenTarget::new(&gpu.device, size, size);
    let mut renderer = wxsl::render::Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size, size, target.format()),
    )
    .expect("renderer");
    renderer.add_effect(RAMP_FILL);
    renderer.add_effect(RAMP_VIEW);

    let mut graph = RenderGraph::new(target.format());
    let ramp = graph.resource(ResourceDesc::buffer("ramp", 256 * 4));
    graph.pass(PassDesc::compute("fill ramp", "ramp_fill").with_write(ramp));
    graph.pass(
        PassDesc::screen("show ramp", "ramp_view")
            .with_color(Attachment::clear(RenderGraph::TARGET, wgpu::Color::BLACK))
            .with_reads([Read::current(ramp)]),
    );
    renderer.set_graph(graph).expect("schedules");

    let image = render_list_in(&gpu, &mut renderer, &target, &DrawList::new(), &unlit())
        .expect("the frame renders");

    let pixel = |x: u32, y: u32| {
        let index = ((y * size + x) * 4) as usize;
        [image[index], image[index + 1], image[index + 2]]
    };
    // The ease starts low and ends high; green dominates on the left, red
    // on the right — a direction a memcpy or an empty buffer cannot fake.
    let left = pixel(8, size / 2);
    let right = pixel(size - 8, size / 2);
    assert!(
        left[1] > left[0] + 40,
        "the left edge is the ramp's low end: {left:?}"
    );
    assert!(
        right[0] > right[1] + 40,
        "the right edge is the ramp's high end: {right:?}"
    );

    // And the compute pass is due only when its policy says so: with none
    // set, it ran for this frame — `pass_run_count` is how a test watches
    // it.
    assert_eq!(renderer.pass_run_count("fill ramp"), Some(1));
    assert_eq!(renderer.pass_run_count("show ramp"), Some(1));
}
