//! The editor, driven for a few frames on a real device.
//!
//! The editor draws itself with the renderer (ADR 0013), so "does it work" is
//! a question about a `wgpu` device: the UI shader has to compile, the
//! instance buffer has to be accepted, the glyph atlas has to fill, and the
//! preview has to render into its own target inside the same encoder. None of
//! that is provable without an adapter — everything that *is* provable
//! without one is tested in `wxsl-render` and `wxsl-editor` themselves.
//!
//! Like `render_cube.rs`, this **skips** (prints a note and passes) when no
//! adapter is available, so do not read a pass as proof that it ran.

use glam::Vec2;
use wxsl::core::graph::Graph;
use wxsl::editor::{Editor, EditorConfig};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::ui::input::{Key, MouseButton, UiEvent};
use wxsl::render::ui::{MsdfBackend, UiTarget};

/// The demo graph, so the editor has a real material to compile and draw.
const DEMO_GRAPH: &str = include_str!("../assets/pbr_cube.wxsl.json");

/// A device, or `None` when the machine has no adapter.
fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(error) => {
            println!("skipping: no wgpu adapter ({error})");
            None
        }
    }
}

/// The two fonts, or `None` when this machine has neither.
///
/// The renderer ships no font (ADR 0014), so a test needs one from the
/// system. A machine with no usable font is a skip, not a failure.
fn fonts() -> Option<(Vec<u8>, Vec<u8>)> {
    let candidates = [
        "C:/Windows/Fonts/segoeui.ttf",
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
    ];
    let mono_candidates = [
        "C:/Windows/Fonts/consola.ttf",
        "C:/Windows/Fonts/cour.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
        "/System/Library/Fonts/Menlo.ttc",
    ];
    let read = |paths: &[&str]| paths.iter().find_map(|path| std::fs::read(path).ok());
    match (read(&candidates), read(&mono_candidates)) {
        (Some(ui), Some(mono)) => Some((ui, mono)),
        // A monospaced font is not on every machine; the proportional one
        // serves for both rather than skipping the whole test.
        (Some(ui), None) => Some((ui.clone(), ui)),
        _ => {
            println!("skipping: no system font found");
            None
        }
    }
}

/// An editor over the demo graph, and a target to draw it into.
fn editor(gpu: &GpuContext, backend: MsdfBackend) -> Option<(Editor, OffscreenTarget)> {
    let (ui_font, mono_font) = fonts()?;
    let graph: Graph = serde_json::from_str(DEMO_GRAPH).expect("the demo graph parses");
    let mut config = EditorConfig::new(
        wxsl::stdlib_library(),
        wxsl::stdlib::registry(),
        graph,
        ui_font,
        mono_font,
    );
    config.msdf_backend = backend;
    // Small enough to keep the test quick, large enough that the panels are
    // not degenerate.
    let (width, height) = (1280, 800);
    let mut editor = Editor::new(&gpu.device, config).expect("the editor starts");
    editor.handle_event(UiEvent::Resized {
        size: Vec2::new(width as f32, height as f32),
        scale: 1.0,
    });
    Some((editor, OffscreenTarget::new(&gpu.device, width, height)))
}

/// Draw one frame at `time`, and return the pixels.
fn frame(gpu: &GpuContext, editor: &mut Editor, target: &OffscreenTarget, time: f64) -> Vec<u8> {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("editor test frame"),
        });
    editor
        .frame(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &UiTarget {
                view: target.view(),
                format: target.format(),
                width: target.width(),
                height: target.height(),
                clear: None,
            },
            time,
        )
        .expect("the frame records");
    gpu.queue.submit([encoder.finish()]);
    target.read_rgba8(&gpu.device, &gpu.queue)
}

/// How many pixels differ from the most common colour.
///
/// A cheap "is there an interface here": a frame that drew nothing is one
/// flat colour, and a frame that drew panels, text and a preview is not.
fn painted_pixels(pixels: &[u8]) -> usize {
    let mut histogram = std::collections::HashMap::new();
    for texel in pixels.chunks_exact(4) {
        *histogram
            .entry([texel[0], texel[1], texel[2]])
            .or_insert(0usize) += 1;
    }
    let total = pixels.len() / 4;
    let most_common = histogram.values().copied().max().unwrap_or(total);
    total - most_common
}

#[test]
fn the_editor_draws_an_interface() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };

    // Two frames: the first fills the glyph atlas and compiles the material,
    // the second is the steady state.
    let first = frame(&gpu, &mut editor, &target, 0.0);
    let painted = painted_pixels(&first);
    assert!(
        painted > 10_000,
        "only {painted} pixels differ from the background — the interface did not draw"
    );
    assert!(
        editor.preview().status().is_ok(),
        "the demo graph should compile: {:?}",
        editor.preview().status()
    );
    assert!(
        editor.preview().wxsl().contains("fn wxsl_material"),
        "the WXSL panel has no material function"
    );
    assert!(
        editor.preview().wgsl().contains("fn fs_main"),
        "the WGSL panel has no fragment entry"
    );

    let second = frame(&gpu, &mut editor, &target, 0.016);
    assert!(painted_pixels(&second) > 10_000);
    println!(
        "drew {painted} painted pixels; {} glyphs cached",
        editor.preview().wxsl().lines().count()
    );
}

#[test]
fn both_msdf_backends_draw_the_same_interface() {
    // The GPU generator is a transliteration of the CPU one (ADR 0014), and
    // this is the test that keeps them honest: the same frame, drawn with
    // glyphs from each, must look the same.
    let Some(gpu) = gpu() else { return };
    let Some((mut cpu, cpu_target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    let Some((mut gpu_editor, gpu_target)) = editor(&gpu, MsdfBackend::Gpu) else {
        return;
    };
    assert_eq!(gpu_editor.msdf_backend(), MsdfBackend::Gpu);

    // Both need a frame to fill their atlas, then a frame to compare.
    frame(&gpu, &mut cpu, &cpu_target, 0.0);
    frame(&gpu, &mut gpu_editor, &gpu_target, 0.0);
    // The preview spins, so freeze it: only the text is under test here.
    let with_cpu = frame(&gpu, &mut cpu, &cpu_target, 0.0);
    let with_gpu = frame(&gpu, &mut gpu_editor, &gpu_target, 0.0);

    assert_eq!(with_cpu.len(), with_gpu.len());
    let differing = with_cpu
        .chunks_exact(4)
        .zip(with_gpu.chunks_exact(4))
        .filter(|(left, right)| {
            // A generated field can differ by a byte between the two
            // implementations (float ordering, and `round` at the encoding
            // step), which is a hair of coverage on a glyph's edge.
            left.iter()
                .zip(right.iter())
                .any(|(a, b)| a.abs_diff(*b) > 12)
        })
        .count();
    let total = with_cpu.len() / 4;
    let fraction = differing as f32 / total as f32;
    assert!(
        fraction < 0.01,
        "{differing} of {total} pixels ({:.2}%) differ between the MSDF backends",
        fraction * 100.0
    );
    println!(
        "MSDF backends agree on {:.3}% of pixels",
        100.0 - fraction * 100.0
    );
}

#[test]
fn editing_the_graph_recompiles_the_material() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let before = editor.preview().wxsl().to_string();
    let nodes_before = editor.graph().node_count();

    // Add a node the way the palette does, and check the recompile lands.
    let added = editor.graph_mut().add_node("math.add.vec3f");
    assert_eq!(editor.graph().node_count(), nodes_before + 1);
    frame(&gpu, &mut editor, &target, 0.032);

    // An unconnected node reaches no output, so the *shader* is unchanged —
    // which is the codegen behaviour worth pinning down: a work-in-progress
    // branch costs nothing.
    assert_eq!(
        editor.preview().wxsl(),
        before,
        "an unconnected node should not change the shader"
    );
    assert!(editor.is_modified(), "the document changed");

    // Removing it again leaves the graph as it was.
    editor.graph_mut().remove_node(added);
    frame(&gpu, &mut editor, &target, 0.048);
    assert_eq!(editor.graph().node_count(), nodes_before);
    assert!(editor.preview().status().is_ok());
}

#[test]
fn the_editor_survives_a_frame_of_every_input_event() {
    // Not "does it do the right thing" — the interaction rules are tested
    // without a device — but "does anything panic or fail to record": a
    // click in a panel, a drag on the canvas, typing, and a scroll all in
    // one frame is the shape of a real user's frame.
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);

    let events = [
        UiEvent::PointerMoved(Vec2::new(640.0, 300.0)),
        UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        },
        UiEvent::PointerMoved(Vec2::new(660.0, 320.0)),
        UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        },
        UiEvent::Scroll(Vec2::new(0.0, -120.0)),
        UiEvent::PointerMoved(Vec2::new(120.0, 60.0)),
        UiEvent::Text('a'),
        UiEvent::Key {
            key: Key::Char('f'),
            pressed: true,
            repeat: false,
        },
        UiEvent::Key {
            key: Key::Char('m'),
            pressed: true,
            repeat: false,
        },
        UiEvent::PointerButton {
            button: MouseButton::Right,
            pressed: true,
        },
        UiEvent::PointerButton {
            button: MouseButton::Right,
            pressed: false,
        },
        UiEvent::FocusChanged(false),
        UiEvent::FocusChanged(true),
    ];
    for (index, event) in events.into_iter().enumerate() {
        editor.handle_event(event);
        frame(&gpu, &mut editor, &target, 0.1 + index as f64 * 0.016);
    }
    // Whatever the clicks did, the editor is still in a state that draws.
    let pixels = frame(&gpu, &mut editor, &target, 1.0);
    assert!(painted_pixels(&pixels) > 10_000);
}

#[test]
fn switching_render_path_and_mesh_keeps_the_preview_compiling() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let forward = editor.preview().wgsl().to_string();

    // `D` switches to the deferred path, `M` cycles the mesh: the same graph,
    // compiled for the other pipeline shape (ADR 0005).
    for (index, key) in [Key::Char('d'), Key::Char('m'), Key::Char('m')]
        .into_iter()
        .enumerate()
    {
        editor.handle_event(UiEvent::Key {
            key,
            pressed: true,
            repeat: false,
        });
        frame(&gpu, &mut editor, &target, 0.2 + index as f64 * 0.016);
    }
    assert!(
        editor.preview().status().is_ok(),
        "{:?}",
        editor.preview().status()
    );
    let deferred = editor.preview().wgsl().to_string();
    assert_ne!(
        forward, deferred,
        "the deferred path should compile to different WGSL"
    );
    assert!(
        deferred.contains("pack_gbuffer") || deferred.contains("GBuffer"),
        "the deferred material should write a G-buffer"
    );
}
