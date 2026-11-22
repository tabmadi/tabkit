//! `gen/build-path`: the table of every recipe that a workflow, a check, or a hook reaches, with the doc comment of the
//! recipe and where it runs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;

use super::Output;
use tabkit_core::config::{Config, Paths};
use tabkit_core::repo::Repo;

const ID: &str = "gen/build-path";
const BEGIN: &str = "<!-- machinery:begin -->";
const END: &str = "<!-- machinery:end -->";

static WORKFLOW_RECIPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"just ([a-z][a-z0-9-]*)").expect("a built-in pattern compiles"));
static FLAKE_GATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#".*gate "[a-z0-9-]+" "([a-z0-9 -]+)".*"#).expect("a built-in pattern compiles")
});

pub fn run(repo: &Repo, cfg: &Config) -> Result<Vec<Output>> {
    let Some(doc_path) = cfg.option_str(ID, "doc")? else {
        return Ok(Vec::new());
    };
    let doc = repo
        .read(&doc_path)
        .with_context(|| format!("read {doc_path}"))?;
    if !doc.lines().any(|l| l == BEGIN) {
        bail!("{doc_path} has no {BEGIN} marker");
    }
    let output = Command::new("just")
        .arg("--justfile")
        .arg(repo.root.join("justfile"))
        .args(["--dump", "--dump-format", "json"])
        .output()
        .context("run `just`")?;
    if !output.status.success() {
        bail!(
            "just --dump: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let dump: Value = serde_json::from_slice(&output.stdout).context("parse the `just` dump")?;
    let recipes = dump["recipes"]
        .as_object()
        .context("the `just` dump has no recipes")?;

    let workflows = Paths::new(&cfg.option_strings(
        ID,
        "workflows",
        &[".github/workflows/*.yml", ".forgejo/workflows/*.yml"],
    )?)?;
    let exempt = Paths::new(&cfg.option_strings(ID, "exempt", &["**/template.yml"])?)?;
    let mut roots: BTreeSet<(String, String)> = BTreeSet::new();
    for path in repo
        .files
        .iter()
        .filter(|f| workflows.matches(f) && !exempt.matches(f))
    {
        let Some(text) = repo.read(path) else {
            continue;
        };
        let file = path.rsplit('/').next().unwrap_or(path);
        for m in WORKFLOW_RECIPE.captures_iter(&text) {
            roots.insert((file.to_owned(), m[1].to_owned()));
        }
    }
    if let Some(flake) = repo.read("flake.nix") {
        for line in flake.lines() {
            if let Some(m) = FLAKE_GATE.captures(line) {
                for recipe in m[1].split_whitespace() {
                    roots.insert(("flake.nix".to_owned(), recipe.to_owned()));
                }
            }
        }
    }
    roots.insert(("git hooks".to_owned(), "ci".to_owned()));

    let mut reached: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (file, recipe) in &roots {
        if recipes.contains_key(recipe) {
            reach(recipe, recipes, file, &mut reached);
        }
    }
    let mut rows = String::from("| Recipe | Owns | Reached from |\n| --- | --- | --- |\n");
    let mut missing = Vec::new();
    for (name, from) in &reached {
        let recipe = &recipes[name];
        let has_body = recipe["body"].as_array().is_some_and(|b| !b.is_empty());
        if !has_body || recipe["private"].as_bool() == Some(true) {
            continue;
        }
        let owns = recipe["doc"].as_str().unwrap_or_default();
        if owns.is_empty() {
            missing.push(name.as_str());
        }
        let from: Vec<&str> = from.iter().map(String::as_str).collect();
        let _ = writeln!(
            rows,
            "| `{name}` | {} | {} |",
            owns.replace('|', r"\|"),
            from.join(", ")
        );
    }
    if !missing.is_empty() {
        bail!(
            "recipes that a workflow, a check, or a hook reaches have no doc comment: {}",
            missing.join(", ")
        );
    }
    Ok(vec![Output {
        path: doc_path,
        bytes: splice(&doc, rows.trim_end()).into_bytes(),
    }])
}

fn reach(
    name: &str,
    recipes: &serde_json::Map<String, Value>,
    file: &str,
    reached: &mut BTreeMap<String, BTreeSet<String>>,
) {
    if !reached
        .entry(name.to_owned())
        .or_default()
        .insert(file.to_owned())
    {
        return;
    }
    let Some(dependencies) = recipes.get(name).and_then(|r| r["dependencies"].as_array()) else {
        return;
    };
    for dependency in dependencies {
        if let Some(next) = dependency["recipe"].as_str() {
            reach(next, recipes, file, reached);
        }
    }
}

/// Replaces the lines between the markers with a blank line, the table, and a blank line.
fn splice(doc: &str, table: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in doc.lines() {
        if line == BEGIN {
            let _ = write!(out, "{line}\n\n{table}\n\n");
            skip = true;
            continue;
        }
        if line == END {
            skip = false;
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_keeps_the_text_around_the_markers() {
        let doc = format!("# A\n\n{BEGIN}\n\nold\n\n{END}\n\nend\n");
        assert_eq!(
            splice(&doc, "new"),
            format!("# A\n\n{BEGIN}\n\nnew\n\n{END}\n\nend\n")
        );
    }

    #[test]
    fn reach_follows_dependencies() {
        let dump: Value = serde_json::json!({
            "ci": {"dependencies": [{"recipe": "lint"}]},
            "lint": {"dependencies": []},
        });
        let mut reached = BTreeMap::new();
        reach("ci", dump.as_object().unwrap(), "check.yml", &mut reached);
        assert!(reached["lint"].contains("check.yml"));
    }
}
