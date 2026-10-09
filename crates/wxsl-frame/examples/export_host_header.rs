fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: export_host_header <output.h>")?;
    std::fs::write(path, wxsl_frame::HOST_HEADER)?;
    Ok(())
}
