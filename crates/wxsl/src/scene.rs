//! Turning a [`wxsl_core::scene::Scene`] into something the renderer can
//! draw.
//!
//! [`SceneResources::load`] uploads a scene document's meshes and compiles
//! its materials, [`SceneResources::draw_list`] turns them into the draw
//! list the renderer takes, and [`environment`] translates the document's
//! lights and cameras. The loader lives in `wxsl-render` so the editor's
//! scene mode can use it too
//! ([ADR 0082](../../../docs/adr/0082-the-editors-scene-mode-and-scene-resources-in-the-renderer-crate.md));
//! this module is its stable path.

pub use wxsl_render::scene::{environment, LoadError, SceneImage, SceneResources};
