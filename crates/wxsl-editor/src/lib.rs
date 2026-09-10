//! `wxsl-editor`: the visual node editor.
//!
//! Build a shader graph by hand: see every node, link and unlink them, add
//! new ones from the library, and watch the material, the WXSL it generates
//! and the WGSL that compiles to, all update as you go.
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
//! | [`app`] | [`Editor`]: the panels, the frame, and the shortcuts |
//! | [`canvas`] | the pan/zoom node canvas: layout, links, hit-testing, dragging |
//! | [`highlight`] | colouring the WXSL/WGSL code panels, from `wxsl-lang`'s own lexer |
//! | [`palette`] | searching the node library |
//! | [`preview`] | the offscreen material preview, and the compiled WXSL and WGSL |
//! | [`ui`] | the immediate-mode layer: identity, interaction, widgets |
//! | [`widgets`] | editors for the values a socket can carry, and the colour picker |
//! | [`theme`] | colours and metrics |
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
//! Worth knowing before reaching for it: there is no undo history, no
//! clipboard (that needs a platform dependency this crate does not have), no
//! multi-graph tabs, and no box selection. None of these are hard; they are
//! simply not there yet.

#![warn(missing_docs)]

pub mod app;
pub mod canvas;
pub mod highlight;
pub mod palette;
pub mod preview;
pub mod theme;
pub mod ui;
pub mod widgets;

pub use app::{CodeTab, Editor, EditorConfig};
pub use canvas::{Canvas, View};
pub use palette::NodePicker;
pub use preview::{Preview, PreviewStatus};
pub use theme::{Theme, ThemeMode};
