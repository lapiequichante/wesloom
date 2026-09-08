//! Small helpers for emitting WXSL text: validated identifiers, module
//! paths, and float literals that are actually legal WGSL.
//!
//! This module is *not* a WXSL parser or compiler — that is the `wxsl-lang` crate's
//! job, called from `wxsl-render`
//! ([ADR 0003](../../../docs/adr/0003-wesl-as-the-shading-language.md)). What
//! lives here is the minimum needed to make sure the text this crate emits
//! is well-formed before it reaches that compiler.

use core::fmt;

/// A validated WXSL (and therefore WGSL) identifier.
///
/// Node definitions name sockets, functions and macros; those names end up
/// verbatim in generated source. Validating them at construction keeps a
/// malformed name from turning into a confusing shader-compiler error later.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WxslIdent(String);

impl WxslIdent {
    /// Validate `name` as a WXSL identifier.
    ///
    /// Accepts the WGSL identifier grammar restricted to ASCII (leading
    /// letter or `_`, then letters/digits/`_`), and rejects the `__`-prefixed
    /// names WGSL reserves for implementations.
    pub fn new(name: &str) -> Option<Self> {
        let mut chars = name.chars();
        let first = chars.next()?;
        if !(first.is_ascii_alphabetic() || first == '_') {
            return None;
        }
        if !chars.clone().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        // WGSL reserves identifiers starting with a double underscore, and a
        // lone `_` is the phony assignment target rather than a name.
        if name == "_" || name.starts_with("__") {
            return None;
        }
        Some(WxslIdent(name.to_string()))
    }

    /// The identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WxslIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for WxslIdent {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A WXSL module path such as `package::lighting::pbr`.
///
/// Stored as the text form the `wxsl-lang` crate parses, with the components kept
/// so imports can be grouped per module.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulePath(String);

impl ModulePath {
    /// Validate `path` as a `::`-separated module path.
    ///
    /// The first component may be `package`, `super` or a package name; every
    /// component must be a valid identifier.
    pub fn new(path: &str) -> Option<Self> {
        if path.is_empty() {
            return None;
        }
        let mut count = 0;
        for component in path.split("::") {
            WxslIdent::new(component)?;
            count += 1;
        }
        if count == 0 {
            return None;
        }
        Some(ModulePath(path.to_string()))
    }

    /// The path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModulePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Append `value` to `out` as a WGSL `f32` literal, returning `None` (and
/// leaving `out` unchanged) if the value has no literal form.
///
/// WGSL has no `inf`/`nan` literals, and an integer-valued float must still be
/// written with a decimal point (`1.0`, not `1`) to be an `AbstractFloat`.
pub fn write_f32(out: &mut String, value: f32) -> Option<()> {
    if !value.is_finite() {
        return None;
    }
    let before = out.len();
    // `{:?}` on f32 is the shortest round-tripping form and always keeps a
    // decimal point or exponent (`1.0`, `1e30`, `0.1`).
    let text = format!("{value:?}");
    out.push_str(&text);
    if !text.contains('.') && !text.contains('e') && !text.contains('E') {
        // Defensive: should be unreachable for f32 Debug, but a bare integer
        // here would silently change the literal's type.
        out.truncate(before);
        return None;
    }
    Some(())
}

/// A stable 64-bit hash of `bytes` (FNV-1a).
///
/// Used for shader-variant cache keys, which must be reproducible across
/// processes and Rust versions — `DefaultHasher` guarantees neither.
pub fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_follow_the_wgsl_grammar() {
        assert!(WxslIdent::new("world_normal").is_some());
        assert!(WxslIdent::new("_private1").is_some());
        assert!(WxslIdent::new("1bad").is_none());
        assert!(WxslIdent::new("has space").is_none());
        assert!(WxslIdent::new("has-dash").is_none());
        assert!(WxslIdent::new("").is_none());
        assert!(WxslIdent::new("_").is_none());
        assert!(WxslIdent::new("__reserved").is_none());
    }

    #[test]
    fn module_paths_validate_every_component() {
        assert!(ModulePath::new("package::lighting::pbr").is_some());
        assert!(ModulePath::new("package").is_some());
        assert!(ModulePath::new("package::").is_none());
        assert!(ModulePath::new("package::9").is_none());
        assert!(ModulePath::new("").is_none());
    }

    #[test]
    fn floats_always_keep_a_decimal_point() {
        let mut out = String::new();
        assert!(write_f32(&mut out, 1.0).is_some());
        assert_eq!(out, "1.0");

        out.clear();
        assert!(write_f32(&mut out, -0.5).is_some());
        assert_eq!(out, "-0.5");

        out.clear();
        assert!(write_f32(&mut out, f32::INFINITY).is_none());
        assert!(out.is_empty());
        assert!(write_f32(&mut out, f32::NAN).is_none());
        assert!(out.is_empty());
    }

    #[test]
    fn stable_hash_is_deterministic_and_sensitive() {
        assert_eq!(stable_hash(b"abc"), stable_hash(b"abc"));
        assert_ne!(stable_hash(b"abc"), stable_hash(b"abd"));
        // Pinned against FNV-1a's published reference vectors, so this
        // checks the implementation is really FNV-1a rather than merely
        // checking it against itself. A change to the hash function shows up
        // here as a test failure instead of as silent variant-cache churn.
        //
        // These deliberately do not use a project name as input: the pin
        // survived a rename that changed the input string and left the
        // expected value stale, which is how this test came to be written
        // this way.
        assert_eq!(stable_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(stable_hash(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
