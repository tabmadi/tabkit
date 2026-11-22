//! `docs/stack-table`, `docs/principles`, and `docs/headcount`: documents that repeat the ADR set by design.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use super::Ctx;
use crate::finding::Finding;

const STACK: &str = "docs/stack-table";
const PRINCIPLES: &str = "docs/principles";
const HEADCOUNT: &str = "docs/headcount";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

static ADR_REF: LazyLock<Regex> = LazyLock::new(|| re(r"ADR-(\d{4})|\]\(docs/adr/(\d{4})-"));
static ADR_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{4})-[a-z0-9-]+\.md$"));
static HEADCOUNT_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(\d+|two|three|four|five|six|seven|eight|nine|ten)\s*([\x{2013}\x{2014}-]|\bto\b)?\s*(\d+|two|three|four|five)?\s+(platform\s+)?(engineers?|people|persons?|FTEs?|staff)\b",
    )
});
static SINGULAR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(1|one)\b"));
static LINK_TEXT: LazyLock<Regex> = LazyLock::new(|| re(r"\[([^\]]*)\]\([^)]*\)"));
static INLINE_TAG: LazyLock<Regex> = LazyLock::new(|| re(r"[`*]"));
static PRINCIPLE_HEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^\*\*(\d+)\.\s"));
static PRINCIPLE_ROW: LazyLock<Regex> = LazyLock::new(|| re(r"^\|\s*(\d+)\s*\|"));
static LINK_TARGET: LazyLock<Regex> = LazyLock::new(|| re(r"\]\(([^)]+)\)"));
static LOCAL_MARKER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\*\*local\*\*"));
static PARENTHETICS: LazyLock<Regex> = LazyLock::new(|| re(r"\([^)]*\)"));

/// Option-cell openings that hold no product name.
const STOP_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "one",
    "two",
    "no",
    "none",
    "do",
    "per",
    "build",
    "write",
    "keep",
    "everything",
    "paths",
    "anything",
    "each",
    "make",
    "plain",
    "custom",
    "rule",
    "requests",
    "vertical",
    "declarative",
    "generated",
    "hand",
    "self",
    "email",
    "chat",
    "scripts",
    "separate",
    "same",
    "all",
    "in",
    "on",
    "at",
    "with",
    "without",
    "second",
    "third",
    "first",
    "flat",
    "shared",
    "manual",
];

#[derive(Default)]
struct Principle {
    local: bool,
    sources: BTreeSet<String>,
    rejected: Vec<String>,
    accepted: Vec<String>,
}

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = headcount(ctx)?;
    let readme_path = ctx
        .cfg
        .option_str(STACK, "readme")?
        .unwrap_or_else(|| "README.md".to_owned());
    let foundations_path = ctx
        .cfg
        .option_str(PRINCIPLES, "foundations")?
        .unwrap_or_else(|| "docs/adr/0000-platform-foundations.md".to_owned());
    let dir = ctx
        .cfg
        .option_str(PRINCIPLES, "adr-dir")?
        .unwrap_or_else(|| "docs/adr".to_owned());
    let prefix = format!("{dir}/");
    let adrs: Vec<(String, String)> = ctx
        .repo
        .files
        .iter()
        .filter_map(|f| {
            let num = ADR_FILE.captures(f.strip_prefix(&prefix)?)?[1].to_owned();
            Some((num, ctx.repo.read(f)?))
        })
        .collect();
    let readme = ctx.repo.read(&readme_path);
    if ctx.cfg.enabled(STACK)
        && let Some(text) = &readme
    {
        let rejected: BTreeMap<&str, Vec<String>> = adrs
            .iter()
            .map(|(num, body)| (num.as_str(), rejected_in(body)))
            .collect();
        out.extend(stack_table(&readme_path, text, &rejected));
    }
    if ctx.cfg.enabled(PRINCIPLES)
        && let (Some(text), Some(foundations)) = (&readme, ctx.repo.read(&foundations_path))
    {
        let in_adr = principles_in_adr(&foundations);
        let in_readme = principles_in_readme(text);
        out.extend(principle_blocks(&foundations_path, &in_adr, &adrs));
        out.extend(anchors(
            &readme_path,
            &foundations_path,
            &in_adr,
            &in_readme,
        ));
    }
    Ok(out)
}

fn principles_in_adr(body: &str) -> BTreeMap<String, Principle> {
    let mut out: BTreeMap<String, Principle> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in body.split('\n') {
        if let Some(head) = PRINCIPLE_HEAD.captures(line) {
            current = Some(head[1].to_owned());
            out.insert(head[1].to_owned(), Principle::default());
            continue;
        }
        let Some(p) = current.as_ref().and_then(|c| out.get_mut(c)) else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("- *Anchor:*") {
            p.sources = external_sources(rest);
            p.local = LOCAL_MARKER.is_match(rest) && p.sources.is_empty();
        } else if let Some(rest) = line.strip_prefix("- *Rejected:*") {
            p.rejected = names_in(rest);
        } else if let Some(rest) = line.strip_prefix("- *Accepted on the same rule:*") {
            p.accepted = names_in(rest);
        }
    }
    out
}

fn principles_in_readme(body: &str) -> BTreeMap<String, Principle> {
    let mut out = BTreeMap::new();
    for line in body.split('\n') {
        let Some(m) = PRINCIPLE_ROW.captures(line) else {
            continue;
        };
        let cells = split_row(line);
        if cells.len() < 4 {
            continue;
        }
        let sources = external_sources(&cells[2]);
        let local = LOCAL_MARKER.is_match(&cells[2]) && sources.is_empty();
        out.insert(
            m[1].to_owned(),
            Principle {
                local,
                sources,
                rejected: names_in(&cells[3]),
                accepted: Vec::new(),
            },
        );
    }
    out
}

fn principle_blocks(
    path: &str,
    principles: &BTreeMap<String, Principle>,
    adrs: &[(String, String)],
) -> Vec<Finding> {
    let mut chosen: BTreeMap<String, &str> = BTreeMap::new();
    let mut lost: BTreeMap<String, &str> = BTreeMap::new();
    for (num, body) in adrs {
        let (won, refused) = compared_names(body);
        for name in won {
            chosen.insert(name, num);
        }
        for name in refused {
            lost.entry(name).or_insert(num);
        }
    }
    let mut out = Vec::new();
    for (num, p) in principles {
        for name in &p.rejected {
            if let Some(adr) = chosen.get(name) {
                let message = format!("principle {num} rejects `{name}`, and ADR-{adr} chose it");
                out.push(Finding::new(path, 0, PRINCIPLES, message));
            }
        }
        for name in &p.accepted {
            if chosen.contains_key(name) {
                continue;
            }
            if let Some(adr) = lost.get(name) {
                let message = format!(
                    "principle {num} accepts `{name}` on the exit-cost rule, and ADR-{adr} rejected it"
                );
                out.push(Finding::new(path, 0, PRINCIPLES, message));
            }
        }
    }
    out
}

fn anchors(
    readme_path: &str,
    adr_path: &str,
    adr: &BTreeMap<String, Principle>,
    readme: &BTreeMap<String, Principle>,
) -> Vec<Finding> {
    let marking = |local: bool| {
        if local {
            "local"
        } else {
            "to an external standard"
        }
    };
    let mut out = Vec::new();
    for (num, r) in readme {
        let Some(a) = adr.get(num) else {
            let message = format!("states principle {num}, and {adr_path} has no such principle");
            out.push(Finding::new(readme_path, 0, PRINCIPLES, message));
            continue;
        };
        if a.local != r.local {
            let message = format!(
                "principle {num} is anchored {}, and {adr_path} marks it {}",
                marking(r.local),
                marking(a.local)
            );
            out.push(Finding::new(readme_path, 0, PRINCIPLES, message));
        }
        for src in r.sources.difference(&a.sources) {
            let message =
                format!("principle {num} cites `{src}`, which the anchor in {adr_path} does not");
            out.push(Finding::new(readme_path, 0, PRINCIPLES, message));
        }
    }
    for num in adr.keys().filter(|n| !readme.contains_key(*n)) {
        let message = format!("states principle {num}, and {readme_path} does not carry it");
        out.push(Finding::new(adr_path, 0, PRINCIPLES, message));
    }
    out
}

/// The citations of an anchor, without links into the ADR set.
fn external_sources(text: &str) -> BTreeSet<String> {
    LINK_TARGET
        .captures_iter(text)
        .map(|m| m[1].to_owned())
        .filter(|t| !ADR_FILE.is_match(t) && !t.starts_with("docs/adr/"))
        .collect()
}

/// Each whole fragment is kept: `Argo Workflows` and `Argo CD` are different decisions.
fn names_in(text: &str) -> Vec<String> {
    let plain_text = plain(text);
    let no_parens = PARENTHETICS.replace_all(&plain_text, "");
    let head = no_parens.split(" \u{2014} ").next().unwrap_or("");
    let trimmed = head.trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed).trim();
    let mut out = Vec::new();
    for part in trimmed.split(',') {
        let mut part = part.trim();
        for prefix in ["and ", "every ", "all "] {
            part = part.strip_prefix(prefix).unwrap_or(part);
        }
        if let Some(name) = clean_name(part) {
            out.push(name);
        }
    }
    out
}

const TRIM: &[char] = &['.', ',', ';', ':', '(', ')'];
const TRIM_CELL: &[char] = &['.', ',', ';', ':', '(', ')', '|'];

fn clean_name(part: &str) -> Option<String> {
    let fields: Vec<&str> = part.split_whitespace().collect();
    if fields.is_empty() || fields.len() > 3 {
        return None;
    }
    let mut out = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let f = field.trim_matches(TRIM);
        if !is_name(f) || (i == 0 && (f.len() < 3 || is_stop_word(f))) {
            return None;
        }
        out.push(f);
    }
    Some(out.join(" "))
}

fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.contains(&word.to_lowercase().as_str())
}

/// The names one ADR refuses: the options that lost, without the ones it adopts, and with the ones it states are unused.
fn rejected_in(body: &str) -> Vec<String> {
    let (chosen, lost) = compared_names(body);
    let adopted = adopted_names(body);
    let mut out: BTreeSet<String> = lost
        .into_iter()
        .filter(|n| !chosen.contains(n) && !adopted.contains(n))
        .collect();
    out.extend(
        body.split('\n')
            .filter_map(|line| refusal_in(&plain(line)))
            .filter(|n| !n.is_empty()),
    );
    out.into_iter().collect()
}

fn compared_names(body: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let (mut chosen, mut lost) = (BTreeSet::new(), BTreeSet::new());
    let mut in_options = false;
    for line in body.split('\n') {
        if line.starts_with("## ") {
            in_options = line.starts_with("## Considered options");
        }
        if !in_options || !line.starts_with("| ") || line.starts_with("| ---") {
            continue;
        }
        let cells = split_row(line);
        if cells.len() < 2 {
            continue;
        }
        let Some(name) = lead_token(&cells[0]) else {
            continue;
        };
        if plain(&cells[0]).split_whitespace().count() > 3 {
            continue;
        }
        if cells[cells.len() - 1].contains("**Chosen") {
            chosen.insert(name);
        } else {
            lost.insert(name);
        }
    }
    (chosen, lost)
}

fn adopted_names(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_decision = false;
    for line in body.split('\n') {
        if line.starts_with("## ") {
            in_decision =
                line.starts_with("## Decision") && !line.starts_with("## Decision drivers");
        }
        if !in_decision || line.contains(" not ") {
            continue;
        }
        for field in plain(line).split_whitespace() {
            let f = field.trim_matches(TRIM_CELL);
            if f.len() >= 3 && is_name(f) {
                out.insert(f.to_owned());
            }
        }
    }
    out
}

/// The name that one line refuses. A qualified refusal, such as `not used for internal calls`, refuses nothing.
fn refusal_in(line: &str) -> Option<String> {
    let (idx, width) = line
        .find(" not used")
        .map(|i| (i, " not used".len()))
        .or_else(|| line.find(" not adopted").map(|i| (i, " not adopted".len())))?;
    let rest = line[idx + width..].trim();
    if !rest.is_empty() && !rest.starts_with(['.', ',']) {
        return None;
    }
    let mut clause = &line[..idx];
    for sep in [". ", "! ", "? ", "; ", ": ", "| ", "\u{2014} ", ", and "] {
        if let Some(i) = clause.rfind(sep) {
            clause = &clause[i + sep.len()..];
        }
    }
    let phrase: Vec<&str> = clause
        .split_whitespace()
        .map(|f| f.trim_matches(TRIM_CELL))
        .filter(|f| f.len() >= 3 && is_name(f) && !is_stop_word(f))
        .filter(|f| f.starts_with(|c: char| c.is_ascii_uppercase()))
        .collect();
    Some(phrase.join(" "))
}

fn stack_table(path: &str, readme: &str, rejected: &BTreeMap<&str, Vec<String>>) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut in_stack = false;
    for (i, line) in readme.split('\n').enumerate() {
        if line.starts_with("## ") {
            in_stack = line.starts_with("## Stack at a glance");
        }
        if !in_stack || !line.starts_with("| ") || line.starts_with("| ---") {
            continue;
        }
        let cells = split_row(line);
        if cells.len() < 3 || cells[0] == "Concern" {
            continue;
        }
        let decision = plain(&cells[1]);
        for reference in ADR_REF.captures_iter(&cells[2]) {
            let num = reference
                .get(1)
                .or_else(|| reference.get(2))
                .map_or("", |m| m.as_str());
            for name in rejected.get(num).into_iter().flatten() {
                if contains_token(&decision, name) {
                    let message = format!(
                        "row `{}` presents `{name}` as the decision, and ADR-{num} rejects it",
                        cells[0]
                    );
                    out.push(Finding::new(path, i + 1, STACK, message));
                }
            }
        }
    }
    out
}

fn headcount(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(HEADCOUNT) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(
        HEADCOUNT,
        "paths",
        &["docs/**/*.md", "README.md", "AGENTS.md"],
    )?;
    let exempt = ctx.paths(HEADCOUNT, "exempt", &["docs/product/**", "**/*.local.md"])?;
    let mut out = Vec::new();
    for path in ctx
        .files()
        .filter(|f| paths.matches(f) && !exempt.matches(f))
    {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        for (i, line) in text.split('\n').enumerate() {
            let Some(m) = HEADCOUNT_RE.find(line) else {
                continue;
            };
            let found = m.as_str().trim();
            if !SINGULAR.is_match(found) {
                let message = format!("states a headcount, `{found}`: state the obligations");
                out.push(Finding::new(path, i + 1, HEADCOUNT, message));
            }
        }
    }
    Ok(out)
}

fn lead_token(cell: &str) -> Option<String> {
    plain(cell)
        .split_whitespace()
        .map(|f| f.trim_matches(TRIM))
        .find(|f| f.len() >= 3 && !is_stop_word(f) && is_name(f))
        .map(str::to_owned)
}

fn is_name(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '/'))
}

/// Every word of a name appears in the cell, so a variant such as `Argo CD Image Updater` needs all of its words.
fn contains_token(haystack: &str, name: &str) -> bool {
    name.split_whitespace().all(|token| {
        Regex::new(&format!(r"\b{}\b", regex::escape(token)))
            .is_ok_and(|word| word.is_match(haystack))
    })
}

fn split_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    trimmed.split(" | ").map(|c| c.trim().to_owned()).collect()
}

fn plain(text: &str) -> String {
    let unlinked = LINK_TEXT.replace_all(text, "$1");
    INLINE_TAG.replace_all(&unlinked, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_must_end_its_clause() {
        assert_eq!(
            refusal_in("Kustomize is not used."),
            Some("Kustomize".to_owned())
        );
        assert_eq!(refusal_in("Hydra is not used for internal calls"), None);
    }

    #[test]
    fn a_losing_variant_of_the_winner_is_not_rejected() {
        let body = "## Considered options\n\n| Option | Verdict |\n| --- | --- |\n| **Helm** | **Chosen** |\n| Helm then Kustomize | Rejected |\n| Kustomize | Rejected |\n\n## Decision\n\nHelm renders it.\n";
        let rejected = rejected_in(body);
        assert!(rejected.contains(&"Kustomize".to_owned()));
        assert!(!rejected.contains(&"Helm".to_owned()));
    }

    #[test]
    fn a_headcount_is_found() {
        assert!(HEADCOUNT_RE.is_match("It needs two to three platform engineers."));
        assert!(!HEADCOUNT_RE.is_match("Two services talk."));
    }
}
