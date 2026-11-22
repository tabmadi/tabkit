//! Reads and edits one scalar of a YAML file. An edit keeps every other byte, so a values diff shows only the pin.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use saphyr::{Scalar, YamlData};
use tabkit_core::yaml;

fn node<'a, 'b>(
    root: &'a saphyr::MarkedYaml<'b>,
    path: &str,
) -> Option<&'a saphyr::MarkedYaml<'b>> {
    let mut current = root;
    for key in path.trim_start_matches('.').split('.') {
        current = yaml::pairs(current)
            .find(|(k, _)| yaml::string(k) == Some(key))
            .map(|(_, value)| value)?;
    }
    Some(current)
}

/// The scalar at a dotted path of the first document, or an empty string.
pub fn get(text: &str, path: &str) -> Result<String> {
    let docs = yaml::load(text)?;
    Ok(docs
        .first()
        .and_then(|doc| node(doc, path))
        .and_then(yaml::text)
        .unwrap_or_default())
}

fn present(text: &str, path: &str) -> Result<bool> {
    let docs = yaml::load(text)?;
    Ok(docs
        .first()
        .and_then(|doc| node(doc, path))
        .is_some_and(|value| !matches!(value.data, YamlData::Value(Scalar::Null))))
}

fn split_key(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    let end = rest.find(':')?;
    let key = &rest[..end];
    let valid = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    valid.then_some((indent, key))
}

/// The 0-based line of a block-style key at a dotted path.
fn scalar_line(text: &str, path: &str) -> Option<usize> {
    let target = path.trim_start_matches('.');
    let mut frames: Vec<(usize, &str)> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let Some((indent, key)) = split_key(line) else {
            continue;
        };
        while frames.last().is_some_and(|(i, _)| *i >= indent) {
            frames.pop();
        }
        frames.push((indent, key));
        let current = frames.iter().map(|(_, k)| *k).collect::<Vec<_>>().join(".");
        if current == target {
            return Some(number);
        }
    }
    None
}

/// Writes the value between the key and any comment. It returns `None` when the key is absent or null.
pub fn replaced(text: &str, path: &str, value: &str) -> Result<Option<String>> {
    if !present(text, path)? {
        return Ok(None);
    }
    let Some(number) = scalar_line(text, path) else {
        bail!("{path} exists but is not a block-style key, so it cannot be edited in place");
    };
    let mut out = String::with_capacity(text.len() + value.len());
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index != number {
            out.push_str(line);
            continue;
        }
        let body = line.strip_suffix('\n').unwrap_or(line);
        let colon = body.find(':').context("the key line has a colon")?;
        let (key, rest) = body.split_at(colon + 1);
        out.push_str(key);
        out.push_str(" \"");
        out.push_str(value);
        out.push('"');
        if let Some(hash) = rest.find('#') {
            out.push_str("   ");
            out.push_str(&rest[hash..]);
        }
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(Some(out))
}

/// Sets a scalar in a file through a temp file, so a failure cannot truncate it. It returns false for an absent key.
pub fn set(path: &Path, key: &str, value: &str) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let Some(new) = replaced(&text, key, value).with_context(|| path.display().to_string())? else {
        return Ok(false);
    };
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp, new).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: &str = "name: orders\nimage:\n  repository: r/orders # the repo\n  tag: \"old\"   # pin\n  digest: \"\"\nworker:\n  image:\n    repository: r/worker\n    tag: \"old\"\n    digest: \"\"\nnothing:\n";

    #[test]
    fn replaces_only_the_nested_key_and_keeps_the_comment() {
        let new = replaced(VALUES, ".image.tag", "abc").unwrap().unwrap();
        assert!(new.contains("  tag: \"abc\"   # pin\n"));
        assert!(new.contains("    tag: \"old\"\n"));
        assert_eq!(new.lines().count(), VALUES.lines().count());
    }

    #[test]
    fn an_empty_string_is_a_key_and_null_is_not() {
        assert!(
            replaced(VALUES, ".image.digest", "sha256:1")
                .unwrap()
                .is_some()
        );
        assert!(replaced(VALUES, ".nothing", "x").unwrap().is_none());
        assert!(replaced(VALUES, ".missing.key", "x").unwrap().is_none());
    }

    #[test]
    fn a_flow_style_key_is_an_error() {
        let text = "image: { tag: old }\n";
        assert!(replaced(text, ".image.tag", "new").is_err());
    }

    #[test]
    fn reads_a_nested_scalar() {
        assert_eq!(get(VALUES, ".worker.image.repository").unwrap(), "r/worker");
        assert_eq!(get(VALUES, ".worker.missing").unwrap(), "");
    }
}
