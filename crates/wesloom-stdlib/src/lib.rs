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
//! and `shaders/README.md` for the porting/authoring workflow and category
//! layout.
//!
//! Ordinary permissively-licensed crate like the rest of the workspace —
//! unlike its predecessor design (a LYGIA port, see ADR 0006, superseded),
//! this crate carries no special license and is not isolated for legal
//! reasons. It stays a separate crate purely for compile-time/binary-size
//! modularity (ADR 0002): a consumer with a small custom node set shouldn't
//! have to compile the whole standard library.
//!
//! This crate is currently scaffolding: no functions have been written yet.

pub mod registry {
    //! Registers every stdlib function as a `wesloom_core::node` node
    //! definition, so the editor and codegen can find them like any other
    //! node.
    //!
    //! Placeholder.
}
