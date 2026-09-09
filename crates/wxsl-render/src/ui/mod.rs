//! The 2D UI layer: an atlas, MSDF text, a draw list, and one pass.
//!
//! This is the half of `wxsl-render` that has nothing to do with
//! materials. It exists because the node editor draws itself with this crate
//! rather than with a third-party GUI toolkit
//! ([ADR 0013](../../../docs/adr/0013-the-editor-draws-itself-with-wxsl-render.md)),
//! and it is useful on its own: an application embedding the renderer gets a
//! text and overlay stack whether or not it ships the editor.
//!
//! ```text
//!   DrawList ──┐
//!   (instances)│
//!              ├──> UiRenderer ──> one render pass ──> frame
//!   Atlas  ────┘        ^
//!     ^                 └── package::wxsl::ui, compiled from the
//!     │                     application's ShaderLibrary (ADR 0009)
//!   GlyphCache ──> MsdfGenerator ──> CPU (ui::msdf) or GPU (package::wxsl::msdf)
//! ```
//!
//! | Module | Holds |
//! |---|---|
//! | [`draw`] | [`Rect`], [`Color`], the [`UiInstance`] layout, and [`DrawList`] |
//! | [`atlas`] | the shared glyph and image texture, and its shelf packer |
//! | [`msdf`] | multi-channel distance fields from outlines, on the CPU |
//! | [`msdf_gpu`] | the same, as a compute pass, and the [`MsdfBackend`] switch |
//! | [`font`] | glyph outlines and metrics, from bytes the application supplies |
//! | [`text`] | the glyph cache, and shaping a string into instances |
//! | [`input`] | windowing-agnostic events and the per-frame [`InputState`] |
//! | [`renderer`] | the pipeline, the instance buffer, and recording a pass |
//!
//! # Everything is one instance
//!
//! A rounded box, a capsule, an image and a glyph are all the same primitive
//! with a different `kind` and a different local frame, and each is one
//! 68-byte instance — the quad's corners come from the vertex index, so there
//! is no vertex or index buffer anywhere in the UI path. A batch is a run of
//! instances sharing a texture and a clip rectangle, which makes a panel full
//! of text and shapes a single `draw` call.
//!
//! # What is testable without a GPU
//!
//! Nearly all of it, and deliberately (ADR 0013): distance-field generation,
//! atlas packing, line breaking, glyph placement, caret and hit-testing,
//! draw-list batching and input accumulation are pure functions over data.
//! Only [`renderer`] needs a device.

pub mod atlas;
pub mod draw;
pub mod font;
pub mod input;
pub mod msdf;
pub mod msdf_gpu;
pub mod renderer;
pub mod text;

pub use atlas::{Atlas, AtlasRegion};
pub use draw::{Batch, Color, DrawList, Rect, TextureId, UiInstance, UiKind};
pub use font::{Font, FontMetrics, GlyphField};
pub use input::{InputState, Key, KeyPress, Modifiers, MouseButton, UiEvent};
pub use msdf::{MsdfBitmap, MsdfRequest, MsdfTransform, Shape};
pub use msdf_gpu::{MsdfBackend, MsdfCompute, MsdfGenerator};
pub use renderer::{UiRenderer, UiTarget};
pub use text::{FontId, Glyph, GlyphCache, TextLayout, TextOptions};
