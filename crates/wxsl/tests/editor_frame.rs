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
use wxsl::core::node::ValueType;
use wxsl::editor::{Editor, EditorConfig, ThemeMode};
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
/// outside. The repo's defaults (`resources/fonts/`) come first, so the
/// test runs — and renders the same text — on every machine; a machine
/// without even a system fallback font is a skip, not a failure.
fn fonts() -> Option<(Vec<u8>, Vec<u8>)> {
    let repo_ui = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../resources/fonts/Inter-Regular.ttf"
    );
    let repo_mono = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../resources/fonts/JetBrainsMono-Regular.ttf"
    );
    let candidates = [
        repo_ui,
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
        repo_mono,
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
    let mut editor = Editor::new(&gpu.device, &gpu.queue, config).expect("the editor starts");
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
        editor.preview().wgsl().contains("fn fs_forward_lit"),
        "the WGSL panel is not showing the forward pipeline's shading stage"
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
    // `math.add` is generic (one node kind serving every type instead of a
    // separate `math.add.f32`/`.vec3f`/…), so it needs its type picked
    // explicitly since nothing is connected to infer it from — and its two
    // operands resolve independently, so picking `A` seeds `B` to match
    // rather than leaving the node half-typed.
    let registry = editor.registry().clone();
    let added = editor.graph_mut().add_node("math.add");
    editor
        .graph_mut()
        .set_generic(&registry, added, "A", ValueType::Vec3)
        .expect("vec3f is one of A's allowed types");
    assert_eq!(
        editor.graph().generic_type(added, "B"),
        Some(ValueType::Vec3),
        "picking one operand's type should carry to the other"
    );
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
    editor.graph_mut().remove_node(&registry, added);
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
fn switching_pipeline_and_mesh_keeps_the_preview_compiling() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let forward = editor.preview().wgsl().to_string();
    assert_eq!(
        editor.preview().pipeline(),
        wxsl::render::StockPipeline::Forward
    );

    // `D` requests the deferred pipeline, `M` cycles the mesh: the same
    // graph, compiled for the other pipeline's stages (ADR 0005, ADR 0022).
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

    // The swap is requested rather than applied, so the editor keeps
    // drawing the forward pipeline until the G-buffer stage is compiled.
    // Every one of those frames is a complete frame.
    let mut waited = 0;
    while editor.preview().swap_progress().is_some() {
        let during = frame(&gpu, &mut editor, &target, 0.3 + waited as f64 * 0.016);
        assert!(
            painted_pixels(&during) > 10_000,
            "a frame during the pipeline swap drew nothing"
        );
        waited += 1;
        assert!(waited < 1_000, "the pipeline swap never landed");
    }
    // One more frame, for the code panel to pick up the stage that landed.
    frame(&gpu, &mut editor, &target, 0.4);

    assert_eq!(
        editor.preview().pipeline(),
        wxsl::render::StockPipeline::Deferred
    );
    assert!(
        editor.preview().status().is_ok(),
        "{:?}",
        editor.preview().status()
    );
    let deferred = editor.preview().wgsl().to_string();
    assert_ne!(
        forward, deferred,
        "the deferred pipeline should show a different stage's WGSL"
    );
    assert!(
        deferred.contains("pack_gbuffer") || deferred.contains("GBuffer"),
        "the deferred material should write a G-buffer"
    );
}

#[test]
fn dragging_a_palette_row_onto_the_canvas_adds_a_node_there() {
    // The bug this pins down: clicking (and so dragging) a palette row did
    // nothing at all, because a premature `active` reset ate every release
    // before the widget that owned it ever saw it (see
    // `wxsl_editor::ui::Interaction`). This drives the exact gesture a user
    // does — press on a row, drag onto the canvas, release — through the
    // real `Editor`, frame by frame, the way `App::window_event` really
    // delivers it.
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let nodes_before = editor.graph().node_count();

    // The palette occupies the left strip and the canvas the middle, but
    // exactly where the first row falls depends on font metrics (the
    // category-button row wraps based on measured text width) — so this
    // probes a vertical strip of the palette for a row, rather than
    // hard-coding one pixel position that would silently test nothing if a
    // future layout tweak shifted it by a few pixels.
    let canvas_center = Vec2::new(700.0, 400.0);
    let mut placed = false;
    for row in 0..20 {
        let press_point = Vec2::new(120.0, 90.0 + row as f64 as f32 * 16.0);

        editor.handle_event(UiEvent::PointerMoved(press_point));
        frame(&gpu, &mut editor, &target, 0.1 + row as f64 * 0.05);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, &mut editor, &target, 0.11 + row as f64 * 0.05);

        // Drag onto the canvas.
        editor.handle_event(UiEvent::PointerMoved(canvas_center));
        frame(&gpu, &mut editor, &target, 0.12 + row as f64 * 0.05);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, &mut editor, &target, 0.13 + row as f64 * 0.05);

        if editor.graph().node_count() == nodes_before + 1 {
            placed = true;
            break;
        }
        // Nothing was under the press point (a gap between rows, or past
        // the last one): harmless, and the next iteration tries lower.
        assert_eq!(
            editor.graph().node_count(),
            nodes_before,
            "a node appeared without a row under the press point"
        );
    }

    assert!(
        placed,
        "no row in the probed strip produced a node; the palette layout \
         may have moved outside the range this test scans"
    );
    let added = editor
        .selection()
        .first()
        .copied()
        .expect("the newly dropped node is selected");
    let position = editor
        .graph()
        .node(added)
        .and_then(|node| node.position)
        .expect("a dropped node is placed, not left without a position");
    let dropped_at = editor
        .canvas_view()
        .to_graph(editor.canvas_rect(), canvas_center);
    let distance = (Vec2::from(position) - dropped_at).length();
    assert!(
        distance < 1.0,
        "the node landed at {position:?}, expected close to {dropped_at:?}"
    );
}

#[test]
fn space_and_a_moving_pointer_pans_and_a_space_tap_toggles_the_spin() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    let space = |pressed| UiEvent::Key {
        key: Key::Char(' '),
        pressed,
        repeat: false,
    };
    let start = Vec2::new(700.0, 400.0);
    editor.handle_event(UiEvent::PointerMoved(start));
    frame(&gpu, &mut editor, &target, 0.0);
    let pan = editor.canvas_view().pan;
    assert!(editor.preview().spinning);

    editor.handle_event(space(true));
    frame(&gpu, &mut editor, &target, 0.1);
    editor.handle_event(UiEvent::PointerMoved(start + Vec2::new(60.0, -30.0)));
    frame(&gpu, &mut editor, &target, 0.2);
    editor.handle_event(space(false));
    frame(&gpu, &mut editor, &target, 0.3);
    let moved = editor.canvas_view().pan - pan;
    assert!(
        moved.x < 0.0 && moved.y > 0.0,
        "the canvas followed the pointer: {moved:?}"
    );
    assert!(editor.preview().spinning, "a pan is not a tap");

    // Holding still, a second pointer move pans nothing.
    let pan = editor.canvas_view().pan;
    editor.handle_event(UiEvent::PointerMoved(start));
    frame(&gpu, &mut editor, &target, 0.4);
    assert_eq!(editor.canvas_view().pan, pan, "no space, no pan");

    editor.handle_event(space(true));
    frame(&gpu, &mut editor, &target, 0.5);
    editor.handle_event(space(false));
    frame(&gpu, &mut editor, &target, 0.6);
    assert!(!editor.preview().spinning, "a tap toggles the spin");
}

#[test]
fn clicking_the_theme_button_toggles_light_and_dark() {
    // The button is pinned to the toolbar's top-right corner and always
    // reaches the panel's right edge (its width only grows leftward to fit
    // "theme: dark" vs "theme: light"), so a point a few pixels in from the
    // top-right corner is inside it regardless of exactly how wide the
    // label measures.
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    assert_eq!(editor.theme().mode, ThemeMode::Dark, "a new editor is dark");

    let click = |editor: &mut Editor, time: f64| {
        let point = Vec2::new(1270.0, 18.0);
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02);
    };

    click(&mut editor, 0.1);
    assert_eq!(
        editor.theme().mode,
        ThemeMode::Light,
        "one click should switch to the light theme"
    );
    click(&mut editor, 0.2);
    assert_eq!(
        editor.theme().mode,
        ThemeMode::Dark,
        "a second click should switch back"
    );
}

#[test]
fn the_pipeline_canvas_edits_the_pipeline_live() {
    // The done-when of plan3 P5: the deferred preset, opened in the
    // editor's second canvas, gains a bloom pass by edit, and the preview
    // runs it — no window between "the document changed" and "the
    // renderer runs it", because the canvas compiles onto the same
    // renderer the preview draws with. The drop itself is the same edit
    // `wxsl-editor::pipeline`'s device-free tests make; this one goes
    // through the editor's own API and lands on a real device.
    use wxsl::core::graph::SocketRef;
    use wxsl::core::pipeline as doc;
    use wxsl::editor::CanvasMode;

    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);

    // Onto the pipeline canvas: it opens on the deferred preset and
    // compiles it onto the preview's renderer at the next frame.
    editor.set_canvas_mode(CanvasMode::Pipeline);
    frame(&gpu, &mut editor, &target, 0.016);
    assert!(
        editor.pipeline_status().is_empty(),
        "the deferred preset compiles: {:?}",
        editor.pipeline_status()
    );
    let passes_before = editor.preview().render_graph().passes().len();

    // The bloom drop: a `pass.screen` with its effect named, a resource
    // for it to write into, and the tonemap's image wire picked up and
    // re-dropped on the new pass's output.
    let registry = wxsl::render::document_registry(editor.preview().effects());
    {
        let graph = editor.pipeline_graph_mut();
        let tonemap = graph
            .nodes()
            .find(|(_, node)| node.label.as_deref() == Some("tonemap"))
            .map(|(id, _)| id)
            .expect("the preset ends in tonemap");
        let scene = graph
            .nodes()
            .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
            .map(|(id, _)| id)
            .expect("the preset has an intermediate target");

        let bloom = graph.add_node(doc::PASS_SCREEN);
        graph.set_setting(bloom, doc::SETTING_EFFECT, "wxsl.bloom");
        if let Some(node) = graph.node_mut(bloom) {
            node.label = Some("bloom".to_string());
        }
        let bloomed = graph.add_node(doc::RESOURCE_COLOR);
        graph.set_setting(bloomed, doc::SETTING_PRECISION, "hdr");
        graph
            .wire(&registry, (scene, "color"), (bloom, "image"))
            .expect("bloom takes an image");
        graph
            .wire(&registry, (bloomed, "color"), (bloom, "into"))
            .expect("bloom writes the new resource");
        graph.disconnect(&registry, &SocketRef::new(tonemap, "image"));
        graph
            .wire(&registry, (bloomed, "color"), (tonemap, "image"))
            .expect("tonemap takes bloom's output");
    }
    frame(&gpu, &mut editor, &target, 0.032);

    assert!(
        editor.pipeline_status().is_empty(),
        "the edited document compiles: {:?}",
        editor.pipeline_status()
    );
    let passes = editor.preview().render_graph().passes();
    assert!(
        passes.iter().any(|pass| pass.label == "bloom"),
        "the running pass list runs bloom: {:?}",
        passes.iter().map(|pass| &pass.label).collect::<Vec<_>>()
    );
    assert_eq!(
        passes.len(),
        passes_before + 1,
        "a bloom drop adds exactly one pass"
    );

    // And the frame is still a picture: the mesh renders through the
    // edited pass list, and the material document was untouched.
    let pixels = frame(&gpu, &mut editor, &target, 0.048);
    assert!(
        painted_pixels(&pixels) > 10_000,
        "the pipeline canvas drew nothing"
    );
    assert!(editor.preview().status().is_ok());
    assert_eq!(editor.canvas_mode(), CanvasMode::Pipeline);

    // The parameter face (ADR 0042): the installed block starts at the
    // declared defaults, and a slider move — what the inspector's editors
    // drive — is a buffer write, compiling nothing.
    use wxsl::core::node::Value;
    assert_eq!(
        editor.preview().pass_param("bloom", "threshold"),
        Some(Value::F32(1.0)),
        "the block starts at the descriptor's defaults"
    );
    let variants_before = editor.preview().variant_stats().0;
    editor
        .set_pass_param("bloom", "threshold", Value::F32(0.5))
        .expect("bloom declares a threshold");
    frame(&gpu, &mut editor, &target, 0.056);
    assert_eq!(
        editor.preview().pass_param("bloom", "threshold"),
        Some(Value::F32(0.5)),
        "the move is visible through the same offsets"
    );
    assert_eq!(
        editor.preview().variant_stats().0,
        variants_before,
        "a slider move compiles nothing"
    );

    // Back to the material canvas: the stock pipeline the material chose
    // comes back, which is what lets the two canvases share one renderer.
    editor.set_canvas_mode(CanvasMode::Material);
    frame(&gpu, &mut editor, &target, 0.064);
    assert!(editor.preview().status().is_ok());
    assert!(
        !editor
            .preview()
            .render_graph()
            .passes()
            .iter()
            .any(|pass| pass.label == "bloom"),
        "the stock pass list came back with the material canvas"
    );
}

/// The features panel (plan3 N9): a checkbox toggles the pipeline's
/// channel live — the plan widens, the budget bar's numbers move with it,
/// and the material is regenerated against the new plan rather than the
/// frame failing its handshake check.
#[test]
fn a_features_checkbox_widens_the_plan_and_the_budget_bar_moves() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    frame(&gpu, &mut editor, &target, 0.016);
    assert!(
        editor.preview().features().is_empty(),
        "a new editor carries no feature channels"
    );
    let bytes = |editor: &Editor| {
        wxsl::render::pipeline::gbuffer_layout_bytes_per_sample(
            editor
                .preview()
                .pipeline_config()
                .plan()
                .expect("the renderer's plan is valid")
                .layout(),
        )
    };
    let before = bytes(&editor);

    // The click points are the layout's own arithmetic at scale 1: the
    // code panel's tab strip is the first row of the bottom panel's inner
    // rect (y = 800 - 24 - 240 + 8 = 544..566), the tabs occupy its first
    // 340 px in four equal cells, so "features" (index 2) centres at
    // x ≈ 220; the checkbox row is the first feature row under the budget
    // bar (y ≈ 596..618).
    let click = |editor: &mut Editor, point: Vec2, time: f64| {
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02);
    };

    click(&mut editor, Vec2::new(220.0, 555.0), 0.1);
    click(&mut editor, Vec2::new(100.0, 607.0), 0.2);
    assert_eq!(
        editor.preview().features(),
        vec!["subsurface"],
        "one click on the checkbox enables the channel"
    );
    assert!(
        bytes(&editor) > before,
        "the budget bar's bytes must move with the plan: {} then {}",
        before,
        bytes(&editor)
    );
    // The regenerated material resolved against the widened plan, which is
    // the handshake both sides check by name — and the preview still runs.
    assert!(
        editor.preview().status().is_ok(),
        "the material recompiled against the new plan: {:?}",
        editor.preview().status()
    );

    // And back off: the channel leaves, the bytes return.
    click(&mut editor, Vec2::new(220.0, 555.0), 0.3);
    click(&mut editor, Vec2::new(100.0, 607.0), 0.4);
    assert!(editor.preview().features().is_empty());
    assert_eq!(bytes(&editor), before);
    assert!(editor.preview().status().is_ok());
}

/// The policy inspector's reset (plan3 N9): fresh run bookkeeping, every
/// pass due again — the `once` policy's observable half. The "run now"
/// button rides the same request path for `on demand` passes, which the
/// stock presets carry none of; `mark_pass` itself is the execution-policy
/// tests' ground.
#[test]
fn the_policy_inspectors_reset_makes_every_pass_due_again() {
    use wxsl::editor::CanvasMode;

    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    editor.set_canvas_mode(CanvasMode::Pipeline);
    frame(&gpu, &mut editor, &target, 0.016);
    frame(&gpu, &mut editor, &target, 0.032);
    frame(&gpu, &mut editor, &target, 0.048);
    let tonemap = editor.preview().pass_run_count("tonemap");
    assert_eq!(tonemap, Some(3), "a per-frame pass runs once per frame");

    // The reset button sits at the left of the passes panel's meta row —
    // the same first row of the bottom panel's inner rect as the material
    // canvas's tabs (y = 544..566), left of the right-aligned counts.
    editor.handle_event(UiEvent::PointerMoved(Vec2::new(305.0, 555.0)));
    frame(&gpu, &mut editor, &target, 0.064);
    editor.handle_event(UiEvent::PointerButton {
        button: MouseButton::Left,
        pressed: true,
    });
    frame(&gpu, &mut editor, &target, 0.072);
    editor.handle_event(UiEvent::PointerButton {
        button: MouseButton::Left,
        pressed: false,
    });
    // The request applies at the end of the frame the click landed in, so
    // it is the *next* frame that runs against fresh bookkeeping.
    frame(&gpu, &mut editor, &target, 0.08);
    frame(&gpu, &mut editor, &target, 0.096);
    assert_eq!(
        editor.preview().pass_run_count("tonemap"),
        Some(1),
        "the reset cleared the bookkeeping and the pass ran once more"
    );
}

/// The resolved material in the inspector (plan3 N9): the demo graph's
/// `tint` parameter as a drag value. Dragging it moves the value in the
/// preview's buffer and pins it on the `param` node, and compiles nothing
/// — a parameter's value is not part of the variant key (ADR 0023).
#[test]
fn dragging_a_material_parameter_writes_the_buffer_and_compiles_nothing() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    // Nothing selected, so the material section sits right under the
    // "no node selected" row; at scale 1 the tint's first component is the
    // field at x ≈ 1086, y ≈ 439.
    editor.select([]);
    frame(&gpu, &mut editor, &target, 0.0);
    frame(&gpu, &mut editor, &target, 0.016);
    let tint = |editor: &Editor| match editor.preview().material_params().as_slice() {
        [(name, wxsl::core::node::Value::Vec3(value))] if name == "tint" => *value,
        other => panic!("the demo graph declares one vec3f tint: {other:?}"),
    };
    assert_eq!(tint(&editor), [1.0, 1.0, 1.0]);
    // Drawing looks variants up every frame, so the baseline is what eight
    // idle frames — as many as the drag below takes — add to the counters.
    let (_, idle_start) = editor.preview().variant_stats();
    for step in 0..8 {
        frame(&gpu, &mut editor, &target, 0.02 + 0.01 * step as f64);
    }
    let (_, compiles_before) = editor.preview().variant_stats();
    let idle = (
        compiles_before.hits - idle_start.hits,
        compiles_before.misses - idle_start.misses,
    );

    let start = Vec2::new(1086.0, 439.0);
    editor.handle_event(UiEvent::PointerMoved(start));
    frame(&gpu, &mut editor, &target, 0.1);
    editor.handle_event(UiEvent::PointerButton {
        button: MouseButton::Left,
        pressed: true,
    });
    frame(&gpu, &mut editor, &target, 0.11);
    for step in 1..=4 {
        editor.handle_event(UiEvent::PointerMoved(
            start - Vec2::new(10.0 * step as f32, 0.0),
        ));
        frame(&gpu, &mut editor, &target, 0.11 + 0.01 * step as f64);
    }
    editor.handle_event(UiEvent::PointerButton {
        button: MouseButton::Left,
        pressed: false,
    });
    frame(&gpu, &mut editor, &target, 0.2);
    frame(&gpu, &mut editor, &target, 0.21);

    let moved = tint(&editor);
    assert!(
        moved[0] < 1.0,
        "dragging left lowers the red component: {moved:?}"
    );
    assert_eq!(&moved[1..], &[1.0, 1.0], "only the dragged component moves");
    let pinned: Vec<_> = editor
        .graph()
        .nodes()
        .filter(|(_, node)| node.def == "param.value")
        .filter_map(|(_, node)| node.params.get("value").copied())
        .collect();
    assert_eq!(
        pinned,
        vec![wxsl::core::node::Value::Vec3(moved)],
        "the value is pinned on the param node, so it saves with the graph"
    );
    let (_, compiles_after) = editor.preview().variant_stats();
    // Hits too: a rebuild of the same graph would be a cache *hit*, so
    // only both counters moving exactly as idle frames do says nothing was
    // regenerated.
    assert_eq!(
        (
            compiles_after.hits - compiles_before.hits,
            compiles_after.misses - compiles_before.misses,
        ),
        idle,
        "a parameter drag neither regenerates nor compiles anything"
    );
    assert!(editor.is_modified());
    assert!(editor.preview().status().is_ok());
}

/// The toolbar's pipeline dropdown: its list opens over the canvas, and
/// picking the third row previews the material through the pipeline
/// canvas's document; picking `forward` puts a stock pipeline back.
#[test]
fn the_pipeline_dropdown_previews_through_the_document() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let click = |editor: &mut Editor, point: Vec2, time: f64| {
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02)
    };
    // At scale 1 the pipeline dropdown sits right of the name field
    // (x ≈ 330..490), and its list opens under it, one 22 px row per
    // option starting at y ≈ 35.
    let dropdown = Vec2::new(400.0, 19.0);
    click(&mut editor, dropdown, 0.1);
    click(&mut editor, Vec2::new(400.0, 90.0), 0.2);
    for step in 0..3 {
        frame(&gpu, &mut editor, &target, 0.3 + 0.01 * step as f64);
    }
    assert!(
        editor.previews_document(),
        "the third row previews through the document"
    );
    assert!(
        editor.pipeline_graph().is_some(),
        "choosing the document opens it"
    );
    assert!(
        editor.pipeline_status().is_empty(),
        "the document compiled: {:?}",
        editor.pipeline_status()
    );
    assert!(
        editor.preview().runs_document(),
        "the renderer runs the installed document, not a stock pass list"
    );
    assert_eq!(
        editor.canvas_mode(),
        wxsl::editor::CanvasMode::Material,
        "the canvas stays on the material"
    );
    assert!(editor.preview().status().is_ok());

    // And back to a stock pipeline through the same list.
    click(&mut editor, dropdown, 0.4);
    click(&mut editor, Vec2::new(400.0, 68.0), 0.5);
    for step in 0..30 {
        frame(&gpu, &mut editor, &target, 0.6 + 0.01 * step as f64);
        if editor.preview().swap_progress().is_none() {
            break;
        }
    }
    assert!(!editor.previews_document());
    assert!(!editor.preview().runs_document());
    assert_eq!(
        editor.preview().pipeline(),
        wxsl::render::StockPipeline::Deferred
    );
    assert!(editor.preview().status().is_ok());
}

/// The G-buffer debug views (plan3 N9): with the subsurface channel in the
/// plan and the material writing a strength of one half, the toolbar's
/// `subsurface` view shows that stored value and nothing else — every
/// covered pixel the same encoded red, no light, no curve — and `shaded`
/// brings the lighting back.
#[test]
fn the_subsurface_debug_view_isolates_the_channel() {
    use wxsl::core::macros::MacroValue;

    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let click = |editor: &mut Editor, point: Vec2, time: f64| {
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02);
    };
    let settle = |editor: &mut Editor, time: f64| {
        for step in 0..30 {
            frame(&gpu, editor, &target, time + 0.01 * step as f64);
            if editor.preview().swap_progress().is_none() && step >= 3 {
                break;
            }
        }
    };

    // The subsurface channel (the features tab's first checkbox, as in
    // the features test), the material's pin of it, then deferred.
    click(&mut editor, Vec2::new(220.0, 555.0), 0.1);
    click(&mut editor, Vec2::new(100.0, 607.0), 0.2);
    assert_eq!(editor.preview().features(), vec!["subsurface"]);
    let graph = editor.graph_mut();
    graph.set_macro("wxsl_subsurface", MacroValue::Flag(true));
    graph.set_macro("wxsl_subsurface_strength", MacroValue::Float(0.5));
    click(&mut editor, Vec2::new(400.0, 19.0), 0.3);
    click(&mut editor, Vec2::new(400.0, 68.0), 0.4);
    settle(&mut editor, 0.5);
    assert_eq!(
        editor.preview().pipeline(),
        wxsl::render::StockPipeline::Deferred
    );
    assert!(editor.preview().status().is_ok());

    // The view menu sits right of the pipeline one (x ≈ 494..686); its
    // rows are 22 px from y ≈ 35: shaded, the six base views, then
    // subsurface, the first feature channel.
    let view_menu = Vec2::new(590.0, 19.0);
    click(&mut editor, view_menu, 1.0);
    click(&mut editor, Vec2::new(590.0, 200.0), 1.1);
    settle(&mut editor, 1.2);
    let shown = wxsl::core::lighting::debug_views()
        .into_iter()
        .find(|view| view.id == editor.preview().debug_view());
    assert_eq!(
        shown.and_then(|view| view.channel),
        Some("subsurface"),
        "the eighth row is the subsurface view"
    );
    assert!(
        editor.preview().status().is_ok(),
        "{:?}",
        editor.preview().status()
    );

    let pixels = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    let background = [pixels[0], pixels[1], pixels[2]];
    let near = |a: u8, b: u8| a.abs_diff(b) <= 2;
    let mut red = 0;
    for texel in pixels.chunks_exact(4) {
        // Nothing lit: no channel rises above the clear colour but the
        // stored strength's red, at the mesh's edges blended with it.
        assert!(
            texel[1] <= background[1] + 2 && texel[2] <= background[2] + 2,
            "a lit pixel under the subsurface view: {texel:?} over {background:?}"
        );
        if near(texel[0], 128) && texel[1] <= 2 && texel[2] <= 2 {
            red += 1;
        }
    }
    assert!(
        red > 5_000,
        "the cube shows the stored 0.5 unbent ({red} pixels)"
    );

    // And back: shading returns, with its light.
    click(&mut editor, view_menu, 2.0);
    click(&mut editor, Vec2::new(590.0, 46.0), 2.1);
    settle(&mut editor, 2.2);
    assert_eq!(editor.preview().debug_view(), 0);
    let pixels = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    let lit = pixels
        .chunks_exact(4)
        .filter(|texel| texel[1] > background[1] + 16)
        .count();
    assert!(lit > 1_000, "shading lights the cube again ({lit} pixels)");
}

/// The A/B split (plan3 N9): the preview header's dropdown puts a deferred
/// B side beside the forward A side, both frames render the same material
/// within the forward/deferred parity the render tests assert, and dragging
/// the edge moves the cut.
#[test]
fn the_ab_split_shows_two_pipelines_and_its_edge_drags() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let click = |editor: &mut Editor, point: Vec2, time: f64| {
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02)
    };
    // At scale 1 the inspector's header holds the dropdown at its right
    // end (x ≈ 1172..1272, y ≈ 39..59); its list opens beneath, one 22 px
    // row per option: off, forward, deferred (y ≈ 119).
    click(&mut editor, Vec2::new(1210.0, 49.0), 0.1);
    click(&mut editor, Vec2::new(1210.0, 119.0), 0.2);
    for step in 0..30 {
        frame(&gpu, &mut editor, &target, 0.3 + 0.01 * step as f64);
        if step >= 3 && editor.preview().swap_progress().is_none() {
            break;
        }
    }
    assert_eq!(
        editor.preview().compare(),
        Some(wxsl::render::StockPipeline::Deferred)
    );
    assert_eq!(
        editor.preview().pipeline(),
        wxsl::render::StockPipeline::Forward,
        "A keeps its pipeline"
    );
    assert!(editor.preview().status().is_ok());

    // The same frame, two pipelines: both drew the cube, and they agree
    // as closely as `render_cube` asserts forward and deferred do.
    let a = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    let b = editor.preview().compare_rgba8(&gpu.device, &gpu.queue);
    assert!(painted_pixels(&a) > 5_000 && painted_pixels(&b) > 5_000);
    let diff: u64 = a
        .iter()
        .zip(&b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    let mean = diff as f64 / a.len() as f64 / 255.0;
    assert!(mean < 0.01, "forward and deferred disagree: {mean}");

    // The edge sits mid-image (x ≈ 1150 of 1047..1253); dragging it left
    // shows more of B.
    let before = editor.preview().split;
    editor.handle_event(UiEvent::PointerMoved(Vec2::new(1150.0, 200.0)));
    frame(&gpu, &mut editor, &target, 1.0);
    editor.handle_event(UiEvent::PointerButton {
        button: MouseButton::Left,
        pressed: true,
    });
    frame(&gpu, &mut editor, &target, 1.01);
    editor.handle_event(UiEvent::PointerMoved(Vec2::new(1100.0, 200.0)));
    frame(&gpu, &mut editor, &target, 1.02);
    editor.handle_event(UiEvent::PointerButton {
        button: MouseButton::Left,
        pressed: false,
    });
    frame(&gpu, &mut editor, &target, 1.03);
    let after = editor.preview().split;
    assert!(
        after < before - 0.1,
        "the edge follows the drag: {before} then {after}"
    );

    // And off again.
    click(&mut editor, Vec2::new(1210.0, 49.0), 1.1);
    click(&mut editor, Vec2::new(1210.0, 75.0), 1.2);
    // The dropdown reads its pick back the frame after the click.
    frame(&gpu, &mut editor, &target, 1.3);
    frame(&gpu, &mut editor, &target, 1.31);
    assert_eq!(editor.preview().compare(), None);
}

/// Pass stepping (plan3 N9): the frame tab freezes the preview on one
/// frame and runs the pass list cut after any pass, showing what that pass
/// wrote; `live` puts the stock pass list back.
#[test]
fn stepping_runs_the_frame_one_pass_at_a_time() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let click = |editor: &mut Editor, point: Vec2, time: f64| {
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02);
        frame(&gpu, editor, &target, time + 0.03)
    };
    click(&mut editor, Vec2::new(400.0, 19.0), 0.1);
    click(&mut editor, Vec2::new(400.0, 68.0), 0.2);
    for step in 0..30 {
        frame(&gpu, &mut editor, &target, 0.3 + 0.01 * step as f64);
        if step >= 3 && editor.preview().swap_progress().is_none() {
            break;
        }
    }
    // The frame tab (the fourth of five cells over the tab strip's 420
    // px), then `step` at the start of its bar.
    click(&mut editor, Vec2::new(294.0, 555.0), 0.7);
    click(&mut editor, Vec2::new(40.0, 581.0), 0.8);
    let step = editor.preview().stepping().expect("the frame is stepped");
    let total = step.passes.len();
    assert_eq!(step.count, total, "stepping starts with every pass run");
    assert_eq!(step.shown, None, "the last pass wrote the frame itself");
    let preview = |editor: &Editor| editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    let full = preview(&editor);
    frame(&gpu, &mut editor, &target, 1.5);
    assert_eq!(preview(&editor), full, "a stepped frame does not move");

    // `previous` twice: the lighting pass, then the G-buffer pass, whose
    // first drawable output is the albedo target — unlit.
    click(&mut editor, Vec2::new(83.0, 581.0), 2.0);
    click(&mut editor, Vec2::new(83.0, 581.0), 2.1);
    let step = editor.preview().stepping().expect("still stepping");
    assert_eq!(step.count, total - 2);
    assert_eq!(step.passes[step.count - 1], "deferred material");
    assert_eq!(step.shown.as_deref(), Some("gbuffer base_color"));
    assert!(editor.preview().status().is_ok());
    let albedo = preview(&editor);
    assert!(painted_pixels(&albedo) > 5_000, "the albedo shows the cube");
    assert_ne!(albedo, full, "the G-buffer is not the lit frame");

    // The show dropdown (x ≈ 170..362) lists the last pass's output, then
    // every texture written so far: shadow maps, the three G-buffer
    // targets, depth — the sixth row, at y ≈ 608 + 5 * 22.
    click(&mut editor, Vec2::new(265.0, 581.0), 3.0);
    click(&mut editor, Vec2::new(265.0, 718.0), 3.1);
    // The dropdown's pick lands after the click's last frame has drawn.
    frame(&gpu, &mut editor, &target, 3.2);
    let step = editor.preview().stepping().expect("still stepping");
    assert_eq!(step.shown.as_deref(), Some("gbuffer depth"));
    assert!(step.pinned);
    let depth = preview(&editor);
    assert!(painted_pixels(&depth) > 5_000, "depth shows the cube");
    assert!(
        depth
            .chunks_exact(4)
            .all(|texel| texel[0] == texel[1] && texel[1] == texel[2]),
        "depth is drawn grey"
    );

    // `live`: the stock pass list again, moving.
    click(&mut editor, Vec2::new(25.0, 581.0), 4.0);
    assert!(editor.preview().stepping().is_none());
    assert!(!editor.preview().runs_document());
    assert_eq!(
        editor.preview().pipeline(),
        wxsl::render::StockPipeline::Deferred
    );
    assert!(editor.preview().status().is_ok());
    let a = preview(&editor);
    frame(&gpu, &mut editor, &target, 4.5);
    assert_ne!(preview(&editor), a, "the live preview spins again");
}

/// Hot reload (plan3 N9): an edited library module on disk lands in the
/// preview, a broken edit is reported in the problems panel while the last
/// working library keeps rendering, and the fix lands again.
#[test]
fn an_edited_shader_file_reloads_into_the_preview() {
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    // A source tree holding one module verbatim: the filmic curve the
    // tonemap pass applies to every pixel of the frame.
    let root = std::env::temp_dir().join(format!("wxsl-hot-reload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("color")).unwrap();
    let file = root.join("color").join("tonemap_filmic.wxsl");
    let original = wxsl::stdlib_library()
        .get("package::color::tonemap_filmic")
        .expect("the library has the curve")
        .to_string();
    std::fs::write(&file, &original).unwrap();
    assert_eq!(editor.watch_shaders(&root), 1);

    // Each write gets a later modification time, so a coarse filesystem
    // clock cannot hide it.
    let mut stamp = std::time::SystemTime::now();
    let mut write = |text: &str| {
        stamp += std::time::Duration::from_secs(5);
        std::fs::write(&file, text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
    };
    let body = original.find("    let x =").expect("the curve's body");
    let edited = |tail: &str| format!("{}{tail}\n}}\n", &original[..body]);
    let red = |pixels: &[u8]| {
        pixels
            .chunks_exact(4)
            .all(|texel| texel[0] == 255 && texel[1] == 0 && texel[2] == 0)
    };

    frame(&gpu, &mut editor, &target, 0.0);
    let before = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    assert!(!red(&before));

    write(&edited("    return vec3f(1.0, 0.0, 0.0);"));
    frame(&gpu, &mut editor, &target, 1.0);
    let pixels = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    assert!(red(&pixels), "the edited curve paints every pixel red");
    assert_eq!(editor.message(), "reloaded tonemap_filmic.wxsl");
    assert!(editor.preview().reload_errors().is_empty());

    write(&edited("    return vec3f(1.0, 0.0;"));
    frame(&gpu, &mut editor, &target, 2.0);
    assert!(
        !editor.preview().reload_errors().is_empty(),
        "the broken edit is reported"
    );
    assert!(editor.message().contains("does not compile"));
    let errors = editor.preview().reload_errors().join("\n");
    assert!(
        errors.contains(&format!(
            "package::color::tonemap_filmic:{}",
            original[..body].lines().count() + 1
        )),
        "the span names the edited module and line: {errors}"
    );
    let pixels = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    assert!(red(&pixels), "the last working curve keeps rendering");

    write(&original);
    frame(&gpu, &mut editor, &target, 3.0);
    assert!(editor.preview().reload_errors().is_empty());
    let pixels = editor.preview().read_rgba8(&gpu.device, &gpu.queue);
    assert!(!red(&pixels), "the original curve is back");
    let _ = std::fs::remove_dir_all(&root);
}

/// Pass timings (plan3 N9): with timestamp queries the pipeline canvas's
/// pass list carries each pass's GPU time a few frames in, and the frame's
/// total is their sum.
#[test]
fn the_pass_list_shows_gpu_timings() {
    use wxsl::editor::CanvasMode;

    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    if !editor.preview().timed() {
        println!("skipping: the device writes no timestamps");
        return;
    }
    editor.set_canvas_mode(CanvasMode::Pipeline);
    for step in 0..40 {
        frame(&gpu, &mut editor, &target, 0.05 * step as f64);
        if step > 4
            && editor.preview().swap_progress().is_none()
            && editor.preview().pass_time("deferred lighting").is_some()
        {
            break;
        }
    }
    let preview = editor.preview();
    let lighting = preview
        .pass_time("deferred lighting")
        .expect("the lighting pass is timed");
    let frame_time = preview.frame_time().expect("the frame is timed");
    let summed: f32 = preview
        .render_graph()
        .passes()
        .iter()
        .filter_map(|pass| preview.pass_time(&pass.label))
        .sum();
    assert!(lighting > 0.0 && lighting <= frame_time);
    assert!((frame_time - summed).abs() < 1e-3 * frame_time.max(1.0));
}

#[test]
fn a_feature_toggled_under_the_document_pipeline_recompiles_it() {
    // The crash this pins down: the preview running the pipeline canvas's
    // document (the pipeline dropdown's third row) kept the pass list
    // compiled against the old plan when a feature was ticked, so the
    // regenerated lighting pass read a G-buffer channel the pass list did
    // not bind — a wgpu validation panic, not an error.
    let Some(gpu) = gpu() else { return };
    let Some((mut editor, target)) = editor(&gpu, MsdfBackend::Cpu) else {
        return;
    };
    frame(&gpu, &mut editor, &target, 0.0);
    let click = |editor: &mut Editor, point: Vec2, time: f64| {
        editor.handle_event(UiEvent::PointerMoved(point));
        frame(&gpu, editor, &target, time);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        frame(&gpu, editor, &target, time + 0.01);
        editor.handle_event(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        frame(&gpu, editor, &target, time + 0.02);
    };
    // The document row of the pipeline dropdown, as in
    // `the_pipeline_dropdown_previews_through_the_document`, then the
    // features tab's first checkbox, as in the features test.
    click(&mut editor, Vec2::new(400.0, 19.0), 0.1);
    click(&mut editor, Vec2::new(400.0, 90.0), 0.2);
    for step in 0..10 {
        frame(&gpu, &mut editor, &target, 0.3 + step as f64 * 0.01);
    }
    let resources = editor.preview().render_graph().resources().len();
    click(&mut editor, Vec2::new(220.0, 555.0), 0.5);
    click(&mut editor, Vec2::new(100.0, 607.0), 0.6);
    assert_eq!(editor.preview().features(), vec!["subsurface"]);
    for step in 0..10 {
        frame(&gpu, &mut editor, &target, 0.7 + step as f64 * 0.01);
    }
    assert_eq!(
        editor.preview().render_graph().resources().len(),
        resources + 1,
        "the document's pass list was recompiled with the channel's target"
    );
    assert!(editor.preview().status().is_ok());
}
