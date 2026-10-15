//! The project root an agent's paths are confined to (ADR 0083).
//!
//! Every path a tool takes is resolved here before the editor sees it,
//! including the paths inside the raw commands `graph_edit` accepts. A
//! path that leaves the root is refused, whether it gets out through `..`,
//! through a symbolic link, or because it is absolute.

use std::io;
use std::path::{Component, Path, PathBuf};

use wxsl_editor::Command;

/// A directory, and everything under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root(PathBuf);

impl Root {
    /// Confine paths to `path`, which must exist.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Root(path.as_ref().canonicalize()?))
    }

    /// The directory, canonical.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Resolve `path`, relative to the root unless it is absolute, and
    /// refuse it if it is not under the root. A file that does not exist
    /// yet, which a save creates, is resolved through its directory.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let outside = || format!("`{path}` is outside the project root {}", self.0.display());
        let joined = self.0.join(path);
        let resolved = match joined.canonicalize() {
            Ok(resolved) => resolved,
            Err(_) => {
                let name = joined
                    .file_name()
                    .filter(|_| {
                        matches!(joined.components().next_back(), Some(Component::Normal(_)))
                    })
                    .ok_or_else(outside)?;
                let parent = joined.parent().ok_or_else(outside)?;
                let parent = parent
                    .canonicalize()
                    .map_err(|error| format!("`{path}`: {error}"))?;
                parent.join(name)
            }
        };
        if resolved.starts_with(&self.0) {
            Ok(resolved)
        } else {
            Err(outside())
        }
    }

    /// Resolve every path `command` names, in place, batches included.
    pub(crate) fn confine(&self, command: &mut Command) -> Result<(), String> {
        match command {
            Command::OpenScene { path }
            | Command::ImportGltf { path }
            | Command::SaveScene { path } => {
                *path = self.resolve(path)?.display().to_string();
            }
            Command::Batch { commands } => {
                for command in commands {
                    self.confine(command)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> (PathBuf, Root) {
        let dir = std::env::temp_dir().join(format!("wxsl-mcp-{test}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("scenes")).unwrap();
        std::fs::write(dir.join("scenes/a.scene.json"), "{}").unwrap();
        let root = Root::new(dir.join("scenes")).unwrap();
        (dir, root)
    }

    #[test]
    fn paths_under_the_root_resolve_and_the_rest_are_refused() {
        let (dir, root) = scratch("resolve");
        assert_eq!(
            root.resolve("a.scene.json").unwrap(),
            root.path().join("a.scene.json")
        );
        assert_eq!(
            root.resolve("new.scene.json").unwrap(),
            root.path().join("new.scene.json"),
            "a file a save creates resolves through its directory"
        );
        for escape in [
            "../outside.json",
            "../../etc/passwd",
            "/etc/passwd",
            "..",
            "missing/dir/file.json",
        ] {
            assert!(root.resolve(escape).is_err(), "{escape} resolved");
        }
        let absolute = root.path().join("a.scene.json").display().to_string();
        assert!(root.resolve(&absolute).is_ok(), "an absolute path inside");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paths_inside_batches_are_confined() {
        let (dir, root) = scratch("batch");
        let mut command = Command::Batch {
            commands: vec![Command::OpenScene {
                path: "a.scene.json".into(),
            }],
        };
        root.confine(&mut command).unwrap();
        let Command::Batch { commands } = &command else {
            unreachable!()
        };
        assert_eq!(
            commands[0],
            Command::OpenScene {
                path: root.path().join("a.scene.json").display().to_string()
            }
        );
        let mut escape = Command::Batch {
            commands: vec![Command::SaveScene {
                path: "../x.scene.json".into(),
            }],
        };
        assert!(root.confine(&mut escape).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
