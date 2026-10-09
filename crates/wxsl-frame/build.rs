fn main() {
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        output.join("host.rs"),
        wxsl_core::host::rust_source(&[
            "CameraUniform",
            "LightUniform",
            "SceneUniform",
            "InstanceTransform",
        ]),
    )
    .unwrap();
    std::fs::write(output.join("wxsl_host.h"), wxsl_core::host::c_header()).unwrap();
}
