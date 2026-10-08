//! Device-free C ABI: documents in, frame plans, WGSL and computed layouts out.
//! See ADR 0049 and `README.md` for the wire and ownership contracts.

#![deny(unsafe_code)]

#[cfg(panic = "abort")]
compile_error!("wxsl-ffi requires panic=unwind to contain panics at the C boundary");

pub mod api;
#[allow(unsafe_code)]
pub mod ffi;

/// Transport ABI and request/output schema revision (not the shader ABI revision).
pub const ABI_VERSION: u32 = 1;

/// Generated directly from the exported Rust declarations.
pub const C_HEADER: &str = include_str!(concat!(env!("OUT_DIR"), "/wxsl.h"));
