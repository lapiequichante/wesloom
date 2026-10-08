//! Export the build-generated header for CMake/installation.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: export_header <output.h>")?;
    std::fs::write(path, wxsl_ffi::C_HEADER)?;
    Ok(())
}
