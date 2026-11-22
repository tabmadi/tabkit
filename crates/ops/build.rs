//! Embeds each file of `scripts/` in the binary, so a project carries no copy of them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the scripts directory is readable") {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let root =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it")).join("scripts");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let mut code = String::from("pub const FILES: &[(&str, &[u8])] = &[\n");
    for file in &files {
        let name = file
            .strip_prefix(&root)
            .expect("under the root")
            .display()
            .to_string();
        writeln!(
            code,
            "    ({name:?}, include_bytes!({:?})),",
            file.display().to_string()
        )
        .expect("a string write");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it")).join("embedded.rs");
    std::fs::write(out, code).expect("the output directory is writable");
}
