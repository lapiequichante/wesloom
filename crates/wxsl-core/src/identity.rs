//! Registry identity: ids are namespaced `package.name`, and the package
//! is the owner.
//!
//! Every registry id in the workspace — node definitions (`math.add`),
//! effects (`wxsl.bloom`), lighting models (`wxsl.pbr`) — is spelled with
//! one leading package segment and a dot separator. The package says who
//! owns the name, which is what makes two libraries' effects or models
//! able to sit in one registry without their `grain` or their `toon`
//! colliding: each brings its own package. The separator is a dot, not
//! the `::` of WXSL module paths — those name shader imports, this names
//! registry entries, and node ids have been dotted (`math.add`) since the
//! first registry.
//!
//! The shipped vocabulary's package is [`PACKAGE`] (`wxsl`). Documents are
//! allowed to spell shipped ids bare — an un-namespaced id in a *document*
//! resolves against [`PACKAGE`] — because every document written before
//! namespacing existed says `bloom`, not `wxsl.bloom`, and a document that
//! meant the shipped effect should keep meaning it. *Registrations* get no
//! such default: the registries reject an un-namespaced id, so a library
//! adding an effect is forced to say who it is
//! ([ADR 0044](../../docs/adr/0044-identity-versions-and-the-capability-check.md)).

use std::borrow::Cow;

/// The package the shipped vocabulary registers under.
pub const PACKAGE: &str = "wxsl";

/// The separator between a package and the name it owns.
pub const SEPARATOR: char = '.';

/// Split `id` into its package and the rest, when it is namespaced.
///
/// Returns `None` for an id with no separator, and for one whose package
/// or remainder would be empty (`"math."`, `".add"`).
pub fn split(id: &str) -> Option<(&str, &str)> {
    let (package, rest) = id.split_once(SEPARATOR)?;
    if package.is_empty() || rest.is_empty() {
        return None;
    }
    Some((package, rest))
}

/// Whether `id` carries a package segment.
pub fn is_namespaced(id: &str) -> bool {
    split(id).is_some()
}

/// Resolve `spelled` the way a document's reference is resolved: a
/// namespaced id is taken as written, a bare one belongs to the shipped
/// [`PACKAGE`] — `bloom` means `wxsl.bloom`.
pub fn resolve(spelled: &str) -> Cow<'_, str> {
    if is_namespaced(spelled) {
        Cow::Borrowed(spelled)
    } else {
        Cow::Owned(format!("{PACKAGE}{SEPARATOR}{spelled}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_namespaced_id_splits_at_its_first_separator() {
        assert_eq!(split("math.add"), Some(("math", "add")));
        // The rest may itself be segmented; the package is still the first
        // piece — `convert.split.vec3f` belongs to `convert`.
        assert_eq!(
            split("convert.split.vec3f"),
            Some(("convert", "split.vec3f"))
        );
        assert_eq!(split("wxsl.bloom"), Some(("wxsl", "bloom")));
    }

    #[test]
    fn a_bare_or_half_spelled_id_is_not_namespaced() {
        assert!(!is_namespaced("bloom"));
        assert!(!is_namespaced("math."));
        assert!(!is_namespaced(".add"));
        assert!(!is_namespaced(""));
    }

    #[test]
    fn a_bare_reference_resolves_into_the_shipped_package() {
        assert_eq!(resolve("bloom"), "wxsl.bloom");
        assert_eq!(resolve("pbr"), "wxsl.pbr");
        // A namespaced spelling is taken as written, borrowed not copied.
        assert_eq!(resolve("wxsl.bloom"), "wxsl.bloom");
        assert_eq!(resolve("demo.grain"), "demo.grain");
    }
}
