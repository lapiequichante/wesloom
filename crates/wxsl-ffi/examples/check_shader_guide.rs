//! Check standalone WXSL snippets in a Markdown guide, without a GPU.
use std::{error::Error, path::PathBuf};
use wxsl_render::wgpu::naga;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs/s2-s3-s4-implementation-guide.md"));
    let document = std::fs::read_to_string(&path)?;
    let mut count = 0;
    let mut source = None;
    let mut start = 0;
    for (line, text) in document.lines().enumerate() {
        if text == "```wxsl" {
            if source.is_some() {
                return Err("nested WXSL fence".into());
            }
            source = Some(String::new());
            start = line + 1;
        } else if text == "```" {
            if let Some(code) = source.take() {
                let mut modules = wxsl_lang::Modules::new();
                for (module, source) in wxsl_stdlib::MODULES {
                    modules.insert(*module, *source);
                }
                modules.insert("package::guide::snippet", code);
                let wgsl = wxsl_lang::compile_with_macros(
                    &modules,
                    "package::guide::snippet",
                    &Default::default(),
                )
                .map_err(|error| format!("{}:{start}: {error}", path.display()))?;
                let module = naga::front::wgsl::parse_str(&wgsl).map_err(|error| {
                    format!(
                        "{}:{start}: {}",
                        path.display(),
                        error.emit_to_string(&wgsl)
                    )
                })?;
                naga::valid::Validator::new(
                    naga::valid::ValidationFlags::all(),
                    naga::valid::Capabilities::all(),
                )
                .validate(&module)
                .map_err(|error| format!("{}:{start}: {error}", path.display()))?;
                count += 1;
            }
        } else if let Some(code) = &mut source {
            code.push_str(text);
            code.push('\n');
        }
    }
    if source.is_some() {
        return Err("unclosed WXSL fence".into());
    }
    if count == 0 {
        return Err("guide has no WXSL snippets".into());
    }
    println!(
        "{count} WXSL snippets compile and validate as WGSL ({})",
        path.display()
    );
    Ok(())
}
