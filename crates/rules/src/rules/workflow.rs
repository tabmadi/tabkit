//! `workflow/thin`: a workflow step calls a `just` recipe, and pipeline logic stays out of YAML.

use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use saphyr::MarkedYaml;

use super::Ctx;
use tabkit_core::finding::Finding;
use tabkit_core::yaml;

const RULE: &str = "workflow/thin";

/// The one command a step may run: a recipe in the dev shell, or in one named dev shell of the flake.
static THIN_STEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^nix develop (\.#[a-z0-9-]+ )?--command just ")
        .expect("a built-in pattern compiles")
});

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(RULE) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(
        RULE,
        "paths",
        &[
            ".github/workflows/*.{yml,yaml}",
            ".forgejo/workflows/*.{yml,yaml}",
            ".github/actions/*/action.{yml,yaml}",
        ],
    )?;
    let exempt = ctx.paths(RULE, "exempt", &["**/check.yml"])?;
    let mut out = Vec::new();
    for path in ctx
        .files()
        .filter(|f| paths.matches(f) && !exempt.matches(f))
    {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        let docs = yaml::load(&text).with_context(|| format!("parse {path}"))?;
        for doc in &docs {
            walk(doc, &mut |script| {
                let first = yaml::content_line(script, &text);
                let body = yaml::string(script).unwrap_or_default();
                for (i, line) in body.split('\n').enumerate() {
                    if let Some(bad) = offending(line) {
                        let message = format!("`{bad}` is logic. Move it into a `just` recipe");
                        out.push(Finding::new(path, first + i, RULE, message));
                    }
                }
            });
        }
    }
    Ok(out)
}

/// Calls `visit` with the script of every step under a `steps` sequence. A reusable workflow can declare an output
/// named `run`, which is a value and not a script.
fn walk<'a>(node: &'a MarkedYaml<'_>, visit: &mut dyn FnMut(&'a MarkedYaml<'_>)) {
    for (key, value) in yaml::pairs(node) {
        if yaml::string(key) == Some("steps") {
            for step in yaml::items(value) {
                for (k, v) in yaml::pairs(step) {
                    if yaml::string(k) == Some("run") && yaml::string(v).is_some() {
                        visit(v);
                    }
                }
            }
        }
    }
    for child in yaml::children(node) {
        walk(child, visit);
    }
}

/// The trimmed line when it is logic, or `None` when a step may hold it.
fn offending(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "set -euo pipefail" {
        return None;
    }
    let command = [r#">> "$GITHUB_OUTPUT""#, r#">> "$GITHUB_ENV""#]
        .iter()
        .find_map(|sink| trimmed.strip_suffix(sink))
        .map_or(trimmed, str::trim);
    (!THIN_STEP.is_match(command)).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::offending;

    #[test]
    fn lines() {
        for allowed in [
            "",
            "# a comment",
            "set -euo pipefail",
            "nix develop --command just ci",
            "nix develop .#e2e --command just e2e",
            r#"nix develop --command just ci-affected >> "$GITHUB_OUTPUT""#,
        ] {
            assert_eq!(offending(allowed), None, "{allowed}");
        }
        for logic in ["curl example.test", "nix develop --command go test ./..."] {
            assert_eq!(offending(logic), Some(logic));
        }
    }
}
