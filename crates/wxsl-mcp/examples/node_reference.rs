//! Write `docs/reference/nodes.md`, the node reference, from the shipped
//! registries (plan6 M2). `tests/node_reference.rs` fails until this has
//! been run after a node changes.
//!
//! ```sh
//! cargo run -p wxsl-mcp --example node_reference
//! ```

use std::path::Path;

use wxsl_render::effect::EffectRegistry;

fn main() -> std::io::Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/nodes.md");
    let reference = wxsl_mcp::node_reference(
        &wxsl_stdlib::registry(),
        &wxsl_render::document_registry(&EffectRegistry::shipped()),
    );
    std::fs::write(&path, reference)?;
    println!("wrote {}", path.display());
    Ok(())
}
