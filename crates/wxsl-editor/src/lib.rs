//! `wxsl-editor`: the visual node editor.
//!
//! Build a shader graph by hand: see every node, link and unlink them, add
//! new ones from the library, and watch the material, the WXSL it generates
//! and the WGSL that compiles to, all update as you go. A second canvas
//! ([`CanvasMode::Pipeline`]) edits the *pipeline* the preview runs through — the
//! document compiled live onto the same renderer. A third
//! ([`CanvasMode::Scene`]) opens a whole scene, a `.scene.json` or an
//! imported glTF file, in a viewport with an outliner and an inspector; a
//! click picks an instance and opens its material in the material canvas
//! ([ADR 0082](../../../docs/adr/0082-the-editors-scene-mode-and-scene-resources-in-the-renderer-crate.md)).
//!
//! The editor **draws itself with `wxsl-render`**
//! ([ADR 0013](../../../docs/adr/0013-the-editor-draws-itself-with-wxsl-render.md)):
//! its chrome is a WXSL shader compiled by `wxsl-lang` and submitted
//! through the same path a material takes, and its preview is the real
//! [`Renderer`](wxsl_render::Renderer) with the real pipelines. So there is
//! no third-party GUI toolkit here, and every frame of editing exercises the
//! stack the editor is for.
//!
//! It is also **windowing-agnostic**: it never opens a window and never
//! depends on winit. An application translates its own events into
//! [`wxsl_render::ui::UiEvent`]s, calls [`Editor::handle_event`] with each,
//! and then [`Editor::frame`] once per frame.
//! `crates/wxsl/examples/editor.rs` is that translation, for winit.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use wxsl_editor::{Editor, EditorConfig};
//! # let device: wgpu::Device = unimplemented!();
//! # let queue: wgpu::Queue = unimplemented!();
//! # let library = wxsl_render::ShaderLibrary::new();
//! # let registry = wxsl_core::node::NodeRegistry::new();
//! # let graph = wxsl_core::graph::Graph::new("untitled");
//! # let (ui_font, mono_font) = (Vec::new(), Vec::new());
//! let mut editor = Editor::new(
//!     &device,
//!     &queue,
//!     EditorConfig::new(library, registry, graph, ui_font, mono_font),
//! )?;
//!
//! // …then, once per frame, having fed it this frame's events:
//! # let view: wgpu::TextureView = unimplemented!();
//! let mut encoder = device.create_command_encoder(&Default::default());
//! editor.frame(
//!     &device,
//!     &queue,
//!     &mut encoder,
//!     &wxsl_render::ui::UiTarget {
//!         view: &view,
//!         format: wgpu::TextureFormat::Bgra8Unorm,
//!         width: 1600,
//!         height: 900,
//!         clear: None,
//!     },
//!     0.0,
//! )?;
//! queue.submit([encoder.finish()]);
//! # Ok(())
//! # }
//! ```
//!
//! # What is where
//!
//! | Module | Holds |
//! |---|---|
//! | `app` | [`Editor`]: the panels, the frame, and the shortcuts |
//! | `buffer` | the code editor's text model: caret, selection, lines, undo |
//! | `canvas` | the pan/zoom node canvas: layout, links, hit-testing, dragging |
//! | `command` | [`Command`] and [`Query`]: every change and question, serializable |
//! | `function` | functions written in the editor, derived into `user.*` nodes on save |
//! | `highlight` | colouring the WXSL/WGSL code panels, from `wxsl-lang`'s own lexer |
//! | `history` | running commands, undo and redo by snapshot |
//! | `palette` | searching the node library |
//! | `pipeline` | the second canvas: the pipeline document, compiled live ([`CanvasMode`]) |
//! | `preview` | the offscreen material preview, and the compiled WXSL and WGSL |
//! | `scene` | the open scene, its commands, picking and the orbit camera, without a device |
//! | `scene_mode` | the scene on the GPU, and the viewport, outliner and inspector ([`CanvasMode::Scene`]) |
//! | `ui` | the immediate-mode layer: identity, interaction, widgets |
//! | `widgets` | editors for the values a socket can carry, and the colour picker |
//! | `theme` | colours and metrics |
//!
//! # Commands
//!
//! A document changes only through [`Editor::apply`], which takes a
//! [`Command`]: the UI's gestures, a test and an agent all speak it
//! ([ADR 0081](../../../docs/adr/0081-commands-are-the-editors-single-mutation-path.md)).
//! Each applied command is one undo step ([`Command::Undo`], or `Ctrl`/`Cmd`+`Z`
//! in the window), a [`Command::Batch`] is atomic, and [`Editor::query`]
//! answers the read-only questions. A transcript is one JSON command per
//! line ([`Command::parse_transcript`]).
//!
//! `wxsl-mcp` serves the same vocabulary to agents
//! ([ADR 0083](../../../docs/adr/0083-an-mcp-server-over-the-editors-commands.md)):
//! [`Editor::apply_as_agent`] marks their steps in the history,
//! [`Editor::sync`] compiles between frames, and the session commands
//! ([`Command::SetPipeline`], [`Command::StepFrame`], …) are not undo steps.
//!
//! # `wxsl-core` still knows nothing about how it looks
//!
//! Which ADR 0004 required and this crate keeps: a node *definition* carries
//! no colour, no icon and no widget kind. A socket's port colour comes from
//! its [`ValueType`](wxsl_core::node::ValueType), and which widget an input
//! gets comes from its type — derived from the interface a node already
//! describes.
//!
//! What a node *instance* carries is a different question, and three answers
//! live in the node format because none of them can be derived from
//! anything: its position
//! ([`Node::position`](wxsl_core::graph::Node::position)), because a layout
//! that does not survive a save is not a layout, and its name and colour
//! ([`Node::label`](wxsl_core::graph::Node::label),
//! [`Node::color`](wxsl_core::graph::Node::color)), because what a part of a
//! graph is *for* is something only its author knows
//! ([ADR 0019](../../../docs/adr/0019-node-colour-and-name-are-instance-metadata.md)).
//! Every node is the same colour until one says otherwise.
//!
//! # What the editor does not do
//!
//! Worth knowing before reaching for it: there is no platform
//! clipboard (that needs a platform dependency this crate does not have; the
//! code editor's copy and paste stay inside the editor), no tabs over
//! several *materials*, and no box selection. (The pipeline canvas
//! is a second document with its own state, not a tab.) None of these are
//! hard; they are simply not there yet.
//!
//! # Stability
//!
//! The names re-exported at this root are the whole public surface, and
//! stable ([ADR 0078](../../../docs/adr/0078-three-api-tiers-pinned-by-public-api-snapshots.md)):
//! [`Editor`] and [`EditorConfig`], the commands and queries, and the
//! types their methods return. The
//! modules are private; `unnameable_types` keeps a public signature from
//! leaking one of their types.

#![warn(missing_docs)]
#![warn(unnameable_types)]

mod app;
mod buffer;
mod canvas;
mod command;
mod function;
mod highlight;
mod history;
mod palette;
mod pipeline;
mod preview;
mod reload;
mod scene;
mod scene_mode;
mod stepping;
mod theme;
mod ui;
mod widgets;

pub use app::{Editor, EditorConfig};
pub use canvas::View;
pub use command::{
    Answer, Command, CommandError, Document, HistoryStep, NodeSummary, Outcome, PassSummary, Query,
    SocketSummary, StepSummary, TranscriptError,
};
pub use pipeline::CanvasMode;
pub use preview::{Preview, PreviewStatus, StepView};
pub use theme::{Metrics, Palette, Theme, ThemeMode};
pub use ui::LineMark;
