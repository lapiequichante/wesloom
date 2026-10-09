fn main() {
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for name in ["Vertex", "UiInstance", "GpuEdge", "GpuJob", "UiViewport"] {
        let structure = wxsl_core::host::structures()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap();
        let mut source = wxsl_core::host::rust_source(&[name]);
        if structure.vertex {
            source.push_str(&format!(
                "impl {name} {{ const ATTRIBUTES: [wgpu::VertexAttribute; {}] = [",
                structure.fields.len()
            ));
            for (index, offset) in structure.offsets().iter().enumerate() {
                source.push_str(&format!("wgpu::VertexAttribute {{ format: wgpu::VertexFormat::{}, offset: {offset}, shader_location: {index} }},", structure.vertex_format(index)));
            }
            source.push_str("]; }\n");
        }
        std::fs::write(output.join(format!("{name}.rs")), source).unwrap();
    }
}
