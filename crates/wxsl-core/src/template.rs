//! Template files with `${NAME}` holes, filled by the generators.
//!
//! The repo's best structural decision is that the file is the truth
//! ([ADR 0020](../../../docs/adr/0020-node-definitions-from-a-text-format-the-file-is-the-truth.md)):
//! shader text a human may want to read, diff or edit belongs in a
//! version-controlled file, not inside a Rust string. But some shader text
//! cannot be a *shipped module*, because it names things no fixed module
//! can — the light loop calls a model the module was never told about, the
//! lighting pass binds one texture per enabled target. For that text the
//! pattern is a **template**: a `.wxsl` file under
//! `templates/`, readable and editable as shader except for its holes,
//! which the generator fills at generation time
//! ([plan2 P1](../../../plan2.md)). The generator owns the *logic* — which
//! models, which targets, which ids — and a table of hole → text; the file
//! owns everything else.
//!
//! Holes are `${NAME}`: a spelling no WGSL uses, so a template can hold
//! any shader text without escaping. Filling is strict — a hole in the
//! template with no entry in the table is a generator bug, and so is text
//! left over in the output — and both fail loudly rather than emitting a
//! half-filled shader for the compiler to reject three layers later.

/// Fill every `${NAME}` hole in `template` from `holes`.
///
/// # Panics
///
/// * if `holes` names a hole the template does not contain, or two of its
///   entries name the same hole — both mean the generator's table and the
///   file disagree;
/// * if the output still contains `${` — a hole with no entry, which
///   would otherwise ship as malformed shader.
pub fn fill(template: &str, holes: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (index, (name, _)) in holes.iter().enumerate() {
        assert!(
            !holes[..index].iter().any(|(seen, _)| seen == name),
            "hole `{name}` appears twice in the generator's table"
        );
    }
    for (name, value) in holes {
        let hole = format!("${{{name}}}");
        assert!(
            out.contains(&hole),
            "template names no hole `{name}`: check the generator's hole table \
             against the file"
        );
        out = out.replace(&hole, value);
    }
    if let Some(at) = out.find("${") {
        let end = out[at..]
            .find('}')
            .map_or(out.len(), |offset| (at + offset + 1).min(out.len()));
        panic!(
            "template still names hole `{}` after filling: the generator's table \
             is missing an entry",
            &out[at..end]
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hole_is_replaced_and_only_holes_are_replaced() {
        let filled = fill("fn ${A}(x: ${B}) { }\n", &[("A", "main"), ("B", "f32")]);
        assert_eq!(filled, "fn main(x: f32) { }\n");
    }

    #[test]
    #[should_panic(expected = "still names hole")]
    fn a_value_introducing_a_hole_is_caught_not_refilled() {
        // Single pass, no re-scan — but the leftover check runs after, so
        // a value that itself carries `${` fails loudly instead of
        // shipping. Shader text never does this; the guarantee is the
        // safety net, not a behaviour to lean on.
        fill("(${A})", &[("A", "${B}")]);
    }

    #[test]
    #[should_panic(expected = "names no hole")]
    fn a_table_entry_without_a_hole_in_the_file_is_a_bug() {
        fill("nothing here", &[("A", "x")]);
    }

    #[test]
    #[should_panic(expected = "still names hole")]
    fn a_hole_without_a_table_entry_is_a_bug() {
        fill("${A} ${B}", &[("A", "x")]);
    }

    #[test]
    #[should_panic(expected = "appears twice")]
    fn the_same_hole_twice_in_the_table_is_a_bug() {
        fill("${A}", &[("A", "x"), ("A", "y")]);
    }
}
