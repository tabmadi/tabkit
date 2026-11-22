//! `go/*`: rules over Go syntax, through tree-sitter, and over the package closure, through `go list`.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;
use tree_sitter::{Node, Parser, Tree};

use super::Ctx;
use crate::config::{Config, Paths};
use crate::finding::Finding;
use crate::repo::Repo;

const CALL_ARGS: &str = "go/call-args";
const ACTIVITY: &str = "go/activity-register";
const DEPS: &str = "go/service-deps";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

static URL_ENV: LazyLock<Regex> = LazyLock::new(|| re(r#""([A-Z][A-Z0-9_]*)_URL""#));
static RECIPE_HEADER: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^([a-z][a-z0-9-]*)( [^:\n]*)?:"));
static ROOT_CALL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?m)^\s+just --justfile \.\./\.\./justfile (.+)$"));

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = call_args(ctx)?;
    out.extend(activity_register(ctx)?);
    out.extend(service_deps(ctx)?);
    Ok(out)
}

/// Parses Go source. tree-sitter recovers from a syntax error, and the compiler reports it.
pub fn parse(text: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .context("load the Go grammar")?;
    parser.parse(text, None).context("parse Go source")
}

/// Calls `visit` with every node of a tree.
pub fn walk<'a>(root: Node<'a>, visit: &mut dyn FnMut(Node<'a>)) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        visit(node);
        let mut cursor = node.walk();
        let children: Vec<Node<'a>> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

/// The argument nodes and the two parens of a call or a conversion.
struct Call<'a> {
    lparen: Node<'a>,
    rparen: Node<'a>,
    args: Vec<Node<'a>>,
}

fn call_parts(node: Node<'_>) -> Option<Call<'_>> {
    let list = match node.kind() {
        "call_expression" => node.child_by_field_name("arguments")?,
        "type_conversion_expression" => node,
        _ => return None,
    };
    let mut cursor = list.walk();
    let children: Vec<Node<'_>> = list.children(&mut cursor).collect();
    let open = children.iter().position(|c| c.kind() == "(")?;
    let close = children.iter().rposition(|c| c.kind() == ")")?;
    let args: Vec<Node<'_>> = children[open + 1..close]
        .iter()
        .copied()
        .filter(|c| c.is_named() && c.kind() != "comment")
        .collect();
    (!args.is_empty()).then(|| Call {
        lparen: children[open],
        rparen: children[close],
        args,
    })
}

/// A call's arguments are on one line, or each on its own line between the parens.
fn well_laid_out(call: &Call<'_>) -> bool {
    let open = call.lparen.start_position().row;
    let close = call.rparen.start_position().row;
    if open == close {
        return true;
    }
    let (Some(first), Some(last)) = (call.args.first(), call.args.last()) else {
        return true;
    };
    open != first.start_position().row
        && close != last.end_position().row
        && call
            .args
            .windows(2)
            .all(|w| w[0].end_position().row != w[1].start_position().row)
}

/// The insertions that put each argument on its own line. The formatter then sets the indent.
fn explode_edits(call: &Call<'_>) -> Vec<(usize, &'static str)> {
    let mut edits = Vec::new();
    let mut previous = call.lparen.end_position().row;
    for arg in &call.args {
        if previous == arg.start_position().row {
            edits.push((arg.start_byte(), "\n"));
        }
        previous = arg.end_position().row;
    }
    if let Some(last) = call.args.last()
        && last.end_position().row == call.rparen.start_position().row
    {
        edits.push((last.end_byte(), ",\n"));
    }
    edits
}

fn is_generated(text: &str) -> bool {
    text.lines()
        .any(|l| l.contains("Code generated") && l.contains("DO NOT EDIT"))
}

fn call_arg_files(cfg: &Config) -> Result<(Paths, Paths)> {
    let paths = Paths::new(&cfg.option_strings(CALL_ARGS, "paths", &["**/*.go"])?)?;
    let exempt = Paths::new(&cfg.option_strings(
        CALL_ARGS,
        "exempt",
        &["services/_*/**", "**/testdata/**", "vendor/**"],
    )?)?;
    Ok((paths, exempt))
}

fn call_args(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(CALL_ARGS) {
        return Ok(Vec::new());
    }
    let (paths, exempt) = call_arg_files(ctx.cfg)?;
    let mut out = Vec::new();
    for path in ctx
        .files()
        .filter(|f| paths.matches(f) && !exempt.matches(f))
    {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        if is_generated(&text) {
            continue;
        }
        let tree = parse(&text)?;
        walk(tree.root_node(), &mut |node| {
            if let Some(call) = call_parts(node)
                && !well_laid_out(&call)
            {
                out.push(Finding::new(
                    path,
                    call.lparen.start_position().row + 1,
                    CALL_ARGS,
                    "call arguments are partly broken across lines: use one line, or one argument on each line",
                ));
            }
        });
    }
    Ok(out)
}

/// Puts the arguments of each badly laid out call on their own lines, and returns the number of files changed.
pub fn fix_call_args(repo: &Repo, cfg: &Config) -> Result<usize> {
    let (paths, exempt) = call_arg_files(cfg)?;
    let mut changed = 0;
    for path in repo
        .files
        .iter()
        .filter(|f| paths.matches(f) && !exempt.matches(f))
    {
        let Some(mut text) = repo.read(path) else {
            continue;
        };
        if is_generated(&text) {
            continue;
        }
        let tree = parse(&text)?;
        let mut edits = Vec::new();
        walk(tree.root_node(), &mut |node| {
            if let Some(call) = call_parts(node)
                && !well_laid_out(&call)
            {
                edits.extend(explode_edits(&call));
            }
        });
        if edits.is_empty() {
            continue;
        }
        edits.sort_unstable();
        edits.dedup();
        for (at, insert) in edits.into_iter().rev() {
            text.insert_str(at, insert);
        }
        let target = repo.root.join(path);
        std::fs::write(&target, text).with_context(|| format!("write {}", target.display()))?;
        changed += 1;
    }
    Ok(changed)
}

/// The text of a string literal, without its quotes.
fn string_literal<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    if !matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    ) {
        return None;
    }
    let text = &source[node.byte_range()];
    text.get(1..text.len().checked_sub(1)?)
}

fn named_args(node: Node<'_>) -> Vec<Node<'_>> {
    let Some(list) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect()
}

fn method_name<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let function = node.child_by_field_name("function")?;
    (function.kind() == "selector_expression")
        .then(|| function.child_by_field_name("field"))
        .flatten()
        .map(|f| &source[f.byte_range()])
}

/// The activity names that a worker file registers.
fn registered(source: &str) -> Result<BTreeSet<String>> {
    let tree = parse(source)?;
    let mut out = BTreeSet::new();
    walk(tree.root_node(), &mut |node| {
        if node.kind() != "call_expression" {
            return;
        }
        let args = named_args(node);
        match method_name(node, source) {
            Some("RegisterActivity") => {
                if let Some(field) = args
                    .first()
                    .filter(|a| a.kind() == "selector_expression")
                    .and_then(|a| a.child_by_field_name("field"))
                {
                    out.insert(source[field.byte_range()].to_owned());
                }
            }
            Some("RegisterActivityWithOptions") => {
                for arg in args.iter().filter(|a| a.kind() == "composite_literal") {
                    out.extend(option_names(*arg, source));
                }
            }
            _ => {}
        }
    });
    Ok(out)
}

/// The expression inside the key or the value of a keyed element.
fn element_part<'t>(element: Node<'t>, field: &str) -> Option<Node<'t>> {
    let wrapper = element.child_by_field_name(field)?;
    let mut cursor = wrapper.walk();
    let first = wrapper.named_children(&mut cursor).next();
    Some(first.unwrap_or(wrapper))
}

/// The `Name: "<name>"` of a composite literal.
fn option_names(literal: Node<'_>, source: &str) -> Vec<String> {
    let Some(body) = literal.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let elements: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    elements
        .into_iter()
        .filter(|e| e.kind() == "keyed_element")
        .filter(|e| element_part(*e, "key").is_some_and(|k| &source[k.byte_range()] == "Name"))
        .filter_map(|e| string_literal(element_part(e, "value")?, source).map(str::to_owned))
        .collect()
}

/// A workflow names its activities as strings, so only this check sees one that its worker does not register.
fn activity_register(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(ACTIVITY) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for service in ctx
        .repo
        .dirs
        .iter()
        .filter_map(|d| d.strip_prefix("services/"))
        .filter(|d| !d.contains('/') && !d.starts_with('_'))
    {
        let prefix = format!("services/{service}/internal/workflows/");
        let worker = format!("services/{service}/cmd/worker/main.go");
        let Some(worker_text) = ctx.repo.read(&worker) else {
            continue;
        };
        let known = registered(&worker_text)?;
        let mut called: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for path in ctx.repo.files.iter().filter(|f| {
            f.strip_prefix(&prefix)
                .is_some_and(|n| !n.contains('/') && n.ends_with(".go") && !n.ends_with("_test.go"))
        }) {
            let Some(source) = ctx.repo.read(path) else {
                continue;
            };
            let tree = parse(&source)?;
            walk(tree.root_node(), &mut |node| {
                if node.kind() != "call_expression"
                    || !matches!(
                        method_name(node, &source),
                        Some("ExecuteActivity" | "ExecuteLocalActivity")
                    )
                {
                    return;
                }
                if let Some(name) = named_args(node)
                    .get(1)
                    .and_then(|a| string_literal(*a, &source))
                {
                    called
                        .entry(name.to_owned())
                        .or_insert_with(|| (path.clone(), node.start_position().row + 1));
                }
            });
        }
        for (name, (path, line)) in called {
            if !known.contains(&name) {
                let message = format!("`{name}` is executed here but never registered in {worker}");
                out.push(Finding::new(&path, line, ACTIVITY, message));
            }
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct Package {
    #[serde(rename = "ImportPath")]
    import_path: String,
    #[serde(rename = "Dir", default)]
    dir: String,
}

fn closure(ctx: &Ctx<'_>, dir: &str) -> Result<Option<Vec<Package>>> {
    let output = Command::new("go")
        .current_dir(&ctx.repo.root)
        .args(["list", "-deps", "-json", &format!("./{dir}")])
        .output();
    let output = match output {
        // A repository can run tabrules without Go, and then this rule has no input.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => other.context("run `go list`")?,
    };
    if !output.status.success() {
        bail!(
            "go list {dir}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let packages = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Package>()
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("decode go list output for {dir}"))?;
    Ok(Some(packages))
}

/// The `X` of every `X_URL` string in the non-test Go files of a directory.
fn env_names(dir: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {dir}"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_file()
            || !name.to_ascii_lowercase().ends_with(".go")
            || name.ends_with("_test.go")
        {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        out.extend(URL_ENV.captures_iter(&text).map(|m| m[1].to_owned()));
    }
    Ok(out)
}

/// The root recipes that each recipe of a service justfile brings up.
fn declared_deps(text: &str) -> BTreeMap<String, BTreeSet<String>> {
    let headers: Vec<_> = RECIPE_HEADER.captures_iter(text).collect();
    let mut out = BTreeMap::new();
    for (i, header) in headers.iter().enumerate() {
        let whole = header.get(0).expect("a match has a whole group");
        let end = headers
            .get(i + 1)
            .and_then(|h| h.get(0))
            .map_or(text.len(), |m| m.start());
        let deps: BTreeSet<String> = ROOT_CALL
            .captures_iter(&text[whole.end()..end])
            .flat_map(|c| {
                c[1].split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        out.insert(header[1].to_owned(), deps);
    }
    out
}

/// Each service entrypoint declares exactly the dependencies that its code uses.
fn service_deps(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(DEPS) {
        return Ok(Vec::new());
    }
    let Some(module) = ctx.repo.read("go.mod").and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("module ").map(|m| m.trim().to_owned()))
    }) else {
        return Ok(Vec::new());
    };
    let services: Vec<&str> = ctx
        .repo
        .dirs
        .iter()
        .filter_map(|d| d.strip_prefix("services/"))
        .filter(|d| !d.contains('/') && !d.starts_with('_'))
        .collect();
    let mut out = Vec::new();
    for service in &services {
        let justfile = format!("services/{service}/justfile");
        let declared = declared_deps(&ctx.repo.read(&justfile).unwrap_or_default());
        for command in ["server", "worker"] {
            let dir = format!("services/{service}/cmd/{command}");
            if !ctx.repo.has(&dir) {
                continue;
            }
            let Some(packages) = closure(ctx, &dir)? else {
                return Ok(out);
            };
            let mut required = BTreeSet::new();
            for package in &packages {
                if package.import_path == "go.temporal.io/sdk/client" {
                    required.insert("dep-temporal".to_owned());
                } else if package.import_path == format!("{module}/libs/go/authz") {
                    required.insert("dep-openfga".to_owned());
                }
                if !package.import_path.starts_with(&module) {
                    continue;
                }
                for name in env_names(&package.dir)? {
                    if name == "DATABASE" {
                        required.insert("dep-postgres".to_owned());
                        continue;
                    }
                    let callee = name.to_lowercase();
                    if callee != *service && services.contains(&callee.as_str()) {
                        required.insert(format!("svc-{callee}"));
                    }
                }
            }
            let empty = BTreeSet::new();
            let listed = declared.get(command).unwrap_or(&empty);
            for dep in required.difference(listed) {
                let message = format!("recipe `{command}` uses {dep} but does not declare it");
                out.push(Finding::new(&justfile, 0, DEPS, message));
            }
            for dep in listed
                .difference(&required)
                .filter(|d| d.starts_with("dep-") || d.starts_with("svc-"))
            {
                let message =
                    format!("recipe `{command}` declares {dep} but nothing in the closure uses it");
                out.push(Finding::new(&justfile, 0, DEPS, message));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALLS: &str = "package a\n\nfunc f(args ...int) {}\n\nfunc calls(xs []int) {\n\tf(1, 2, 3)\n\tf(\n\t\t1,\n\t\t2,\n\t)\n\tf(1, // bad\n\t\t2)\n\tf( // bad\n\t\t1, 2,\n\t)\n\tf( // bad\n\t\txs...)\n}\n";

    fn bad_lines(source: &str) -> Vec<usize> {
        let tree = parse(source).unwrap();
        let mut out = Vec::new();
        walk(tree.root_node(), &mut |node| {
            if let Some(call) = call_parts(node)
                && !well_laid_out(&call)
            {
                out.push(call.lparen.start_position().row + 1);
            }
        });
        out
    }

    #[test]
    fn mixed_layouts_are_found() {
        assert_eq!(bad_lines(CALLS), [11, 13, 16]);
    }

    #[test]
    fn the_fix_puts_each_argument_on_its_own_line() {
        let tree = parse(CALLS).unwrap();
        let mut edits = Vec::new();
        walk(tree.root_node(), &mut |node| {
            if let Some(call) = call_parts(node)
                && !well_laid_out(&call)
            {
                edits.extend(explode_edits(&call));
            }
        });
        edits.sort_unstable();
        let mut fixed = CALLS.to_owned();
        for (at, insert) in edits.into_iter().rev() {
            fixed.insert_str(at, insert);
        }
        assert_eq!(bad_lines(&fixed), Vec::<usize>::new());
    }

    #[test]
    fn a_worker_registers_by_method_and_by_option() {
        let source = "package main\nfunc main() {\n\tw.RegisterActivity(a.Charge)\n\tw.RegisterActivityWithOptions(a.Refund, activity.RegisterOptions{Name: \"RefundV2\"})\n}\n";
        let got: Vec<String> = registered(source).unwrap().into_iter().collect();
        assert_eq!(got, ["Charge", "RefundV2"]);
    }

    #[test]
    fn a_recipe_declares_its_root_calls() {
        let text = "server: env\n    just --justfile ../../justfile dep-postgres svc-orders\n    go run .\n\ntest:\n    go test\n";
        let got = declared_deps(text);
        assert!(got["server"].contains("dep-postgres") && got["server"].contains("svc-orders"));
        assert_eq!(got["test"].len(), 0);
    }
}
