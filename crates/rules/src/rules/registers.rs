//! `adr/adoption-components`, `adr/deferral-register`, and `adr/tool-register`: registers that must agree with the ADRs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use super::Ctx;
use tabkit_core::finding::Finding;

const COMPONENTS: &str = "adr/adoption-components";
const DEFERRALS: &str = "adr/deferral-register";
const TOOLS: &str = "adr/tool-register";

static ADR_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ADR-(\d{4})").expect("a built-in pattern compiles"));
static ADR_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4})-[a-z0-9-]+\.md$").expect("a built-in pattern compiles"));
static BOLD_CELL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\| \*\*([^*|]+)\*\*").expect("a built-in pattern compiles"));
static ADR_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(\d{4})\]\(adr/(\d{4}-[a-z0-9-]+)\.md\)").expect("a built-in pattern compiles")
});
static MARKUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[`*_]|\[([^\]]*)\]\([^)]*\)").expect("a built-in pattern compiles")
});

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "into", "its", "own", "per", "alone", "then", "service",
    "services", "backend", "plain",
];
const ESCAPE_HATCHES: &[&str] = &["none", "review alone", "build it", "nothing", "any "];

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = components(ctx)?;
    out.extend(deferrals(ctx)?);
    out.extend(tool_register(ctx)?);
    Ok(out)
}

fn one(ctx: &Ctx<'_>, rule: &str, key: &str, default: &str) -> Result<String> {
    Ok(ctx
        .cfg
        .option_str(rule, key)?
        .unwrap_or_else(|| default.to_owned()))
}

/// Every component that the adoption document offers to remove is one that the inventory lists.
fn components(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(COMPONENTS) {
        return Ok(Vec::new());
    }
    let surface = one(ctx, COMPONENTS, "inventory", "docs/operational-surface.md")?;
    let adoption = one(ctx, COMPONENTS, "adoption", "docs/adoption-path.md")?;
    let section = one(ctx, COMPONENTS, "section", "## Band")?;
    let (Some(inventory), Some(path)) = (ctx.repo.read(&surface), ctx.repo.read(&adoption)) else {
        return Ok(Vec::new());
    };
    let inventory = inventory.to_lowercase();
    let mut out = Vec::new();
    let mut in_band = false;
    for (i, line) in path.split('\n').enumerate() {
        if line.starts_with("## ") {
            in_band = line.starts_with(&section);
        }
        if !in_band {
            continue;
        }
        let Some(m) = BOLD_CELL.captures(line) else {
            continue;
        };
        let name = m[1].trim();
        let head = name.split('\'').next().unwrap_or(name);
        let head = head.split(" + ").next().unwrap_or(head);
        if !inventory.contains(&head.to_lowercase()) {
            let message = format!("names `{head}`, which {surface} does not list");
            out.push(Finding::new(&adoption, i + 1, COMPONENTS, message));
        }
    }
    Ok(out)
}

/// Every ADR that states a deferral has a row in the register.
fn deferrals(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(DEFERRALS) {
        return Ok(Vec::new());
    }
    let register = one(
        ctx,
        DEFERRALS,
        "register",
        "docs/reference/deferral-register.md",
    )?;
    let dir = one(ctx, DEFERRALS, "adr-dir", "docs/adr")?;
    let exempt = ctx.cfg.option_strings(DEFERRALS, "exempt", &["0000"])?;
    let Some(body) = ctx.repo.read(&register) else {
        return Ok(Vec::new());
    };
    let registered: BTreeSet<String> = body
        .split('\n')
        .filter(|l| l.starts_with("| "))
        .flat_map(|l| ADR_REF.captures_iter(l).map(|m| m[1].to_owned()))
        .collect();
    let prefix = format!("{dir}/");
    let mut out = Vec::new();
    for path in ctx.files() {
        let Some(num) = path
            .strip_prefix(&prefix)
            .and_then(|n| ADR_FILE.captures(n))
            .map(|m| m[1].to_owned())
        else {
            continue;
        };
        if exempt.contains(&num) || registered.contains(&num) {
            continue;
        }
        if ctx
            .repo
            .read(path)
            .is_some_and(|t| t.contains("| **Trigger** |"))
        {
            let message = format!("states a deferral with no row in {register}");
            out.push(Finding::new(path, 0, DEFERRALS, message));
        }
    }
    Ok(out)
}

struct Row {
    tier: u8,
    tool: String,
    adr: String,
    runner_up: String,
    alternatives: Vec<String>,
    line: usize,
}

fn tool_register(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(TOOLS) {
        return Ok(Vec::new());
    }
    let register = one(ctx, TOOLS, "register", "docs/tool-register.md")?;
    let dir = one(ctx, TOOLS, "adr-dir", "docs/adr")?;
    let Some(text) = ctx.repo.read(&register) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{dir}/");
    let adrs: BTreeMap<&str, String> = ctx
        .repo
        .files
        .iter()
        .filter_map(|f| {
            let name = f.strip_prefix(&prefix)?.strip_suffix(".md")?;
            Some((name, ctx.repo.read(f)?))
        })
        .collect();
    let mut out = Vec::new();
    for row in parse_register(&text) {
        for problem in check_row(&row, &adrs) {
            let message = format!("{}: {problem}", row.tool);
            out.push(Finding::new(&register, row.line, TOOLS, message));
        }
    }
    Ok(out)
}

fn check_row(row: &Row, adrs: &BTreeMap<&str, String>) -> Vec<String> {
    if row.tier == 3 {
        return if row.alternatives.is_empty() {
            vec!["a Tier 3 row names nothing it was picked over".to_owned()]
        } else {
            Vec::new()
        };
    }
    if row.adr.is_empty() {
        return vec!["no owning ADR".to_owned()];
    }
    let Some(body) = adrs.get(row.adr.as_str()) else {
        return vec![format!("owning ADR {} does not exist", row.adr)];
    };
    let Some(options) = considered_options(body) else {
        return vec![format!("ADR {} has no Considered options section", row.adr)];
    };
    let mut out = Vec::new();
    if !options.contains("| ---") {
        out.push(format!(
            "ADR {} Considered options has no comparison table",
            row.adr
        ));
    }
    if row.tier == 1 && row.runner_up.is_empty() {
        out.push("a Tier 1 row has an empty Runner-up cell".to_owned());
    }
    let haystack = normalise(&strip(body));
    for alt in &row.alternatives {
        if alt.is_empty() || is_escape_hatch(alt) || mentions(&haystack, alt) {
            continue;
        }
        out.push(format!(
            "`{alt}` is recorded against it but appears nowhere in ADR {}",
            row.adr
        ));
    }
    out
}

/// One significant word is enough: the register and the ADR can word one option differently.
fn mentions(haystack: &str, alt: &str) -> bool {
    normalise(alt)
        .split_whitespace()
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(w))
        .any(|w| haystack.contains(w))
}

/// Lowercases and turns separators into spaces. `#`, `.`, and `+` stay, because they are part of a name.
fn normalise(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '#' | '.' | '+') {
                c
            } else {
                ' '
            }
        })
        .collect()
}

fn is_escape_hatch(alt: &str) -> bool {
    let lower = alt.to_lowercase();
    ESCAPE_HATCHES.iter().any(|p| lower.starts_with(p))
}

fn considered_options(body: &str) -> Option<&str> {
    let (_, rest) = body.split_once("## Considered options")?;
    Some(
        rest.split_once("\n## ")
            .map_or(rest, |(section, _)| section),
    )
}

fn parse_register(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut tier = 0;
    for (i, line) in text.split('\n').enumerate() {
        if line.starts_with("## ") {
            tier = match line {
                l if l.starts_with("## Tier 1") => 1,
                l if l.starts_with("## Tier 2") => 2,
                l if l.starts_with("## Tier 3") => 3,
                _ => 0,
            };
            continue;
        }
        if tier == 0 || !line.starts_with("| ") {
            continue;
        }
        let cells: Vec<&str> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 3
            || cells[0] == "Tool"
            || cells[0] == "Library"
            || cells[0].starts_with("---")
        {
            continue;
        }
        rows.push(build_row(tier, &cells, i + 1));
    }
    rows
}

fn build_row(tier: u8, cells: &[&str], line: usize) -> Row {
    let mut row = Row {
        tier,
        tool: strip(cells[0]),
        adr: String::new(),
        runner_up: String::new(),
        alternatives: Vec::new(),
        line,
    };
    if tier == 3 {
        row.alternatives = split_list(cells[2]);
        return row;
    }
    if let Some(m) = ADR_LINK.captures(cells[2]) {
        m[2].clone_into(&mut row.adr);
    }
    if tier == 1 && cells.len() >= 7 {
        row.runner_up = strip(cells[5]);
        row.alternatives = split_list(cells[6]);
    }
    if tier == 2 && cells.len() >= 6 {
        row.alternatives = split_list(cells[5]);
    }
    row
}

fn split_list(cell: &str) -> Vec<String> {
    cell.split(',')
        .map(|part| strip(part.trim()))
        .filter(|part| !part.is_empty())
        .collect()
}

/// The text that a reader sees, without emphasis, code ticks, or link targets.
fn strip(text: &str) -> String {
    MARKUP.replace_all(text, "$1").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_register_row_is_read_by_tier() {
        let text = "## Tier 1\n\n| Tool | Concern | ADR | Licence | Governance | Runner-up | Recorded against |\n| --- | --- | --- | --- | --- | --- | --- |\n| `sqlc` | SQL | [0300](adr/0300-data.md) | MIT | One | sqlx | sqlx, GORM |\n\n## Tier 3\n\n| Library | Concern | Picked over |\n| --- | --- | --- |\n| a | b | c |\n";
        let rows = parse_register(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tool, "sqlc");
        assert_eq!(rows[0].adr, "0300-data");
        assert_eq!(rows[0].alternatives, ["sqlx", "GORM"]);
        assert_eq!(rows[1].tier, 3);
    }

    #[test]
    fn an_alternative_matches_on_one_word() {
        let haystack = normalise(".NET (C#) and Traefik");
        assert!(mentions(&haystack, "C#/.NET"));
        assert!(mentions(
            &haystack,
            "Traefik ForwardAuth to a first-party service"
        ));
        assert!(!mentions(&haystack, "Kong"));
        assert!(is_escape_hatch("none: the browser sets it"));
    }
}
