//! Importing glTF 2.0 files as scene documents
//! ([ADR 0080](../../../docs/adr/0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)).
//!
//! The import itself is `wxsl-gltf`'s, device-free and shared with the C
//! ABI; this module runs it against the stdlib registry.
//! [`SceneResources::load_imported`] uploads the result.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let device: wgpu::Device = unimplemented!();
//! let imported = wxsl::asset::import_gltf("DamagedHelmet.glb")?;
//! for warning in &imported.warnings {
//!     eprintln!("{warning}");
//! }
//! let resources = wxsl::scene::SceneResources::load_imported(
//!     &device,
//!     &imported,
//!     &wxsl::stdlib::registry(),
//!     &imported.lighting()?,
//!     &[],
//! )?;
//! # Ok(())
//! # }
//! ```
//!
//! [`SceneResources::load_imported`]: crate::scene::SceneResources::load_imported

use std::path::Path;

pub use wxsl_gltf::{
    ImportError, ImportWarning, ImportedScene, GLTF_PBR_GRAPH, MAPPED_EXTENSIONS, TAG_DOUBLE_SIDED,
    TAG_REFRACTIVE,
};

/// Import a `.gltf` or `.glb` file, its materials built from the stdlib.
pub fn import_gltf(path: impl AsRef<Path>) -> Result<ImportedScene, ImportError> {
    wxsl_gltf::import(path, &wxsl_stdlib::registry())
}

/// Import a GLB from memory, for hosts with no file system (the web).
pub fn import_gltf_slice(bytes: &[u8]) -> Result<ImportedScene, ImportError> {
    wxsl_gltf::import_slice(bytes, &wxsl_stdlib::registry())
}
