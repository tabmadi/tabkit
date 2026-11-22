//! The files of a repository, listed one way for every gate.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::exec;

const PRUNED: [&str; 5] = [".git", "node_modules", ".next", "dist", "vendor"];

fn walk(dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() && !path.is_symlink() {
            if !PRUNED.contains(&name.as_ref()) {
                walk(&path, out)?;
            }
        } else {
            out.push(
                path.strip_prefix(".")
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    Ok(())
}

/// Tracked and untracked files that git does not ignore. Outside a git tree, it walks the folder without vendored trees.
pub fn repo_files() -> Result<Vec<String>> {
    let mut files = Vec::new();
    if exec::succeeds("git", &["rev-parse", "--is-inside-work-tree"]) {
        for args in [
            &["ls-files", "-z"][..],
            &["ls-files", "-z", "--others", "--exclude-standard"][..],
        ] {
            files.extend(
                exec::output("git", args)?
                    .split('\0')
                    .filter(|f| !f.is_empty())
                    .map(str::to_string),
            );
        }
        files.retain(|f| Path::new(f).symlink_metadata().is_ok());
    } else {
        walk(Path::new("."), &mut files)?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Every shell script of the repository. An empty list is an error, because the listing is then broken.
pub fn shell_files() -> Result<Vec<String>> {
    Ok(repo_files()?
        .into_iter()
        .filter(|f| f.ends_with(".sh"))
        .collect())
}

/// The path `<parent>/<child>/<rest>` for each sorted child folder that holds it.
pub fn under(parent: &str, rest: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(parent)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|entry| entry.path().join(rest)))
                .collect()
        })
        .unwrap_or_default();
    out.retain(|path| path.exists());
    out.sort();
    out
}

/// The name of the child folder of `services/<name>/<file>`.
pub fn service_of(path: &Path) -> String {
    path.components()
        .nth(1)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A folder that is deleted on every exit path.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("tabkit-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_service_of_a_path() {
        assert_eq!(
            service_of(Path::new("services/orders/openapi.yaml")),
            "orders"
        );
    }

    #[test]
    fn a_temp_dir_is_removed() {
        let path = {
            let dir = TempDir::new("test").unwrap();
            assert!(dir.0.is_dir());
            dir.0.clone()
        };
        assert!(!path.exists());
    }
}
