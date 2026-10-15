//! The node reference: every node a graph can place, generated from the
//! registries (plan6 M2).
//!
//! One generator serves both readers. `wxsl://nodes` is generated from
//! the running editor's registries, so it includes the session's `user.*`
//! functions. `docs/reference/nodes.md` is generated from the shipped
//! ones by `cargo run -p wxsl-mcp --example node_reference`, and a test
//! fails when the file is out of date.

use std::fmt::Write;

use wxsl_core::node::{
    Domains, GraphDomain, NodeDefinition, NodeRegistry, Socket, TypeRule, ValueType,
};

/// The reference over `shaders` (what a material or a screen graph places)
/// and `pipeline` (what a pipeline document places), as Markdown.
pub fn node_reference<'a>(shaders: &'a NodeRegistry, pipeline: &'a NodeRegistry) -> String {
    let mut out = String::from(
        "# Node reference\n\n\
         Every node a graph can place, generated from the registries. A node is \
         added with `add_node` by its id, and its sockets are named as listed \
         here. A socket with a default can be left unconnected. `T`, `U`… are \
         generic: they take the type of what is connected, or the one \
         `set_generic` picks.\n\n",
    );
    let parts = [
        ("Shader nodes", "shader", shaders, Domains::SHADERS),
        ("Pipeline nodes", "pipeline", pipeline, Domains::DOCUMENT),
    ];
    let placed = |registry: &'a NodeRegistry, domains: Domains| {
        registry.iter().filter(move |definition| {
            domains
                .iter()
                .any(|domain| definition.domains.allows(domain))
        })
    };
    for (title, anchor, registry, domains) in parts {
        let count = placed(registry, domains).count();
        let _ = writeln!(out, "- [{title}](#{anchor}-nodes) ({count})");
    }
    for (title, anchor, registry, domains) in parts {
        let _ = write!(out, "\n## {title}\n\n");
        let categories = registry.categories_in(domains);
        let index: Vec<String> = categories
            .iter()
            .map(|category| format!("[{category}](#{anchor}-{category})"))
            .collect();
        let _ = writeln!(out, "Categories: {}.", index.join(", "));
        for category in categories {
            let _ = write!(
                out,
                "\n<a id=\"{anchor}-{category}\"></a>\n\n### {category}\n"
            );
            for definition in
                placed(registry, domains).filter(|definition| definition.category == category)
            {
                entry(&mut out, definition);
            }
        }
    }
    out
}

/// One node's entry.
fn entry(out: &mut String, definition: &NodeDefinition) {
    let _ = write!(out, "\n#### `{}` · {}\n\n", definition.id, definition.label);
    let doc = definition
        .doc
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !doc.is_empty() {
        let _ = write!(out, "{doc}\n\n");
    }
    for (direction, sockets) in [("in", &definition.inputs), ("out", &definition.outputs)] {
        for socket in sockets {
            socket_line(out, direction, socket);
        }
    }
    for generic in &definition.generics {
        let allowed: Vec<String> = generic.allowed.iter().map(ValueType::to_string).collect();
        let _ = writeln!(out, "- `{}`: one of {}", generic.name, allowed.join(", "));
    }
    for setting in &definition.settings {
        let choices = if setting.choices.is_empty() {
            String::new()
        } else {
            format!(", one of {}", setting.choices.join(", "))
        };
        let _ = writeln!(
            out,
            "- setting `{}` = `{}`{choices}{}",
            setting.name,
            setting.default,
            described(&setting.doc)
        );
    }
    for definition in &definition.macros {
        let _ = writeln!(
            out,
            "- macro `{}` = `{}`{}",
            definition.name,
            definition.default,
            described(&definition.doc)
        );
    }
    let mut notes = Vec::new();
    if definition.domains != Domains::ALL {
        let domains: Vec<&str> = definition.domains.iter().map(domain_name).collect();
        notes.push(format!("only in {} graphs", domains.join(" and ")));
    }
    if definition.fragment_only {
        notes.push("fragment stage only".to_string());
    }
    if !notes.is_empty() {
        let _ = writeln!(out, "- {}", notes.join("; "));
    }
}

/// `- in `name: type = default`: doc`.
fn socket_line(out: &mut String, direction: &str, socket: &Socket) {
    let ty = match (&socket.generic, &socket.combine) {
        (Some(generic), _) => generic.to_string(),
        (None, Some(combined)) => {
            let operator = match combined.rule {
                TypeRule::Componentwise => "+",
                TypeRule::Product => "*",
            };
            format!("{} {operator} {}", combined.a, combined.b)
        }
        (None, None) => socket.ty.to_string(),
    };
    let default = match (socket.default, socket.splat_default) {
        (Some(value), _) => format!(" = {value}"),
        (None, Some(value)) => format!(" = {value:?} in every component"),
        (None, None) => String::new(),
    };
    let mut flags = Vec::new();
    if socket.optional {
        flags.push("optional");
    }
    if socket.constant {
        flags.push("a pinned value, never connected");
    }
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!(" ({})", flags.join(", "))
    };
    let _ = writeln!(
        out,
        "- {direction} `{}: {ty}{default}`{flags}{}",
        socket.name,
        described(&socket.doc)
    );
}

/// `: doc`, or nothing.
fn described(doc: &str) -> String {
    let doc = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    if doc.is_empty() {
        doc
    } else {
        format!(": {doc}")
    }
}

fn domain_name(domain: GraphDomain) -> &'static str {
    match domain {
        GraphDomain::Surface => "surface",
        GraphDomain::Screen => "screen",
        GraphDomain::Document => "document",
    }
}
