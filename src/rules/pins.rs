//! `image/*`, `script/*`, `version/*`, and `just/*`: small line checks over the files that a repository pins or runs.

use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;

use super::Ctx;
use crate::finding::Finding;

static FLOATING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(:latest|:stable|:main)([^A-Za-z0-9_.-]|$)").expect("a built-in pattern compiles")
});
static ACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"uses: [^ ]+@(main|master|develop)\b").expect("a built-in pattern compiles")
});
static BARE_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(echo|printf)(\s+-[a-zA-Z]+)*\s+["'](\s*(✓|✗|⚠|→)|(WARN(ING)?|ERROR|FAIL(ED)?|OK)\b)"#,
    )
    .expect("a built-in pattern compiles")
});
static FLAKE_GO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgo_1_(\d+)\b").expect("a built-in pattern compiles"));
static MOD_GO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^go 1\.(\d+)").expect("a built-in pattern compiles"));
static FLAKE_PYTHON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bpython3(\d{2,})\b").expect("a built-in pattern compiles"));
static PYPROJECT_PYTHON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^requires-python\s*=\s*"[^"0-9]*3\.(\d+)"#)
        .expect("a built-in pattern compiles")
});

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = images(ctx)?;
    out.extend(log_vocabulary(ctx)?);
    out.extend(version_sync(ctx));
    out.extend(just_doc_comments(ctx)?);
    Ok(out)
}

fn images(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    const TAG: &str = "image/floating-tag";
    const USES: &str = "image/unpinned-action";
    let tag_paths = ctx.paths(
        TAG,
        "paths",
        &[
            "**/Dockerfile*",
            "infra/helm/**",
            "infra/local/**",
            ".github/workflows/**",
            ".forgejo/workflows/**",
            "services/**",
            "apps/**",
        ],
    )?;
    let tag_exempt = ctx.paths(TAG, "exempt", &["**/image-refs.txt", "**/node_modules/**"])?;
    let use_paths = ctx.paths(
        USES,
        "paths",
        &[".github/workflows/**", ".forgejo/workflows/**"],
    )?;
    let mut out = Vec::new();
    for path in ctx.files() {
        let tags = ctx.cfg.enabled(TAG) && tag_paths.matches(path) && !tag_exempt.matches(path);
        let uses = ctx.cfg.enabled(USES) && use_paths.matches(path);
        if !tags && !uses {
            continue;
        }
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if tags && let Some(m) = FLOATING.captures(line) {
                let message = format!("floating tag `{}`: pin a version or a digest", &m[1]);
                out.push(Finding::new(path, i + 1, TAG, message));
            }
            if uses && let Some(m) = ACTION.find(line) {
                let message = format!("`{}` floats: pin a version or a SHA", m.as_str());
                out.push(Finding::new(path, i + 1, USES, message));
            }
        }
    }
    Ok(out)
}

fn log_vocabulary(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    const RULE: &str = "script/log-vocabulary";
    if !ctx.cfg.enabled(RULE) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(RULE, "paths", &["scripts/**/*.sh"])?;
    let exempt = ctx.paths(RULE, "exempt", &["scripts/lib/log.sh"])?;
    let mut out = Vec::new();
    for path in ctx
        .files()
        .filter(|f| paths.matches(f) && !exempt.matches(f))
    {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if BARE_STATUS.is_match(line) {
                out.push(Finding::new(
                    path,
                    i + 1,
                    RULE,
                    "bare status output: use the helpers of the log library",
                ));
            }
        }
    }
    Ok(out)
}

/// A version line in the flake, compared with the one in the file of the language.
fn version_sync(ctx: &Ctx<'_>) -> Vec<Finding> {
    const RULE: &str = "version/sync";
    if !ctx.cfg.enabled(RULE) {
        return Vec::new();
    }
    let Some(flake) = ctx.repo.read("flake.nix") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, file, in_flake, in_file) in [
        ("Go", "go.mod", &*FLAKE_GO, &*MOD_GO),
        (
            "Python",
            "pyproject.toml",
            &*FLAKE_PYTHON,
            &*PYPROJECT_PYTHON,
        ),
    ] {
        let Some(text) = ctx.repo.read(file) else {
            continue;
        };
        let (Some(a), Some(b)) = (in_flake.captures(&flake), in_file.captures(&text)) else {
            continue;
        };
        if a[1] != b[1] {
            let major = if name == "Go" { 1 } else { 3 };
            let line = text[..b.get(0).map_or(0, |m| m.start())].lines().count() + 1;
            let message = format!(
                "{name} line {major}.{} differs from {major}.{} in `flake.nix`",
                &b[1], &a[1]
            );
            out.push(Finding::new(file, line, RULE, message));
        }
    }
    out
}

fn just_doc_comments(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    const RULE: &str = "just/doc-comment";
    if !ctx.cfg.enabled(RULE) {
        return Ok(Vec::new());
    }
    let Some(text) = ctx.repo.read("justfile") else {
        return Ok(Vec::new());
    };
    let output = Command::new("just")
        .arg("--justfile")
        .arg(ctx.repo.root.join("justfile"))
        .args(["--dump", "--dump-format", "json"])
        .output();
    // A repository can run tabrules without `just`, and then this rule has no input.
    let output = match output {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other.context("run `just`")?,
    };
    if !output.status.success() {
        bail!(
            "just --dump: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let dump: Value = serde_json::from_slice(&output.stdout).context("parse the `just` dump")?;
    let mut out = Vec::new();
    for (name, recipe) in dump["recipes"].as_object().into_iter().flatten() {
        if recipe["private"].as_bool() == Some(true) || recipe["doc"].is_string() {
            continue;
        }
        let line = text
            .lines()
            .position(|l| {
                l.trim_start_matches('@')
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.starts_with([':', ' ']))
            })
            .map_or(0, |i| i + 1);
        let message = format!("recipe `{name}` has no doc comment");
        out.push(Finding::new("justfile", line, RULE, message));
    }
    Ok(out)
}
