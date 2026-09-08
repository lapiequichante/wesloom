//! Macro variables: the graph-level knobs that change the *shape* of the
//! generated shader rather than a value flowing through it.
//!
//! A macro is either
//!
//! * a [`MacroValue::Flag`], which becomes a WESL conditional-translation
//!   feature — `@if(name)` blocks in a node's WESL are kept or dropped by the
//!   compiler ([ADR 0003](../../../docs/adr/0003-wesl-as-the-shading-language.md)),
//!   or
//! * a [`MacroValue::Int`] / [`MacroValue::Float`], which becomes a
//!   module-scope `const` declaration in the generated WESL, so a node's
//!   function can use it in array sizes, loop bounds and const expressions.
//!
//! Either way the value is part of the identity of the compiled shader, so
//! [`MacroSet::signature`] feeds `wesloom-render`'s variant cache key
//! ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).
//!
//! Macros are declared by node definitions ([`MacroDef`]) and can be pinned
//! per graph, which is what makes them editable in the serialized node format
//! ([ADR 0008](../../../docs/adr/0008-surface-graphs-and-a-named-shader-abi.md)).

use core::fmt;
use std::collections::BTreeMap;

use crate::wesl::{write_f32, WeslIdent};

/// The kind of a macro variable, i.e. how it reaches the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MacroKind {
    /// A boolean, bound as a WESL conditional-translation feature.
    Flag,
    /// A signed integer, emitted as an `i32` const declaration.
    Int,
    /// A float, emitted as an `f32` const declaration.
    Float,
}

impl fmt::Display for MacroKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad`, not `write_str`: these names get column-aligned in
        // listings, and `write_str` ignores the width in `{:<8}`.
        f.pad(match self {
            MacroKind::Flag => "flag",
            MacroKind::Int => "int",
            MacroKind::Float => "float",
        })
    }
}

/// The value of a macro variable.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MacroValue {
    /// Bound as a WESL feature flag: `@if(name)` blocks compile in when true.
    Flag(bool),
    /// Emitted as `const name: i32 = value;`.
    Int(i32),
    /// Emitted as `const name: f32 = value;`.
    Float(f32),
}

impl MacroValue {
    /// This value's kind.
    pub fn kind(&self) -> MacroKind {
        match self {
            MacroValue::Flag(_) => MacroKind::Flag,
            MacroValue::Int(_) => MacroKind::Int,
            MacroValue::Float(_) => MacroKind::Float,
        }
    }

    /// The flag value, if this is a [`MacroValue::Flag`].
    pub fn as_flag(&self) -> Option<bool> {
        match self {
            MacroValue::Flag(v) => Some(*v),
            _ => None,
        }
    }

    /// Parse a value from the `name=value` syntax used on command lines and
    /// in editor text fields: `true`/`false` give a flag, a bare integer an
    /// int, anything with a `.`/`e` a float.
    ///
    /// ```
    /// # use wesloom_core::macros::MacroValue;
    /// assert_eq!(MacroValue::parse("true"), Some(MacroValue::Flag(true)));
    /// assert_eq!(MacroValue::parse("4"), Some(MacroValue::Int(4)));
    /// assert_eq!(MacroValue::parse("0.5"), Some(MacroValue::Float(0.5)));
    /// assert_eq!(MacroValue::parse("nope"), None);
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text {
            "true" | "on" | "yes" => return Some(MacroValue::Flag(true)),
            "false" | "off" | "no" => return Some(MacroValue::Flag(false)),
            _ => {}
        }
        if text.contains('.') || text.contains('e') || text.contains('E') {
            text.parse::<f32>().ok().map(MacroValue::Float)
        } else {
            text.parse::<i32>().ok().map(MacroValue::Int)
        }
    }

    /// This value as a WESL const declaration, or `None` for a
    /// [`MacroValue::Flag`] (those are bound as compiler features, not
    /// declarations).
    ///
    /// Returns `None` for a non-finite float, which has no WESL literal.
    pub fn wesl_const(&self, name: &WeslIdent) -> Option<String> {
        match self {
            MacroValue::Flag(_) => None,
            MacroValue::Int(v) => Some(format!("const {name}: i32 = {v};")),
            MacroValue::Float(v) => {
                let mut out = format!("const {name}: f32 = ");
                write_f32(&mut out, *v)?;
                out.push(';');
                Some(out)
            }
        }
    }
}

impl fmt::Display for MacroValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MacroValue::Flag(v) => write!(f, "{v}"),
            MacroValue::Int(v) => write!(f, "{v}"),
            MacroValue::Float(v) => write!(f, "{v}"),
        }
    }
}

/// A macro variable declared by a node definition: its name, default value
/// and what it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct MacroDef {
    /// The macro name, as written in WESL (`@if(NAME)` or the const's name).
    pub name: WeslIdent,
    /// The value used when neither the graph nor the caller pins one.
    pub default: MacroValue,
    /// One-line description, shown in the editor next to the toggle.
    pub doc: &'static str,
}

impl MacroDef {
    /// Declare a macro.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid WESL identifier. Node definitions are
    /// authored in Rust, so this is a programming error, not input handling.
    pub fn new(name: &str, default: MacroValue, doc: &'static str) -> Self {
        MacroDef {
            name: WeslIdent::new(name).expect("macro name must be a valid WESL identifier"),
            default,
            doc,
        }
    }

    /// This macro's kind, taken from its default.
    pub fn kind(&self) -> MacroKind {
        self.default.kind()
    }
}

/// A set of macro name/value pairs.
///
/// Ordered (`BTreeMap`) so that [`MacroSet::signature`] and the generated
/// WESL are byte-for-byte reproducible for the same logical set — the variant
/// cache in `wesloom-render` depends on that.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct MacroSet {
    values: BTreeMap<String, MacroValue>,
}

impl MacroSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin `name` to `value`, returning the value it replaced.
    pub fn set(&mut self, name: impl Into<String>, value: MacroValue) -> Option<MacroValue> {
        self.values.insert(name.into(), value)
    }

    /// Remove `name`, so it falls back to its declared default.
    pub fn unset(&mut self, name: &str) -> Option<MacroValue> {
        self.values.remove(name)
    }

    /// The value pinned for `name`, if any.
    pub fn get(&self, name: &str) -> Option<MacroValue> {
        self.values.get(name).copied()
    }

    /// Whether `name` is pinned in this set.
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// Number of pinned macros.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether nothing is pinned.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Iterate over the pinned macros in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, MacroValue)> {
        self.values.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// The `(name, bool)` pairs to bind as WESL conditional-translation
    /// features, in name order.
    pub fn flags(&self) -> impl Iterator<Item = (&str, bool)> {
        self.iter()
            .filter_map(|(name, value)| value.as_flag().map(|v| (name, v)))
    }

    /// Overlay `other` on top of `self`: every macro pinned in `other`
    /// replaces the one here.
    pub fn overlay(&mut self, other: &MacroSet) {
        for (name, value) in other.iter() {
            self.set(name.to_string(), value);
        }
    }

    /// A stable, human-readable signature of the whole set.
    ///
    /// Two sets with the same signature generate the same shader, which is
    /// what makes this usable in a variant cache key. Kept readable on
    /// purpose: it shows up in shader labels and error messages.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for (name, value) in self.iter() {
            if !out.is_empty() {
                out.push(',');
            }
            out.push_str(name);
            out.push('=');
            match value {
                MacroValue::Flag(v) => out.push_str(if v { "true" } else { "false" }),
                MacroValue::Int(v) => out.push_str(&v.to_string()),
                // Bit pattern, not Display: -0.0 and 0.0 generate different
                // WESL literals, so they must not collide here.
                MacroValue::Float(v) => out.push_str(&format!("{:08x}", v.to_bits())),
            }
        }
        out
    }
}

impl FromIterator<(String, MacroValue)> for MacroSet {
    fn from_iter<T: IntoIterator<Item = (String, MacroValue)>>(iter: T) -> Self {
        MacroSet {
            values: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_covers_the_three_kinds() {
        assert_eq!(MacroValue::parse("  on "), Some(MacroValue::Flag(true)));
        assert_eq!(MacroValue::parse("-3"), Some(MacroValue::Int(-3)));
        assert_eq!(MacroValue::parse("2.5e1"), Some(MacroValue::Float(25.0)));
        assert_eq!(MacroValue::parse(""), None);
    }

    #[test]
    fn flag_has_no_const_declaration() {
        let name = WeslIdent::new("WESLOOM_X").unwrap();
        assert_eq!(MacroValue::Flag(true).wesl_const(&name), None);
        assert_eq!(
            MacroValue::Int(4).wesl_const(&name).unwrap(),
            "const WESLOOM_X: i32 = 4;"
        );
        assert_eq!(
            MacroValue::Float(2.0).wesl_const(&name).unwrap(),
            "const WESLOOM_X: f32 = 2.0;"
        );
        assert_eq!(MacroValue::Float(f32::NAN).wesl_const(&name), None);
    }

    #[test]
    fn signature_is_order_independent_and_distinguishes_signed_zero() {
        let mut a = MacroSet::new();
        a.set("B", MacroValue::Int(1));
        a.set("A", MacroValue::Flag(true));
        let mut b = MacroSet::new();
        b.set("A", MacroValue::Flag(true));
        b.set("B", MacroValue::Int(1));
        assert_eq!(a.signature(), b.signature());

        let mut pos = MacroSet::new();
        pos.set("Z", MacroValue::Float(0.0));
        let mut neg = MacroSet::new();
        neg.set("Z", MacroValue::Float(-0.0));
        assert_ne!(pos.signature(), neg.signature());
    }

    #[test]
    fn overlay_replaces_only_pinned_entries() {
        let mut base = MacroSet::new();
        base.set("A", MacroValue::Int(1));
        base.set("B", MacroValue::Int(2));
        let mut over = MacroSet::new();
        over.set("B", MacroValue::Int(9));
        base.overlay(&over);
        assert_eq!(base.get("A"), Some(MacroValue::Int(1)));
        assert_eq!(base.get("B"), Some(MacroValue::Int(9)));
    }
}
