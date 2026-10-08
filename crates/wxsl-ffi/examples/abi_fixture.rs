//! Native Rust variant outputs for the dynamically loaded C/C++ parity harness.

use wxsl_core::{
    abi::MaterialStage,
    graph::Graph,
    macros::{MacroSet, MacroValue},
    material::MaterialConfig,
};
use wxsl_render::{library::ShaderLibrary, material::Material, variants::MaterialRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: abi_fixture <directory>")?,
    );
    let graph: Graph = serde_json::from_str(include_str!("../../wxsl/assets/pbr_cube.wxsl.json"))?;
    let mut macros = MacroSet::new();
    macros.set("WXSL_FBM_OCTAVES", MacroValue::Int(3));
    let config = MaterialConfig::with_macros(macros);
    let material = Material::with_config(&graph, &wxsl_stdlib::registry(), &config)?;
    let mut library = ShaderLibrary::new();
    library.insert_all(wxsl_stdlib::MODULES.iter().copied());
    std::fs::create_dir_all(&dir)?;
    for stage in MaterialStage::ALL {
        let wgsl = MaterialRequest::new(&material, *stage)
            .compile(&library)
            .1?;
        let request =
            serde_json::json!({"graph": graph, "stage": stage.name(), "material": config});
        std::fs::write(
            dir.join(format!("{}.request.json", stage.name())),
            serde_json::to_vec(&request)?,
        )?;
        std::fs::write(dir.join(format!("{}.wgsl", stage.name())), wgsl)?;
    }
    Ok(())
}
