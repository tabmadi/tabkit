//! `gen/data-classes`: the registry of classified columns and their retention, from the migrations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;

use super::Output;
use crate::config::{Config, Paths};
use crate::repo::Repo;
use crate::yaml;

const ID: &str = "gen/data-classes";

static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)comment\s+on\s+column\s+([a-z0-9_."]+)\.([a-z0-9_"]+)\s+is\s+'pii:([a-z_]+)'"#,
    )
    .expect("a built-in pattern compiles")
});

struct Policy {
    period: String,
    disposal: String,
    note: String,
}

pub fn run(repo: &Repo, cfg: &Config) -> Result<Vec<Output>> {
    let Some(output) = cfg.option_str(ID, "output")? else {
        return Ok(Vec::new());
    };
    let retention = cfg
        .option_str(ID, "retention")?
        .unwrap_or_else(|| ".config/retention.yaml".to_owned());
    let Some(text) = repo.read(&retention) else {
        return Ok(Vec::new());
    };
    let piece = |key: &str| -> Result<String> {
        match cfg.option_str(ID, key)? {
            Some(path) => repo.read(&path).with_context(|| format!("read {path}")),
            None => Ok(String::new()),
        }
    };
    let (header, footer) = (piece("header")?, piece("footer")?);
    let policies = load_policies(&text).with_context(|| format!("parse {retention}"))?;
    let migrations =
        Paths::new(&cfg.option_strings(ID, "migrations", &["services/*/migrations/*.sql"])?)?;
    let columns = scan(repo, &migrations);
    if columns.is_empty() {
        bail!("no tagged columns: the registry would be empty");
    }
    for ((service, table, name), class) in &columns {
        if !policies.contains_key(class) {
            bail!(
                "{service}.{table}.{name} is tagged pii:{class}, which {retention} does not define"
            );
        }
    }
    Ok(vec![Output {
        path: output,
        bytes: render(&columns, &policies, &header, &footer).into_bytes(),
    }])
}

fn load_policies(text: &str) -> Result<BTreeMap<String, Policy>> {
    let docs = yaml::load(text)?;
    let mut out = BTreeMap::new();
    for doc in &docs {
        for (key, classes) in yaml::pairs(doc) {
            if yaml::string(key) != Some("classes") {
                continue;
            }
            for (name, policy) in yaml::pairs(classes) {
                let field = |wanted: &str| {
                    yaml::pairs(policy)
                        .find(|(k, _)| yaml::string(k) == Some(wanted))
                        .and_then(|(_, v)| yaml::text(v))
                        .unwrap_or_default()
                };
                out.insert(
                    yaml::text(name).unwrap_or_default(),
                    Policy {
                        period: field("period"),
                        disposal: field("disposal"),
                        note: field("note"),
                    },
                );
            }
        }
    }
    Ok(out)
}

/// Every classified column, by service, table, and name. A later migration reclassifies a column.
fn scan(repo: &Repo, migrations: &Paths) -> BTreeMap<(String, String, String), String> {
    let mut latest = BTreeMap::new();
    for path in repo.files.iter().filter(|f| migrations.matches(f)) {
        let Some(service) = path.split('/').rev().nth(2) else {
            continue;
        };
        let Some(raw) = repo.read(path) else {
            continue;
        };
        // The `migrate:down` half removes the tags, so it is not read.
        let body = raw.split("-- migrate:down").next().unwrap_or(&raw);
        for m in TAG.captures_iter(body) {
            let key = (service.to_owned(), unquote(&m[1]), unquote(&m[2]));
            latest.insert(key, m[3].to_lowercase());
        }
    }
    latest
}

fn unquote(text: &str) -> String {
    let clean = text.replace('"', "");
    clean.rsplit('.').next().unwrap_or(&clean).to_owned()
}

fn render(
    columns: &BTreeMap<(String, String, String), String>,
    policies: &BTreeMap<String, Policy>,
    header: &str,
    footer: &str,
) -> String {
    let mut b = header.to_owned();
    b.push_str("\n## Policy per class\n\n| Class | Retention | At the end | Why |\n| --- | --- | --- | --- |\n");
    for (name, p) in policies {
        let _ = writeln!(
            b,
            "| `{name}` | {} | {} | {} |",
            p.period, p.disposal, p.note
        );
    }
    let services: BTreeSet<&String> = columns.keys().map(|(s, _, _)| s).collect();
    let _ = write!(
        b,
        "\n## Classified columns\n\n{} columns across {} services.\n\n| Service | Column | Class | Retention | At the end |\n| --- | --- | --- | --- | --- |\n",
        columns.len(),
        services.len()
    );
    for ((service, table, name), class) in columns {
        let p = &policies[class];
        let _ = writeln!(
            b,
            "| {service} | `{table}.{name}` | `{class}` | {} | {} |",
            p.period, p.disposal
        );
    }
    b.push_str(footer);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_names_its_table_and_column() {
        let m = TAG
            .captures(r#"COMMENT ON COLUMN app."users".email IS 'pii:contact';"#)
            .unwrap();
        assert_eq!(unquote(&m[1]), "users");
        assert_eq!(unquote(&m[2]), "email");
        assert_eq!(&m[3], "contact");
    }
}
