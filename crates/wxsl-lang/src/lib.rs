//! `wxsl-lang`: **WXSL**, the shading language this project owns.
//!
//! A lexer, parser, template monomorphizer and WGSL backend for the language
//! the node graph compiles into and the standard library is written in.
//! Sources have the `.wxsl` extension. See
//! [ADR 0011](../../../docs/adr/0011-own-the-shading-language.md) for why
//! this exists rather than a dependency on WXSL.
//!
//! WXSL is WGSL plus four things:
//!
//! * **templates** — `fn f<T: f32 | vec2f>(a: T) -> T`, instantiated per
//!   concrete type with `sizeof(T)` available as a compile-time constant;
//! * **imports** — `import package::math::remap;`, resolved against a module
//!   map the application supplies;
//! * **conditional translation** — `@if(FEATURE)` on declarations and
//!   statements;
//! * **macro constants** — declared with a default in the file that uses
//!   them, overridable globally by a graph or per node instance.
//!
//! Templates and macros share one mechanism: an *instantiation* is a module
//! plus a set of bindings, and two instantiations of one function with
//! different bindings coexist in the output. That is what lets a single
//! material draw the same function twice with different compile-time
//! settings.
//!
//! # Status
//!
//! Complete and in use: `wxsl-render` compiles every shader through this
//! crate, and the `wesl` dependency it replaced is gone. Templates are
//! monomorphized (the `mono` pass), so a generic function reaches the
//! backend as one concrete copy per type it is used at.
//!
//! # Stability
//!
//! The root re-exports are the stable surface ([ADR 0078](../../../docs/adr/0078-three-api-tiers-pinned-by-public-api-snapshots.md)): the
//! `compile*` entry points, [`Modules`], [`Bindings`], diagnostics and
//! [`node_from_source`]. The `ast`, `lexer` and `token` modules are unstable;
//! the compiler's passes are private.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ast;
mod compile;
mod cond;
mod diagnostic;
mod emit;
pub mod lexer;
mod mono;
mod node;
mod parse;
mod resolve;
mod span;
pub mod token;

lalrpop_util::lalrpop_mod!(
    #[allow(clippy::all, clippy::pedantic, unused, missing_docs)]
    grammar
);

pub use ast::{Module, ModulePath};
pub use compile::{compile, compile_to_wxsl, compile_with_macros, CompileError};
pub use cond::{Bindings, Value};
pub use diagnostic::{Diagnostic, Diagnostics, Severity};
pub use emit::{emit, emit_wgsl};
pub use mono::Origins;
pub use node::node_from_source;
pub use parse::{parse, parse_expr};
pub use resolve::{mangle, resolve, Modules};
pub use span::{Span, Spanned};
pub use token::Tok;
