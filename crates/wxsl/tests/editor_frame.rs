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
