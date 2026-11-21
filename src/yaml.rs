//! YAML with source lines, through saphyr. A rule that reports a line reads this view.

use anyhow::{Result, anyhow};
use saphyr::{LoadableYamlNode, MarkedYaml, Scalar, YamlData};

/// Every document of a YAML text.
pub fn load(text: &str) -> Result<Vec<MarkedYaml<'_>>> {
    MarkedYaml::load_from_str(text).map_err(|e| anyhow!("{e}"))
}

/// The text of a string scalar.
pub fn string<'a>(node: &'a MarkedYaml<'_>) -> Option<&'a str> {
    match &node.data {
        YamlData::Value(Scalar::String(s)) => Some(s),
        _ => None,
    }
}

/// The key and value pairs of a mapping, in source order.
pub fn pairs<'a, 'b>(
    node: &'a MarkedYaml<'b>,
) -> impl Iterator<Item = (&'a MarkedYaml<'b>, &'a MarkedYaml<'b>)> {
    let map = match &node.data {
        YamlData::Mapping(map) => Some(map),
        _ => None,
    };
    map.into_iter().flat_map(|m| m.iter())
}

/// The items of a sequence.
pub fn items<'a, 'b>(node: &'a MarkedYaml<'b>) -> &'a [MarkedYaml<'b>] {
    match &node.data {
        YamlData::Sequence(items) => items,
        _ => &[],
    }
}

/// Every direct child of a node: the items of a sequence, or the values of a mapping.
pub fn children<'a, 'b>(node: &'a MarkedYaml<'b>) -> Vec<&'a MarkedYaml<'b>> {
    match &node.data {
        YamlData::Sequence(items) => items.iter().collect(),
        YamlData::Mapping(map) => map.values().collect(),
        _ => Vec::new(),
    }
}

/// The line of the first content of a scalar. A block scalar starts one line after its `|` or `>` indicator.
pub fn content_line(node: &MarkedYaml<'_>, text: &str) -> usize {
    let line = node.span.start.line();
    let block = text
        .lines()
        .nth(line.saturating_sub(1))
        .and_then(|l| l.chars().nth(node.span.start.col()))
        .is_some_and(|c| c == '|' || c == '>');
    line + usize::from(block)
}
