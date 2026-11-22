//! `tabkit affected`: the JSON manifest of the services, apps, and libraries that a diff touches, for CI.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};
use serde::Serialize;

use tabkit_core::config::Config;

/// The `[rules]` key of this command.
pub const ID: &str = "affected";

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Manifest {
    global: bool,
    services: Vec<String>,
    apps: Vec<String>,
    libs: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    reason: String,
}

/// The paths that decide a global build.
struct Triggers {
    files: Vec<String>,
    prefixes: Vec<String>,
}

pub fn run(root: &Path, cfg: &Config, base: &str, all: bool, runtime: bool) -> Result<String> {
    let triggers = Triggers {
        files: cfg.option_strings(
            ID,
            "global-files",
            &[
                "go.mod",
                "go.sum",
                "go.work",
                "go.work.sum",
                "package.json",
                "bun.lock",
                "bun.nix",
                "flake.nix",
                "flake.lock",
            ],
        )?,
        prefixes: cfg.option_strings(ID, "global-prefixes", &["infra/", "tools/", ".github/"])?,
    };
    let ignored = cfg.option_strings(ID, "runtime-ignore", &["tools/", ".github/", ".forgejo/"])?;
    let mut files = changed_files(root, base)?;
    if runtime {
        files.retain(|f| !ignored.iter().any(|p| f.starts_with(p.as_str())));
    }
    let manifest = classify(&files, all, &triggers);
    Ok(serde_json::to_string_pretty(&manifest)? + "\n")
}

fn changed_files(root: &Path, base: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", &format!("{base}...HEAD")])
        .output()?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

fn classify(files: &[String], all: bool, triggers: &Triggers) -> Manifest {
    if all {
        return Manifest {
            global: true,
            reason: "--all flag".to_owned(),
            ..Manifest::default()
        };
    }
    let (mut services, mut apps, mut libs) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for file in files {
        if triggers.files.contains(file)
            || triggers
                .prefixes
                .iter()
                .any(|p| file.starts_with(p.as_str()))
        {
            return Manifest {
                global: true,
                reason: format!("global trigger: {file}"),
                ..Manifest::default()
            };
        }
        let segment = |n: usize| file.split('/').nth(n).map(str::to_owned);
        if file.starts_with("services/") {
            services.extend(segment(1));
        } else if file.starts_with("apps/") {
            apps.extend(segment(1));
        } else if file.starts_with("libs/go/sdks/") || file.starts_with("libs/ts/sdks/") {
            // A client change affects the consumers of that service, so the service is marked.
            services.extend(segment(3));
        } else if file.starts_with("libs/go/") || file.starts_with("libs/ts/") {
            libs.extend(segment(2));
        }
    }
    Manifest {
        global: false,
        services: services.into_iter().collect(),
        apps: apps.into_iter().collect(),
        libs: libs.into_iter().collect(),
        reason: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triggers() -> Triggers {
        Triggers {
            files: vec!["go.mod".to_owned()],
            prefixes: vec!["infra/".to_owned()],
        }
    }

    fn files(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn paths_map_to_scopes() {
        let m = classify(
            &files(&[
                "services/orders/main.go",
                "apps/frontend/a.ts",
                "libs/go/sdks/billing/client.go",
                "libs/ts/money/a.ts",
                "docs/a.md",
            ]),
            false,
            &triggers(),
        );
        assert_eq!(m.services, ["billing", "orders"]);
        assert_eq!(m.apps, ["frontend"]);
        assert_eq!(m.libs, ["money"]);
        assert!(!m.global);
    }

    #[test]
    fn a_global_trigger_clears_the_scopes() {
        let m = classify(
            &files(&["services/orders/a.go", "infra/x.yaml"]),
            false,
            &triggers(),
        );
        assert!(m.global);
        assert_eq!(m.reason, "global trigger: infra/x.yaml");
        assert_eq!(m.services, Vec::<String>::new());
    }

    #[test]
    fn all_forces_global() {
        assert_eq!(classify(&[], true, &triggers()).reason, "--all flag");
    }

    #[test]
    fn the_json_shape_is_stable() {
        let text = serde_json::to_string(&classify(&[], false, &triggers())).unwrap();
        assert_eq!(
            text,
            r#"{"global":false,"services":[],"apps":[],"libs":[]}"#
        );
    }
}
