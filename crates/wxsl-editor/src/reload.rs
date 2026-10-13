//! Shader hot reload (plan3 N9): which files on disk are which library
//! modules, and which of them changed since the last look.
//!
//! The library is embedded (`wxsl_stdlib::MODULES`), so the watch maps a
//! file back to its module by path — `root/color/luminance.wxsl` is
//! `package::color::luminance` — and keeps only the files whose text *is*
//! that module at startup. A file the build script rewrites before
//! embedding (the ABI's generated bindings) differs from its module and is
//! left out, rather than reloaded as an unexpanded template.
//!
//! Polling modification times rather than an OS watcher: no dependency,
//! and a few hundred `stat`s twice a second is nothing. Pure filesystem
//! work, no device, so it is tested here.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use wxsl_render::library::ShaderLibrary;

/// One watched file.
struct Watched {
    path: PathBuf,
    module: String,
    modified: Option<SystemTime>,
    source: String,
}

/// The `.wxsl` files under one root that are library modules verbatim.
pub struct ShaderWatch {
    files: Vec<Watched>,
}

/// One changed module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleChange {
    /// The module path the source replaces.
    pub module: String,
    /// The file it was read from.
    pub path: PathBuf,
    /// The new text.
    pub source: String,
}

impl ShaderWatch {
    /// Watch every file under `root` whose text is the module of
    /// `library` its path names.
    pub fn new(root: &Path, library: &ShaderLibrary) -> Self {
        let mut found = Vec::new();
        collect(root, root, &mut found);
        let files = found
            .into_iter()
            .filter_map(|(path, module)| {
                let source = std::fs::read_to_string(&path).ok()?;
                (library.get(&module) == Some(source.as_str())).then(|| Watched {
                    modified: modified(&path),
                    path,
                    module,
                    source,
                })
            })
            .collect();
        ShaderWatch { files }
    }

    /// How many files are watched.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether nothing is watched — a wrong root, most likely.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The files whose text changed since the last call. A touched file
    /// whose text is the same is not a change.
    pub fn poll(&mut self) -> Vec<ModuleChange> {
        let mut changes = Vec::new();
        for file in &mut self.files {
            let now = modified(&file.path);
            if now == file.modified {
                continue;
            }
            file.modified = now;
            // Mid-save, or deleted: try again at the next change.
            let Ok(source) = std::fs::read_to_string(&file.path) else {
                continue;
            };
            if source != file.source {
                file.source.clone_from(&source);
                changes.push(ModuleChange {
                    module: file.module.clone(),
                    path: file.path.clone(),
                    source,
                });
            }
        }
        changes
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// Every `.wxsl` file under `directory`, with the module path its place
/// under `root` names.
fn collect(root: &Path, directory: &Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "wxsl") {
            continue;
        }
        let Ok(relative) = path
            .with_extension("")
            .strip_prefix(root)
            .map(Path::to_path_buf)
        else {
            continue;
        };
        let parts: Option<Vec<&str>> = relative.iter().map(|part| part.to_str()).collect();
        if let Some(parts) = parts {
            out.push((path, format!("package::{}", parts.join("::"))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wxsl-reload-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("color")).unwrap();
        dir
    }

    /// Rewrite with a modification time the filesystem cannot round to
    /// the previous one.
    fn rewrite(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    #[test]
    fn a_verbatim_module_is_watched_and_its_edit_reported() {
        let root = scratch("edit");
        let file = root.join("color").join("luminance.wxsl");
        std::fs::write(&file, "fn a() {}").unwrap();
        // Same path, different text: generated before embedding.
        std::fs::write(root.join("color").join("generated.wxsl"), "template").unwrap();
        let mut library = ShaderLibrary::new();
        library.insert("package::color::luminance", "fn a() {}");
        library.insert("package::color::generated", "expanded");

        let mut watch = ShaderWatch::new(&root, &library);
        assert_eq!(watch.len(), 1);
        assert!(watch.poll().is_empty(), "nothing changed yet");

        rewrite(&file, "fn b() {}");
        assert_eq!(
            watch.poll(),
            vec![ModuleChange {
                module: "package::color::luminance".into(),
                path: file.clone(),
                source: "fn b() {}".into(),
            }]
        );
        assert!(watch.poll().is_empty(), "reported once");

        rewrite(&file, "fn b() {}");
        assert!(watch.poll().is_empty(), "a touch is not a change");
        let _ = std::fs::remove_dir_all(&root);
    }
}
