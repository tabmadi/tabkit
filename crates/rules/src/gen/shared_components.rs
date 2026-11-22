//! `gen/shared-components`: copies each shared OpenAPI component that a spec reaches into the marked region of that
//! spec. It copies source text, so a component keeps its comments and its style.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;

use super::Output;
use tabkit_core::config::{Config, Paths};
use tabkit_core::repo::Repo;

const ID: &str = "gen/shared-components";
const BEGIN: &str = "# >>> shared-components:";
const END: &str = "# <<< shared-components";
const SECTIONS: [&str; 2] = ["schemas", "responses"];

static REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#/components/(schemas|responses)/([A-Za-z0-9_.-]+)")
        .expect("a built-in pattern compiles")
});

/// One component: its name and its text, dedented to column zero.
struct Component {
    name: String,
    text: String,
}

pub fn run(repo: &Repo, cfg: &Config) -> Result<Vec<Output>> {
    let Some(source) = cfg.option_str(ID, "source")? else {
        return Ok(Vec::new());
    };
    let Some(text) = repo.read(&source) else {
        return Ok(Vec::new());
    };
    let sections: BTreeMap<&str, Vec<Component>> = SECTIONS
        .iter()
        .map(|key| (*key, components(&text, key)))
        .collect();
    let specs = Paths::new(&cfg.option_strings(ID, "specs", &["services/*/openapi.yaml"])?)?;
    let mut out = Vec::new();
    for path in repo.files.iter().filter(|f| specs.matches(f)) {
        let spec = repo.read(path).with_context(|| format!("read {path}"))?;
        let keep = reachable(&sections, &spec);
        let mut updated = spec;
        for key in SECTIONS {
            let begin = format!("{BEGIN} {key}. Generated from {source}. Do not edit");
            let block: Vec<&str> = sections[key]
                .iter()
                .filter(|c| keep.contains(&format!("{key}/{}", c.name)))
                .map(|c| c.text.as_str())
                .collect();
            updated = splice(&updated, &begin, &block.concat())
                .with_context(|| format!("{path}: {key}"))?;
        }
        out.push(Output {
            path: path.clone(),
            bytes: updated.into_bytes(),
        });
    }
    Ok(out)
}

/// The entries of a top-level section, each with the comment lines above it. Blank lines between entries are dropped.
fn components(text: &str, section: &str) -> Vec<Component> {
    let header = format!("{section}:");
    let mut out: Vec<Component> = Vec::new();
    let mut inside = false;
    let mut comments: Vec<&str> = Vec::new();
    let mut blanks = 0;
    for line in text.lines() {
        if !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
            inside = line == header;
            comments.clear();
            blanks = 0;
            continue;
        }
        if !inside {
            continue;
        }
        if line.trim().is_empty() {
            blanks += 1;
            continue;
        }
        let body = line.strip_prefix("  ").unwrap_or(line);
        let is_key = !body.starts_with([' ', '#']) && body.ends_with(':');
        if body.starts_with('#') {
            comments.push(body);
        } else if is_key {
            let mut entry = String::new();
            for comment in comments.drain(..) {
                entry.push_str(comment);
                entry.push('\n');
            }
            entry.push_str(body);
            entry.push('\n');
            out.push(Component {
                name: body.trim_end_matches(':').trim_matches('"').to_owned(),
                text: entry,
            });
        } else if let Some(last) = out.last_mut() {
            // A blank line inside a block scalar is part of the value.
            for _ in 0..blanks {
                last.text.push('\n');
            }
            for comment in comments.drain(..) {
                last.text.push_str(comment);
                last.text.push('\n');
            }
            last.text.push_str(body);
            last.text.push('\n');
        }
        if !body.starts_with('#') {
            blanks = 0;
        }
    }
    out
}

/// The shared components that a spec points at, closed over their own references.
fn reachable(sections: &BTreeMap<&str, Vec<Component>>, spec: &str) -> BTreeSet<String> {
    let by_name: BTreeMap<String, &str> = sections
        .iter()
        .flat_map(|(key, list)| {
            list.iter()
                .map(move |c| (format!("{key}/{}", c.name), c.text.as_str()))
        })
        .collect();
    let mut pending = refs(&strip_regions(spec));
    let mut seen = BTreeSet::new();
    while let Some(reference) = pending.pop() {
        if seen.contains(&reference) {
            continue;
        }
        if let Some(text) = by_name.get(&reference) {
            pending.extend(refs(text));
            seen.insert(reference);
        }
    }
    seen
}

fn refs(text: &str) -> Vec<String> {
    REF.captures_iter(text)
        .map(|m| format!("{}/{}", &m[1], &m[2]))
        .collect()
}

/// The spec without its generated regions, because a component is carried only when the spec needs it.
fn strip_regions(doc: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in doc.split('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with(BEGIN) {
            inside = true;
        } else if inside && trimmed == END {
            inside = false;
        } else if !inside {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Replaces the region between the markers. A spec without the markers is an error, because it is not wired up.
fn splice(doc: &str, begin: &str, block: &str) -> Result<String> {
    let lines: Vec<&str> = doc.split('\n').collect();
    let Some(start) = lines.iter().position(|l| l.trim() == begin) else {
        bail!("no `{begin}` marker");
    };
    let Some(end) = lines[start..]
        .iter()
        .position(|l| l.trim() == END)
        .map(|i| i + start)
    else {
        bail!("`{begin}` is not closed by `{END}`");
    };
    let indent = &lines[start][..lines[start].len() - lines[start].trim_start_matches(' ').len()];
    let mut out: Vec<String> = lines[..=start].iter().map(|l| (*l).to_owned()).collect();
    for line in block.trim_end_matches('\n').split('\n') {
        out.push(if line.is_empty() {
            String::new()
        } else {
            format!("{indent}{line}")
        });
    }
    out.extend(lines[end..].iter().map(|l| (*l).to_owned()));
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "schemas:\n  # Why it exists.\n  Problem:\n    type: object\n    properties:\n      money:\n        $ref: \"#/components/schemas/Money\"\n\n  Money:\n    type: string\n\n  Unused:\n    type: string\nresponses:\n  NotFound:\n    content:\n      schema:\n        $ref: \"#/components/schemas/Problem\"\n";

    #[test]
    fn a_spec_gets_what_it_reaches() {
        let sections: BTreeMap<&str, Vec<Component>> = SECTIONS
            .iter()
            .map(|key| (*key, components(SOURCE, key)))
            .collect();
        assert_eq!(sections["schemas"].len(), 3);
        assert!(
            sections["schemas"][0]
                .text
                .starts_with("# Why it exists.\nProblem:\n")
        );
        let keep = reachable(&sections, "$ref: '#/components/responses/NotFound'");
        assert!(keep.contains("schemas/Money"));
        assert!(!keep.contains("schemas/Unused"));
    }

    #[test]
    fn splice_replaces_only_the_region() {
        let begin = "# >>> shared-components: schemas. Generated from s. Do not edit";
        let doc = format!("a:\n  {begin}\n  old: 1\n  {END}\nb: 2\n");
        let out = splice(&doc, begin, "new: 2\n").unwrap();
        assert_eq!(out, format!("a:\n  {begin}\n  new: 2\n  {END}\nb: 2\n"));
        assert!(splice("a: 1\n", begin, "x").is_err());
    }
}
