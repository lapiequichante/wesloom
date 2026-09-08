//! `wesloom-stdlib`: the base node library.
//!
//! A granular, composable library of shader functions — math, color, space
//! transforms, SDFs, lighting, generative noise, and so on — exposed as
//! `wesloom-core` nodes. Organized the way libraries like
//! [LYGIA](https://lygia.xyz) organize themselves (one small function per
//! file, grouped by category) because that granularity is genuinely a good
//! design, but every implementation here is written from scratch: no
//! upstream source is translated or copied. See
//! `docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md` for why,
//! and `shaders/README.md` for the authoring rule and category layout.
//!
//! Ordinary permissively-licensed crate like the rest of the workspace —
//! unlike its predecessor design (a LYGIA port, see ADR 0006, superseded),
//! this crate carries no special license and is not isolated for legal
//! reasons. It stays a separate crate purely for compile-time/binary-size
//! modularity (ADR 0002): a consumer with a small custom node set shouldn't
//! have to compile the whole standard library.
//!
//! # The two halves
//!
//! * [`shaders`] holds the WESL sources, embedded and keyed by module path.
//!   These are what the `wesl` compiler resolves imports against, and they
//!   include the shader ABI (`package::wesloom::*`) that a generated material
//!   module is written against.
//! * [`mod@registry`] holds the [`NodeRegistry`](wesloom_core::node::NodeRegistry)
//!   describing those functions as nodes — which one takes what, and returns
//!   what.
//!
//! Both are needed, and they are two halves of one thing: the registry lets a
//! graph be built and type-checked, and the sources let the result compile.
//!
//! ```
//! let registry = wesloom_stdlib::registry();
//! let pbr = registry.get("lighting.pbr_direct").expect("PBR node");
//! assert_eq!(pbr.inputs.len(), 7);
//!
//! // The function the node calls is shipped as WESL source in this crate.
//! assert!(wesloom_stdlib::shaders::module("package::lighting::pbr_direct").is_some());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod registry;
pub mod shaders;

pub use registry::{all_nodes, registry};
pub use shaders::MODULES;
