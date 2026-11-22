//! `money/*`: a monetary value is the shared money type in the spec, in Go, and in TypeScript.

use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use saphyr::{MarkedYaml, YamlData};
use tree_sitter::Node;

use super::Ctx;
use super::golang;
use crate::finding::Finding;
use crate::yaml;

const SPEC: &str = "money/spec";
const NO_FLOAT: &str = "money/no-float";
const TYPESCRIPT: &str = "money/typescript";

const MONEY_WORDS: [&str; 10] = [
    "amount", "balance", "charge", "cost", "discount", "fee", "price", "refund", "subtotal",
    "total",
];
/// A last word that counts something and holds no value.
const COUNT_WORDS: [&str; 15] = [
    "bytes", "count", "items", "keys", "length", "models", "pages", "points", "quantity", "qty",
    "rows", "seconds", "size", "tokens", "users",
];
const GO_SKIP: [&str; 6] = [
    "libs/go/money",
    "libs/go/sdks",
    "internal/store",
    "node_modules",
    "vendor",
    ".next",
];
const TS_SKIP: [&str; 7] = [
    "libs/ts/money",
    "libs/ts/sdks",
    "node_modules",
    ".next",
    "dist",
    "test-results",
    "playwright-report",
];
const NAMED_KINDS: [&str; 6] = [
    "field_declaration",
    "parameter_declaration",
    "variadic_parameter_declaration",
    "type_parameter_declaration",
    "var_spec",
    "const_spec",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

static CENTS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)cents"));
static TS_FIELD: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b([a-z_][a-z0-9_]*)\s*\??\s*:\s*(number\b|-?[0-9])"));
static DISPLAY_ONLY: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^// lint:money-allow: \S"));

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = spec(ctx)?;
    out.extend(go(ctx)?);
    out.extend(typescript(ctx)?);
    Ok(out)
}

/// Breaks snake_case, camelCase, and PascalCase into lowercase words.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in name.chars() {
        if matches!(c, '_' | '-' | '.') {
            if !current.is_empty() {
                out.push(current.to_lowercase());
                current.clear();
            }
        } else if c.is_ascii_uppercase() {
            if !current.is_empty() {
                out.push(current.to_lowercase());
                current.clear();
            }
            current.push(c);
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        out.push(current.to_lowercase());
    }
    out
}

/// The last word decides what the name is: `total_count` counts, and `order_total` is a total.
fn is_money_name(name: &str) -> bool {
    let parts = words(name);
    let Some(last) = parts.last() else {
        return false;
    };
    !COUNT_WORDS.contains(&last.as_str()) && parts.iter().any(|w| MONEY_WORDS.contains(&w.as_str()))
}

fn spec(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(SPEC) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(SPEC, "specs", &["services/*/openapi.yaml"])?;
    let component = ctx
        .cfg
        .option_str(SPEC, "component")?
        .unwrap_or_else(|| "Money".to_owned());
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| paths.matches(f)) {
        let text = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        let docs = yaml::load(&text).with_context(|| format!("parse {path}"))?;
        for doc in &docs {
            spec_node(doc, path, &component, &mut out);
        }
    }
    Ok(out)
}

fn spec_node(node: &MarkedYaml<'_>, path: &str, component: &str, out: &mut Vec<Finding>) {
    for (key, value) in yaml::pairs(node) {
        if yaml::string(key) == Some("properties")
            && matches!(value.data, YamlData::Mapping(_))
            && !is_money_definition(value)
        {
            for (name, schema) in yaml::pairs(value) {
                let name_text = yaml::text(name).unwrap_or_default();
                if !is_money_name(&name_text) {
                    continue;
                }
                let field = |wanted: &str| {
                    yaml::pairs(schema)
                        .find(|(k, _)| yaml::string(k) == Some(wanted))
                        .and_then(|(_, v)| yaml::text(v))
                        .unwrap_or_default()
                };
                let reference = field("$ref");
                if reference.rsplit('/').next() == Some(component) {
                    continue;
                }
                let kind = field("type");
                let shape = if kind.is_empty() {
                    "an unrecognised shape".to_owned()
                } else {
                    format!("type: {kind}")
                };
                let message = format!(
                    "`{name_text}` is a monetary property but is {shape}, not $ref: {component}"
                );
                out.push(Finding::new(path, name.span.start.line(), SPEC, message));
            }
        }
    }
    for child in yaml::children(node) {
        spec_node(child, path, component, out);
    }
}

/// The component itself is recognised by its members: `amount` with `currency`.
fn is_money_definition(properties: &MarkedYaml<'_>) -> bool {
    let has = |wanted: &str| yaml::pairs(properties).any(|(k, _)| yaml::string(k) == Some(wanted));
    has("amount") && has("currency")
}

fn go(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(NO_FLOAT) {
        return Ok(Vec::new());
    }
    let skip = ctx.cfg.option_strings(NO_FLOAT, "skip", &GO_SKIP)?;
    let mut out = Vec::new();
    for path in ctx
        .repo
        .files
        .iter()
        .filter(|f| f.ends_with(".go") && !skip.iter().any(|s| f.contains(s.as_str())))
    {
        let Some(source) = ctx.repo.read(path) else {
            continue;
        };
        let tree = golang::parse(&source)?;
        golang::walk(tree.root_node(), &mut |node| {
            if !NAMED_KINDS.contains(&node.kind()) {
                return;
            }
            let kind = node
                .child_by_field_name("type")
                .filter(|t| t.kind() == "type_identifier")
                .map(|t| &source[t.byte_range()]);
            let mut cursor = node.walk();
            let names: Vec<Node<'_>> = node.children_by_field_name("name", &mut cursor).collect();
            for name in names {
                let text = &source[name.byte_range()];
                let line = name.start_position().row + 1;
                if CENTS.is_match(text) {
                    let message =
                        format!("`{text}` names minor units. A monetary value is the money type");
                    out.push(Finding::new(path, line, NO_FLOAT, message));
                } else if is_money_name(text)
                    && let Some(float) = kind.filter(|k| matches!(*k, "float32" | "float64"))
                {
                    let message = format!("`{text}` is monetary and {float}. Use the money type");
                    out.push(Finding::new(path, line, NO_FLOAT, message));
                }
            }
        });
    }
    Ok(out)
}

fn typescript(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(TYPESCRIPT) {
        return Ok(Vec::new());
    }
    let skip = ctx.cfg.option_strings(TYPESCRIPT, "skip", &TS_SKIP)?;
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| {
        (f.ends_with(".ts") || f.ends_with(".tsx") || f.ends_with(".js"))
            && !skip.iter().any(|s| f.contains(s.as_str()))
    }) {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        for (line, message) in typescript_text(&text) {
            out.push(Finding::new(path, line, TYPESCRIPT, message));
        }
    }
    Ok(out)
}

/// A name and a number on one line. A file with a reasoned marker holds display figures only.
fn typescript_text(source: &str) -> Vec<(usize, String)> {
    if DISPLAY_ONLY.is_match(source) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, line) in source.split('\n').enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with('*') {
            continue;
        }
        for m in TS_FIELD.captures_iter(line) {
            let name = &m[1];
            if is_money_name(name) || CENTS.is_match(name) {
                out.push((
                    i + 1,
                    format!("`{name}` is a monetary value as a number. Use the money type"),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_names() {
        for name in ["price", "total_amount", "unitPrice", "order_total", "Fee"] {
            assert!(is_money_name(name), "{name}");
        }
        for name in [
            "total_count",
            "total_pages",
            "amount_of_rows",
            "totals",
            "pricing_enabled",
        ] {
            assert!(!is_money_name(name), "{name}");
        }
    }

    #[test]
    fn typescript_lines() {
        assert_eq!(typescript_text("const a = { price: 12 };\n").len(), 1);
        assert_eq!(
            typescript_text("type A = { total_cents?: number };\n").len(),
            1
        );
        assert_eq!(typescript_text("// price: 12\n").len(), 0);
        assert_eq!(typescript_text("const a = { total_count: 12 };\n").len(), 0);
        assert_eq!(
            typescript_text("// lint:money-allow: a chart\nconst a = { price: 12 };\n").len(),
            0
        );
    }

    #[test]
    fn go_declarations() {
        let source = "package a\ntype T struct {\n\tPrice float64\n\tCount float64\n\tTotalCents int\n}\nfunc f(fee float32, n int) {}\nvar cost float64\n";
        let tree = golang::parse(source).unwrap();
        let mut found = Vec::new();
        golang::walk(tree.root_node(), &mut |node| {
            if NAMED_KINDS.contains(&node.kind()) {
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    found.push(source[name.byte_range()].to_owned());
                }
            }
        });
        for name in ["Price", "Count", "TotalCents", "fee", "n", "cost"] {
            assert!(found.iter().any(|f| f == name), "{name}");
        }
    }
}
