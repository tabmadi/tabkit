//! The bundle of ADR-1300 of tabkit: the Nix closure, the images, and the repository, carried across an air gap.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::files::TempDir;
use super::{detail, exec, ok, step};

const USAGE: &str = "usage: tabkit-ops bundle export <file.tar.zst> --key <nix-signing-key> [--images <list>] [installable...]\n       tabkit-ops bundle import <file.tar.zst> [--registry <host:port>] [--plain-http] [--forge <git-url>]";

/// The full name of an image, with its registry. A name with no registry is on Docker Hub.
fn full_name(reference: &str) -> String {
    let first = reference.split('/').next().unwrap_or_default();
    let has_registry = reference.contains('/')
        && (first.contains('.') || first.contains(':') || first == "localhost");
    if has_registry {
        reference.to_owned()
    } else if reference.contains('/') {
        format!("docker.io/{reference}")
    } else {
        format!("docker.io/library/{reference}")
    }
}

/// The images of a list file: the last field of each line, without comments and blank lines.
fn images(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().last())
        .map(full_name)
    {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].as_str())
}

/// The arguments that are not flags and not flag values.
fn positional<'a>(args: &'a [String], with_value: &[&str], without: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if with_value.contains(&arg.as_str()) {
            skip = true;
        } else if !without.contains(&arg.as_str()) {
            out.push(arg.as_str());
        }
    }
    out
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("the path is not UTF-8")
}

fn export(args: &[String]) -> Result<()> {
    let rest = positional(args, &["--key", "--images"], &[]);
    let Some((file, installables)) = rest.split_first() else {
        bail!(USAGE);
    };
    let key = flag(args, "--key").context("--key names the Nix signing key of the site")?;
    let key = fs::canonicalize(key).with_context(|| format!("cannot read the key {key}"))?;
    let system = exec::output(
        "nix",
        &[
            "eval",
            "--impure",
            "--raw",
            "--expr",
            "builtins.currentSystem",
        ],
    )?;
    let default_shell = format!(".#devShells.{system}.default");
    let installables: Vec<&str> = if installables.is_empty() {
        vec![default_shell.as_str()]
    } else {
        installables.to_vec()
    };
    let work = TempDir::new("bundle")?;
    let cache = format!(
        "file://{}?compression=zstd&secret-key={}",
        path_str(&work.0.join("nix"))?,
        path_str(&key)?
    );

    step("building the Nix paths");
    let mut build = vec!["build", "--no-link", "--max-jobs", "auto"];
    build.extend(&installables);
    exec::status("nix", &build, &[])?;

    step("copying the Nix closure, signed");
    let mut copy = vec!["copy", "--to", cache.as_str()];
    copy.extend(&installables);
    exec::status("nix", &copy, &[])?;
    // The flake inputs, so the site evaluates the flake with no network.
    exec::status("nix", &["flake", "archive", "--to", &cache], &[])?;
    // The derivations of the dev shell, so `nix develop` finds them in the store.
    exec::status(
        "nix",
        &["copy", "--derivation", "--to", &cache, &default_shell],
        &[],
    )?;

    let mut manifest = format!(
        "rev {}\nflake.lock {}\n",
        exec::output("git", &["rev-parse", "HEAD"])?.trim(),
        exec::output("sha256sum", &["flake.lock"])?
            .split_whitespace()
            .next()
            .unwrap_or_default()
    );
    for installable in &installables {
        writeln!(manifest, "nix {installable}")?;
    }

    if let Some(list) = flag(args, "--images") {
        let names =
            images(&fs::read_to_string(list).with_context(|| format!("cannot read {list}"))?);
        step(&format!("copying {} images", names.len()));
        for (index, name) in names.iter().enumerate() {
            let layout = work.0.join("images").join(index.to_string());
            fs::create_dir_all(&layout)?;
            detail(name);
            exec::status(
                "skopeo",
                &[
                    "copy",
                    "-q",
                    "--retry-times",
                    "3",
                    "--multi-arch",
                    "all",
                    "--preserve-digests",
                    &format!("docker://{name}"),
                    &format!("oci:{}:image", path_str(&layout)?),
                ],
                &[],
            )?;
            writeln!(manifest, "image {index} {name}")?;
        }
    }

    step("bundling the repository");
    exec::status(
        "git",
        &[
            "bundle",
            "create",
            path_str(&work.0.join("repo.bundle"))?,
            "--all",
        ],
        &[],
    )?;
    fs::write(work.0.join("manifest.txt"), &manifest)?;

    step(&format!("writing {file}"));
    exec::status(
        "tar",
        &["--zstd", "-cf", file, "-C", path_str(&work.0)?, "."],
        &[],
    )?;
    ok(&format!(
        "{file} holds the bundle of {}",
        manifest.lines().next().unwrap_or_default()
    ));
    Ok(())
}

fn import(args: &[String]) -> Result<()> {
    let rest = positional(args, &["--registry", "--forge"], &["--plain-http"]);
    let [file] = rest.as_slice() else {
        bail!(USAGE);
    };
    let work = TempDir::new("bundle-import")?;
    step(&format!("unpacking {file}"));
    exec::status(
        "tar",
        &["--zstd", "-xf", file, "-C", path_str(&work.0)?],
        &[],
    )?;
    let manifest =
        fs::read_to_string(work.0.join("manifest.txt")).context("the bundle has no manifest")?;

    step("loading the Nix closure. The store checks each signature");
    let cache = format!("file://{}", path_str(&work.0.join("nix"))?);
    exec::status("nix", &["copy", "--from", &cache, "--all"], &[])?;

    let image_lines: Vec<(&str, &str)> = manifest
        .lines()
        .filter_map(|l| l.strip_prefix("image "))
        .filter_map(|l| l.split_once(' '))
        .collect();
    if !image_lines.is_empty() {
        let registry =
            flag(args, "--registry").context("the bundle holds images, so pass --registry")?;
        let tls = if args.iter().any(|a| a == "--plain-http") {
            "--dest-tls-verify=false"
        } else {
            "--dest-tls-verify=true"
        };
        step(&format!(
            "pushing {} images to {registry}",
            image_lines.len()
        ));
        for (index, name) in image_lines {
            let layout: PathBuf = work.0.join("images").join(index);
            detail(name);
            exec::status(
                "skopeo",
                &[
                    "copy",
                    "-q",
                    "--multi-arch",
                    "all",
                    "--preserve-digests",
                    tls,
                    &format!("oci:{}:image", path_str(&layout)?),
                    &format!("docker://{registry}/{name}"),
                ],
                &[],
            )?;
        }
    }

    if let Some(forge) = flag(args, "--forge") {
        step(&format!("pushing the repository to {forge}"));
        let mirror = work.0.join("repo.git");
        exec::status(
            "git",
            &[
                "clone",
                "--quiet",
                "--mirror",
                path_str(&work.0.join("repo.bundle"))?,
                path_str(&mirror)?,
            ],
            &[],
        )?;
        exec::status_in(&mirror, "git", &["push", "--quiet", "--mirror", forge], &[])?;
    }
    ok(&format!(
        "imported {}",
        manifest.lines().next().unwrap_or_default()
    ));
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    match args.split_first() {
        Some((verb, rest)) if verb == "export" => export(rest),
        Some((verb, rest)) if verb == "import" => import(rest),
        _ => bail!(USAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_gets_its_registry() {
        assert_eq!(
            full_name("busybox:stable"),
            "docker.io/library/busybox:stable"
        );
        assert_eq!(
            full_name("alpine/k8s:1.37.2"),
            "docker.io/alpine/k8s:1.37.2"
        );
        assert_eq!(full_name("ghcr.io/a/b@sha256:00"), "ghcr.io/a/b@sha256:00");
        assert_eq!(full_name("localhost:5000/a:1"), "localhost:5000/a:1");
    }

    #[test]
    fn the_list_takes_the_last_field_once() {
        let names = images(
            "# comment\n\nbase alpine/k8s:1.37.2\nfull docker.io/alpine/k8s:1.37.2\nquay.io/x:1\n",
        );
        assert_eq!(names, ["docker.io/alpine/k8s:1.37.2", "quay.io/x:1"]);
    }

    #[test]
    fn the_flags_are_not_installables() {
        let args: Vec<String> = ["out.tar.zst", "--key", "k", ".#a", "--images", "i", ".#b"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            positional(&args, &["--key", "--images"], &[]),
            ["out.tar.zst", ".#a", ".#b"]
        );
        assert_eq!(flag(&args, "--images"), Some("i"));
    }
}
