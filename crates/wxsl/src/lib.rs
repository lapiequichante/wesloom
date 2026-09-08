//! `wxsl`: WXSL shader node graphs on wgpu, with an optional visual node
//! editor.
//!
//! This is the facade most consumers should depend on directly; it
//! re-exports the workspace's other crates behind feature flags so a given
//! build only pays for what it uses:
//!
//! | Feature | Default | Pulls in | Use when |
//! |---|---|---|---|
//! | `render` | yes | [`wxsl_render`] | you need wgpu pipelines (forward/deferred) driven by a graph |
//! | `stdlib` | yes | [`wxsl_stdlib`] | you want the built-in library of base nodes (math, color, lighting, SDFs, …) |
//! | `editor` | no | [`wxsl_editor`] (implies `render`) | you're building a UI that lets users edit graphs visually |
//!
//! [`wxsl_core`] (the graph model) is always available; it has no wgpu
//! or GUI dependency regardless of which features are enabled. See
//! `docs/architecture.md` for the full crate graph and
//! `docs/adr/0002-cargo-workspace-crate-boundaries.md` for why the split
//! exists.
//!
//! # Putting the halves together
//!
//! The renderer deliberately does not depend on the node library (the
//! dependency arrow only points into `wxsl-core`), so an application has
//! to hand the library's WXSL modules to the renderer itself. With both
//! features on, [`stdlib_library`] is that one line:
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use wxsl::render::{Renderer, TargetConfig};
//!
//! let registry = wxsl::stdlib::registry();
//! let graph = wxsl::core::graph::Graph::new("empty");
//! # let device: wgpu::Device = unimplemented!();
//!
//! let mut renderer = Renderer::new(
//!     &device,
//!     wxsl::stdlib_library(),
//!     TargetConfig::new(1280, 720, wgpu::TextureFormat::Rgba8Unorm),
//! )?;
//! let material = wxsl::render::Material::from_graph(&graph, &registry)?;
//! renderer.prepare(&device, &material)?;
//! # Ok(())
//! # }
//! ```
//!
//! See `crates/wxsl/examples/pbr_cube.rs` for a complete program: a PBR
//! cube, authored as a node graph, rendered through either path.

pub use wxsl_core as core;

#[cfg(feature = "render")]
pub use wxsl_render as render;

#[cfg(feature = "editor")]
pub use wxsl_editor as editor;

#[cfg(feature = "stdlib")]
pub use wxsl_stdlib as stdlib;

/// A [`ShaderLibrary`](wxsl_render::ShaderLibrary) preloaded with every
/// WXSL module `wxsl-stdlib` ships, including the shader ABI a generated
/// material module is written against.
///
/// Available only with both `render` and `stdlib`, because it is precisely
/// the bridge between them that neither crate may build itself.
#[cfg(all(feature = "render", feature = "stdlib"))]
pub fn stdlib_library() -> wxsl_render::ShaderLibrary {
    let mut library = wxsl_render::ShaderLibrary::new();
    library.insert_all(wxsl_stdlib::MODULES.iter().copied());
    library
}

#[cfg(all(test, feature = "render", feature = "stdlib"))]
mod tests {
    #[test]
    fn the_stdlib_library_satisfies_the_shader_abi() {
        let library = super::stdlib_library();
        library
            .check_abi()
            .expect("the stdlib ships every ABI module");
        assert_eq!(library.len(), wxsl_stdlib::MODULES.len());
    }
}
