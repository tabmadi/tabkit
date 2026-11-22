//! The platform commands. Each command is a script embedded in the binary, written once to a cache that its content names.

pub mod native;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// The name of each command: a top-level script, without its extension.
pub fn commands() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = FILES
        .iter()
        .filter_map(|(name, _)| name.strip_suffix(".sh"))
        .filter(|name| !name.contains('/'))
        .chain(native::COMMANDS.iter().copied())
        .collect();
    names.sort_unstable();
    names
}

/// FNV-1a over the names and contents. It names a cache folder, and it is not a security control.
fn digest() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for (name, body) in FILES {
        for byte in name.as_bytes().iter().chain(body.iter()) {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// The cache root: `$XDG_CACHE_HOME`, then `$HOME/.cache`, then the system temp folder.
pub fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("tabkit/ops")
}

/// Writes the scripts under `root` once, and returns their folder. A rename makes the write atomic for parallel runs.
pub fn materialize(root: &Path) -> io::Result<PathBuf> {
    let dir = root.join(format!("{}-{:016x}", env!("CARGO_PKG_VERSION"), digest()));
    if dir.is_dir() {
        return Ok(dir);
    }
    fs::create_dir_all(root)?;
    let tmp = root.join(format!(".tmp-{}", std::process::id()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    for (name, body) in FILES {
        let path = tmp.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, body)?;
    }
    match fs::rename(&tmp, &dir) {
        Ok(()) => Ok(dir),
        Err(_) if dir.is_dir() => {
            fs::remove_dir_all(&tmp)?;
            Ok(dir)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_hold_no_library_file() {
        let names = commands();
        assert!(names.contains(&"cluster"));
        assert!(!names.iter().any(|name| name.contains("lib/")));
    }

    #[test]
    fn materialize_writes_every_file_once() {
        let root = std::env::temp_dir().join(format!("tabkit-ops-test-{}", std::process::id()));
        let first = materialize(&root).expect("the first write works");
        assert!(first.join("lib/bootstrap.sh").is_file());
        assert_eq!(materialize(&root).expect("the second call works"), first);
        fs::remove_dir_all(root).expect("the test folder is removable");
    }
}
