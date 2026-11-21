//! A small YAML writer for generated files: block style, two-space indent, and a stable order of keys.

use std::cmp::Ordering;

/// A YAML value. A `Map` sorts its keys when it is written, and a `Struct` keeps the order it was given.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
    Struct(Vec<(String, Node)>),
}

impl From<&str> for Node {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for Node {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

/// Builds a `Map` from key and value pairs.
pub fn map<const N: usize>(pairs: [(&str, Node); N]) -> Node {
    Node::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// Writes a value as a YAML document.
pub fn to_string(node: &Node) -> String {
    let mut out = String::new();
    write_node(&mut out, node, 0, Position::Root);
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Position {
    Root,
    Value,
    Item,
}

fn entries(node: &Node) -> Option<Vec<&(String, Node)>> {
    match node {
        Node::Map(pairs) => {
            let mut sorted: Vec<_> = pairs.iter().collect();
            sorted.sort_by(|a, b| natural(&a.0, &b.0));
            Some(sorted)
        }
        Node::Struct(pairs) => Some(pairs.iter().collect()),
        _ => None,
    }
}

fn write_node(out: &mut String, node: &Node, indent: usize, position: Position) {
    let pad = " ".repeat(indent);
    match node {
        Node::Str(s) => finish(out, &scalar(s), position),
        Node::Int(n) => finish(out, &n.to_string(), position),
        Node::Bool(b) => finish(out, &b.to_string(), position),
        Node::List(items) if items.is_empty() => finish(out, "[]", position),
        Node::List(items) => {
            if position == Position::Value {
                out.push('\n');
            }
            for (i, item) in items.iter().enumerate() {
                if i > 0 || position != Position::Item {
                    out.push_str(&pad);
                }
                out.push_str("- ");
                write_node(out, item, indent + 2, Position::Item);
            }
        }
        Node::Map(_) | Node::Struct(_) => {
            let pairs = entries(node).unwrap_or_default();
            if pairs.is_empty() {
                finish(out, "{}", position);
                return;
            }
            if position == Position::Value {
                out.push('\n');
            }
            for (i, (key, value)) in pairs.into_iter().enumerate() {
                if i > 0 || position != Position::Item {
                    out.push_str(&pad);
                }
                out.push_str(&scalar(key));
                out.push(':');
                let nested = matches!(value, Node::List(l) if !l.is_empty())
                    || entries(value).is_some_and(|e| !e.is_empty());
                if !nested {
                    out.push(' ');
                }
                write_node(out, value, indent + 2, Position::Value);
            }
        }
    }
}

fn finish(out: &mut String, text: &str, _position: Position) {
    out.push_str(text);
    out.push('\n');
}

/// A string in plain style when YAML reads it back as the same string, and in quotes otherwise.
fn scalar(text: &str) -> String {
    if text.is_empty() {
        return "\"\"".to_owned();
    }
    if plain_allowed(text) {
        return text.to_owned();
    }
    if text.chars().any(char::is_control) {
        return format!("{text:?}");
    }
    format!("'{}'", text.replace('\'', "''"))
}

fn plain_allowed(text: &str) -> bool {
    if text.is_empty() || text.starts_with(' ') || text.ends_with(' ') {
        return false;
    }
    let lower = text.to_lowercase();
    if matches!(
        lower.as_str(),
        "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n"
    ) || text.parse::<f64>().is_ok()
    {
        return false;
    }
    let first = text.chars().next().unwrap_or(' ');
    if "!&*-?|>@`\"'%#{}[],:".contains(first)
        && !(first == '-' && text.len() > 1 && !text.starts_with("- "))
    {
        return false;
    }
    !text.contains(": ")
        && !text.contains(" #")
        && !text.ends_with(':')
        && !text.chars().any(char::is_control)
}

/// The key order of the source generators: digits compare as numbers, and a non-letter sorts before a letter.
fn natural(a: &str, b: &str) -> Ordering {
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut digits = false;
    for i in 0..x.len().min(y.len()) {
        if x[i] == y[i] {
            digits = x[i].is_ascii_digit();
            continue;
        }
        let (xl, yl) = (x[i].is_alphabetic(), y[i].is_alphabetic());
        if xl && yl {
            return x[i].cmp(&y[i]);
        }
        if xl || yl {
            let x_first = if digits { xl } else { yl };
            return if x_first {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        if x[i].is_ascii_digit() || y[i].is_ascii_digit() {
            let run = |s: &[char], from: usize| {
                s[from..]
                    .iter()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            };
            if let (Some(m), Some(n)) = (run(&x, i), run(&y, i))
                && m != n
            {
                return m.cmp(&n);
            }
        }
        return x[i].cmp(&y[i]);
    }
    x.len().cmp(&y.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_document() {
        let doc = Node::Struct(vec![
            ("id".to_owned(), "page".into()),
            (
                "blocks".to_owned(),
                Node::List(vec![map([
                    ("b", Node::Int(1)),
                    ("a", Node::Bool(true)),
                    ("list", Node::List(vec!["x".into()])),
                ])]),
            ),
            ("empty".to_owned(), Node::Map(Vec::new())),
            ("text".to_owned(), "Editing id: ".into()),
        ]);
        assert_eq!(
            to_string(&doc),
            "id: page\nblocks:\n  - a: true\n    b: 1\n    list:\n      - x\nempty: {}\ntext: 'Editing id: '\n"
        );
    }

    #[test]
    fn keys_sort_naturally() {
        assert_eq!(natural("a2", "a10"), Ordering::Less);
        assert_eq!(natural("_state", "id"), Ordering::Less);
    }
}
