//! `docs/reference/nodes.md` is what the generator makes of the shipped
//! registries (plan6 M2). No device.

use std::path::Path;

use wxsl_render::effect::EffectRegistry;

#[test]
fn the_published_node_reference_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/nodes.md");
    let generated = wxsl_mcp::node_reference(
        &wxsl_stdlib::registry(),
        &wxsl_render::document_registry(&EffectRegistry::shipped()),
    );
    let published = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        published == generated,
        "{} is out of date: run `cargo run -p wxsl-mcp --example node_reference`",
        path.display()
    );
}

#[test]
fn every_node_and_socket_is_in_the_reference() {
    let registry = wxsl_stdlib::registry();
    let reference = wxsl_mcp::node_reference(&registry, &wxsl_core::node::NodeRegistry::new());
    for definition in registry.iter() {
        let heading = format!("#### `{}` · ", definition.id);
        let at = reference
            .find(&heading)
            .unwrap_or_else(|| panic!("{} has no entry", definition.id));
        let entry = &reference[at + heading.len()..];
        let entry = &entry[..entry.find("\n#### ").unwrap_or(entry.len())];
        for socket in definition.inputs.iter().chain(&definition.outputs) {
            assert!(
                entry.contains(&format!("`{}: ", socket.name)),
                "{}: socket `{}` is missing",
                definition.id,
                socket.name
            );
        }
    }
}
