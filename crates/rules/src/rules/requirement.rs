//! Behaviour requirements: EARS sentences with IDs in `requirements.md`, each cited by a test. The rules run only
//! when the repository holds a requirements file.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use super::Ctx;
use tabkit_core::config::Paths;
use tabkit_core::finding::Finding;

static PREFIX_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"^- \*\*Prefix:\*\* (\S+)\s*$"));
static VALID_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Z]{2,4}$"));
/// A row whose first cell looks like an ID is a requirement. The header and delimiter rows do not match.
static ROW: LazyLock<Regex> = LazyLock::new(|| re(r"^\| ([A-Z]{2,4}-\d{3}) \| (.+) \|\s*$"));
static ID_SHAPED_ROW: LazyLock<Regex> = LazyLock::new(|| re(r"^\| *[A-Z]+-\d+ *\|"));
/// A citation is an ID-shaped word in a test file. Only a declared prefix counts, so `SHA-256` is not a citation.
static CITATION: LazyLock<Regex> = LazyLock::new(|| re(r"\b([A-Z]{2,4})-(\d{3})\b"));
static EARS: LazyLock<Regex> = LazyLock::new(|| re(r"^(The|When|While|If|Where) "));

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

struct Requirement<'a> {
    file: &'a str,
    line: usize,
    template: bool,
}

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let cfg = &ctx.cfg.requirements;
    let files = Paths::new(&cfg.files)?;
    let templates = Paths::new(&cfg.template_files)?;
    let mut out = Vec::new();
    let mut prefixes: BTreeMap<String, &str> = BTreeMap::new();
    let mut reqs: BTreeMap<String, Requirement<'_>> = BTreeMap::new();

    for path in ctx
        .files()
        .filter(|f| files.matches(f) && !f.ends_with("/README.md") && *f != "README.md")
    {
        let template = templates.matches(path);
        check_file(ctx, path, template, &mut prefixes, &mut reqs, &mut out);
    }
    if !prefixes.is_empty() {
        check_trace(ctx, &prefixes, &reqs, &mut out)?;
    }
    Ok(out)
}

/// Reads one requirements file into the shared maps.
fn check_file<'a>(
    ctx: &Ctx<'_>,
    path: &'a str,
    template: bool,
    prefixes: &mut BTreeMap<String, &'a str>,
    reqs: &mut BTreeMap<String, Requirement<'a>>,
    out: &mut Vec<Finding>,
) {
    let Some(text) = ctx.repo.read(path) else {
        return;
    };
    let mut prefix: Option<String> = None;
    let mut count = 0;
    for (n, line) in text.lines().enumerate() {
        let at = n + 1;
        if prefix.is_none()
            && let Some(m) = PREFIX_LINE.captures(line)
        {
            let p = m[1].to_owned();
            check_prefix(ctx, path, at, &p, template, prefixes, out);
            prefix = Some(p);
            continue;
        }
        let Some(row) = ROW.captures(line) else {
            if ID_SHAPED_ROW.is_match(line) {
                out.push(Finding::new(
                    path,
                    at,
                    "requirement/row",
                    "write a requirement as one row: `| ABC-001 | sentence |`",
                ));
            }
            continue;
        };
        count += 1;
        let id = row[1].to_owned();
        let sentence = row[2].trim();
        if !prefix
            .as_deref()
            .is_some_and(|p| id.starts_with(&format!("{p}-")))
        {
            out.push(Finding::new(
                path,
                at,
                "requirement/row",
                format!(
                    "{id} does not use the prefix of the file: `{}`",
                    prefix.as_deref().unwrap_or("none")
                ),
            ));
        }
        if sentence.contains(" | ") {
            out.push(Finding::new(
                path,
                at,
                "requirement/row",
                format!("{id} has more than one sentence cell"),
            ));
        }
        if !EARS.is_match(sentence) {
            out.push(Finding::new(
                path,
                at,
                "requirement/ears",
                format!("{id} does not open with an EARS keyword: The, When, While, If, or Where"),
            ));
        }
        if let Some(earlier) = reqs.get(&id) {
            out.push(Finding::new(
                path,
                at,
                "requirement/row",
                format!(
                    "{id} is already defined at {}:{}",
                    earlier.file, earlier.line
                ),
            ));
            continue;
        }
        reqs.insert(
            id,
            Requirement {
                file: path,
                line: at,
                template,
            },
        );
    }
    if prefix.is_none() {
        out.push(Finding::new(
            path,
            0,
            "requirement/prefix",
            "the header has no `- **Prefix:**` line",
        ));
    }
    if count == 0 {
        out.push(Finding::new(
            path,
            0,
            "requirement/empty",
            "the file holds no requirement",
        ));
    }
}

/// Every requirement has a citing test, and every ID that a test cites exists.
fn check_trace(
    ctx: &Ctx<'_>,
    prefixes: &BTreeMap<String, &str>,
    reqs: &BTreeMap<String, Requirement<'_>>,
    out: &mut Vec<Finding>,
) -> Result<()> {
    let cfg = &ctx.cfg.requirements;
    let tests = Paths::new(&cfg.tests)?;

    let mut cited: BTreeSet<String> = BTreeSet::new();
    for path in ctx.files().filter(|f| tests.matches(f)) {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            for m in CITATION.captures_iter(line) {
                let prefix = &m[1];
                if !prefixes.contains_key(prefix) || prefix == cfg.template_prefix {
                    continue;
                }
                let id = m[0].to_owned();
                if !reqs.contains_key(&id) {
                    out.push(Finding::new(
                        path,
                        n + 1,
                        "requirement/unknown",
                        format!("the test cites {id}, which no requirements file defines"),
                    ));
                }
                cited.insert(id);
            }
        }
    }
    for (id, req) in reqs {
        if !req.template && !cited.contains(id) {
            out.push(Finding::new(
                req.file,
                req.line,
                "requirement/untested",
                format!(
                    "{id} has no test that cites it. Write the test, or delete the requirement"
                ),
            ));
        }
    }
    Ok(())
}

fn check_prefix<'a>(
    ctx: &Ctx<'_>,
    path: &'a str,
    line: usize,
    prefix: &str,
    template: bool,
    prefixes: &mut BTreeMap<String, &'a str>,
    out: &mut Vec<Finding>,
) {
    let mut bad =
        |message: String| out.push(Finding::new(path, line, "requirement/prefix", message));
    if !VALID_PREFIX.is_match(prefix) {
        bad(format!(
            "the prefix `{prefix}` is not two to four capital letters"
        ));
    }
    if prefix == ctx.cfg.requirements.template_prefix && !template {
        bad(format!(
            "the file uses the template prefix `{prefix}`. Choose its own prefix"
        ));
    }
    if let Some(other) = prefixes.get(prefix) {
        bad(format!(
            "the prefix `{prefix}` is already declared by {other}"
        ));
    } else {
        prefixes.insert(prefix.to_owned(), path);
    }
}
