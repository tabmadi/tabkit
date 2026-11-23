//! `sql/*`, `alert/*`, `api/*`, `edge/*`, and `naming/*`: rules over the data, API, and infrastructure files.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use saphyr::{MarkedYaml, Scalar, YamlData};
use serde_json::Value;

use super::Ctx;
use tabkit_core::finding::Finding;
use tabkit_core::yaml;

const PII: &str = "sql/pii-tag";
const SEVERITY: &str = "alert/severity";
const RUNBOOK: &str = "alert/runbook";
const AUDIENCE: &str = "api/audience";
const PREFIXES: &str = "api/prefixes";
const WILDCARD: &str = "edge/api-wildcard";
const AUTHORIZER: &str = "edge/authorizer";
const NAMING: &str = "naming/resource";
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "patch", "options", "head", "trace",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = pii(ctx)?;
    out.extend(severity(ctx)?);
    out.extend(runbook(ctx)?);
    out.extend(api(ctx)?);
    out.extend(edge(ctx)?);
    out.extend(naming(ctx)?);
    Ok(out)
}

fn one(ctx: &Ctx<'_>, rule: &str, key: &str, default: &str) -> Result<String> {
    Ok(ctx
        .cfg
        .option_str(rule, key)?
        .unwrap_or_else(|| default.to_owned()))
}

fn get<'a, 'b>(node: &'a MarkedYaml<'b>, key: &str) -> Option<&'a MarkedYaml<'b>> {
    yaml::pairs(node)
        .find(|(k, _)| yaml::string(k) == Some(key))
        .map(|(_, v)| v)
}

fn text(node: &MarkedYaml<'_>, key: &str) -> String {
    get(node, key).and_then(yaml::text).unwrap_or_default()
}

const PII_INDICATORS: [&str; 28] = [
    "email",
    "phone",
    "mobile",
    "address",
    "postcode",
    "zip",
    "first_name",
    "last_name",
    "full_name",
    "given_name",
    "family_name",
    "surname",
    "user_id",
    "subject_id",
    "identity_id",
    "customer_id",
    "person",
    "dob",
    "birth",
    "national_id",
    "passport",
    "ssn",
    "tax_id",
    "ip_address",
    "user_agent",
    "device_id",
    "latitude",
    "longitude",
];
const PII_CLASSES: [&str; 10] = [
    "none",
    "contact",
    "name",
    "identifier",
    "government_id",
    "financial",
    "location",
    "device",
    "special_category",
    "free_text",
];
const CONSTRAINTS: [&str; 6] = [
    "primary key",
    "foreign key",
    "unique",
    "check",
    "constraint",
    "exclude",
];

static CREATE_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?is)create\s+table\s+(?:if\s+not\s+exists\s+)?([a-z0-9_."]+)\s*\((.*?)\n\s*\)\s*;"#)
});
static PII_COMMENT: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?i)comment\s+on\s+column\s+([a-z0-9_."]+)\.([a-z0-9_"]+)\s+is\s+'pii:([a-z_]+)'"#)
});
static COLUMN: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)^\s*([a-z0-9_"]+)\s+[a-z]"#));

fn unquote(name: &str) -> String {
    let clean = name.replace('"', "");
    clean.rsplit('.').next().unwrap_or(&clean).to_owned()
}

fn column_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("--") {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if CONSTRAINTS.iter().any(|k| lower.starts_with(k)) {
        return None;
    }
    COLUMN.captures(trimmed).map(|m| unquote(&m[1]))
}

/// A column whose name suggests personal data carries a `pii:<class>` comment in the migration set.
fn pii(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(PII) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(PII, "paths", &["services/*/migrations/*.sql"])?;
    let (mut columns, mut tagged) = (BTreeMap::new(), BTreeSet::new());
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| paths.matches(f)) {
        let Some(service) = path.split('/').rev().nth(2) else {
            continue;
        };
        let sql = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        for m in CREATE_TABLE.captures_iter(&sql) {
            let table = unquote(&m[1]);
            for line in m[2].split('\n') {
                if let Some(name) = column_name(line) {
                    columns.insert(format!("{service}:{table}.{name}"), path.as_str());
                }
            }
        }
        for m in PII_COMMENT.captures_iter(&sql) {
            let key = format!("{service}:{}.{}", unquote(&m[1]), unquote(&m[2]));
            if PII_CLASSES.contains(&&m[3]) {
                tagged.insert(key);
            } else {
                let message = format!(
                    "{key} is tagged pii:{}, which is not a class. Use one of: {}",
                    &m[3],
                    PII_CLASSES.join(", ")
                );
                out.push(Finding::new(path, 0, PII, message));
            }
        }
    }
    for (key, path) in &columns {
        let column = key.rsplit('.').next().unwrap_or(key).to_lowercase();
        if PII_INDICATORS.iter().any(|i| column.contains(i)) && !tagged.contains(key) {
            let message = format!(
                "{key} holds personal data by its name and has no pii: tag. Add one, or pii:none if it does not"
            );
            out.push(Finding::new(path, 0, PII, message));
        }
    }
    Ok(out)
}

/// Every alert has a severity from the routing tree, and a dead man's switch exists.
fn severity(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(SEVERITY) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(SEVERITY, "paths", &["infra/observability/alerts/*.yaml"])?;
    let allowed = ctx
        .cfg
        .option_strings(SEVERITY, "allowed", &["page", "ticket"])?;
    let watchdog = one(ctx, SEVERITY, "watchdog", "Watchdog")?;
    let files: Vec<&String> = ctx.repo.files.iter().filter(|f| paths.matches(f)).collect();
    let Some(first) = files.first() else {
        return Ok(Vec::new());
    };
    let mut found = false;
    let mut out = Vec::new();
    for path in &files {
        let source = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        let docs = match yaml::load(&source) {
            Ok(docs) => docs,
            Err(e) => {
                out.push(Finding::new(path, 0, SEVERITY, format!("parse: {e}")));
                continue;
            }
        };
        let Some(doc) = docs.first() else {
            continue;
        };
        let groups = get(doc, "groups").map_or(&[][..], yaml::items);
        for group in groups {
            for rule in get(group, "rules").map_or(&[][..], yaml::items) {
                let alert = text(rule, "alert");
                if alert.is_empty() {
                    continue;
                }
                found |= alert == watchdog;
                let line = get(rule, "alert").map_or(0, |n| n.span.start.line());
                match get(rule, "labels").and_then(|l| get(l, "severity")) {
                    None => {
                        let message = format!("{alert} has no severity");
                        out.push(Finding::new(path, line, SEVERITY, message));
                    }
                    Some(node) => {
                        let value = yaml::text(node).unwrap_or_default();
                        if !allowed.contains(&value) {
                            let message = format!(
                                "{alert} has severity `{value}`, which is not {}",
                                allowed.join(" or ")
                            );
                            out.push(Finding::new(path, line, SEVERITY, message));
                        }
                    }
                }
            }
        }
    }
    if !found {
        let message = format!("no {watchdog} rule. Nothing detects a dead alerting pipeline");
        out.push(Finding::new(first, 0, SEVERITY, message));
    }
    Ok(out)
}

/// The repo path of a runbook reference: a relative path, or a forge link with `/blob/<branch>/`.
fn runbook_path(target: &str) -> &str {
    let clean = target.split('#').next().unwrap_or(target);
    match clean.split_once("/blob/") {
        Some((_, rest)) => rest.split_once('/').map_or(rest, |(_, path)| path),
        None => clean.trim_start_matches("./"),
    }
}

/// Every alert names a runbook that exists in this repo.
fn runbook(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(RUNBOOK) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(RUNBOOK, "paths", &["infra/observability/alerts/*.yaml"])?;
    let annotation = one(ctx, RUNBOOK, "annotation", "runbook")?;
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| paths.matches(f)) {
        let source = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        let docs = match yaml::load(&source) {
            Ok(docs) => docs,
            Err(e) => {
                out.push(Finding::new(path, 0, RUNBOOK, format!("parse: {e}")));
                continue;
            }
        };
        let Some(doc) = docs.first() else {
            continue;
        };
        for group in get(doc, "groups").map_or(&[][..], yaml::items) {
            for rule in get(group, "rules").map_or(&[][..], yaml::items) {
                let alert = text(rule, "alert");
                if alert.is_empty() {
                    continue;
                }
                let line = get(rule, "alert").map_or(0, |n| n.span.start.line());
                let target = get(rule, "annotations")
                    .map(|a| text(a, &annotation))
                    .unwrap_or_default();
                if target.is_empty() {
                    let message = format!("{alert} names no runbook");
                    out.push(Finding::new(path, line, RUNBOOK, message));
                } else if !ctx.repo.files.iter().any(|f| f == runbook_path(&target)) {
                    let message =
                        format!("{alert} names `{target}`, which is not a file of this repo");
                    out.push(Finding::new(path, line, RUNBOOK, message));
                }
            }
        }
    }
    Ok(out)
}

/// The resources that a service routes at the edge, with whether its ingress is on.
fn routes(ctx: &Ctx<'_>, template: &str, service: &str) -> Result<Option<(bool, Vec<String>)>> {
    let path = template.replace("{service}", service);
    let Some(source) = ctx.repo.read(&path) else {
        return Ok(None);
    };
    let docs = yaml::load(&source).with_context(|| format!("parse {path}"))?;
    let ingress = docs.first().and_then(|d| get(d, "ingress"));
    let enabled = ingress
        .and_then(|i| get(i, "enabled"))
        .is_none_or(|e| !matches!(e.data, YamlData::Value(Scalar::Boolean(false))));
    let resources = ingress
        .and_then(|i| get(i, "resources"))
        .map(|r| yaml::items(r).iter().filter_map(yaml::text).collect())
        .unwrap_or_default();
    Ok(Some((enabled, resources)))
}

/// Each operation with its path and its audience: its own, then the service default, then `cluster`.
fn operations(doc: &MarkedYaml<'_>) -> Vec<(String, String)> {
    let default = get(doc, "info")
        .map(|i| text(i, "x-audience"))
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "cluster".to_owned());
    let mut out = Vec::new();
    let Some(paths) = get(doc, "paths") else {
        return out;
    };
    for (path, methods) in yaml::pairs(paths) {
        let path = yaml::text(path).unwrap_or_default();
        for (method, operation) in yaml::pairs(methods) {
            let method = yaml::text(method).unwrap_or_default();
            if !METHODS.contains(&method.as_str()) {
                continue;
            }
            let own = text(operation, "x-audience");
            let audience = if own.is_empty() { default.clone() } else { own };
            out.push((path.clone(), audience));
        }
    }
    out
}

fn api(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let (audience_on, prefixes_on) = (ctx.cfg.enabled(AUDIENCE), ctx.cfg.enabled(PREFIXES));
    if !audience_on && !prefixes_on {
        return Ok(Vec::new());
    }
    let specs = ctx.paths(AUDIENCE, "specs", &["services/*/openapi.yaml"])?;
    let values = one(
        ctx,
        AUDIENCE,
        "values",
        "infra/gitops/services/dev/values/{service}.yaml",
    )?;
    let mut claims: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut first_spec = None;
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| specs.matches(f)) {
        let Some(service) = path.split('/').rev().nth(1) else {
            continue;
        };
        first_spec.get_or_insert(path.as_str());
        let source = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        let docs = yaml::load(&source).with_context(|| format!("parse {path}"))?;
        let Some(doc) = docs.first() else {
            continue;
        };
        let ops = operations(doc);
        let route = routes(ctx, &values, service)?;
        if audience_on {
            let exposed = route.as_ref().is_some_and(|(on, r)| *on && !r.is_empty());
            for message in audience_problems(&ops, exposed) {
                out.push(Finding::new(path, 0, AUDIENCE, message));
            }
        }
        if prefixes_on {
            let routed: Vec<String> = route
                .filter(|(on, _)| *on)
                .map(|(_, r)| r)
                .unwrap_or_default();
            if routed.is_empty() {
                continue;
            }
            let served: BTreeSet<&str> = ops
                .iter()
                .filter(|(_, a)| a != "cluster")
                .filter_map(|(p, _)| {
                    let trimmed = p.strip_prefix('/').unwrap_or(p);
                    trimmed.split('/').next().filter(|s| !s.is_empty())
                })
                .collect();
            for resource in &routed {
                claims
                    .entry(resource.clone())
                    .or_default()
                    .push(service.to_owned());
                if !served.contains(resource.as_str()) {
                    let message = format!(
                        "routes `{resource}` at the edge but its spec has no /{resource} path"
                    );
                    out.push(Finding::new(path, 0, PREFIXES, message));
                }
            }
            for prefix in served.iter().filter(|p| !routed.iter().any(|r| r == *p)) {
                let message = format!(
                    "spec serves /{prefix} but the edge route table does not claim `{prefix}`"
                );
                out.push(Finding::new(path, 0, PREFIXES, message));
            }
        }
    }
    if let Some(spec) = first_spec {
        for (resource, claimants) in claims.iter().filter(|(_, c)| c.len() > 1) {
            let message = format!(
                "resource `{resource}` is claimed by {}. One flat /api namespace allows one service for each resource",
                claimants.join(", ")
            );
            out.push(Finding::new(spec, 0, PREFIXES, message));
        }
    }
    Ok(out)
}

fn audience_problems(ops: &[(String, String)], exposed: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut has_edge = false;
    for (_, audience) in ops {
        match audience.as_str() {
            "cluster" => {}
            "internal" | "public" => has_edge = true,
            other => out.push(format!(
                "unknown x-audience `{other}`, expected cluster, internal, or public"
            )),
        }
    }
    if has_edge && !exposed {
        out.push(
            "has edge operations, with x-audience internal or public, but no /api route".to_owned(),
        );
    } else if !has_edge && exposed {
        out.push("is edge-exposed but every operation is x-audience: cluster".to_owned());
    }
    out
}

static BARE_WILDCARD: LazyLock<Regex> = LazyLock::new(|| re(r"/api/(<\*|\*)"));

fn edge(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let (wildcard_on, authorizer_on) = (ctx.cfg.enabled(WILDCARD), ctx.cfg.enabled(AUTHORIZER));
    if !wildcard_on && !authorizer_on {
        return Ok(Vec::new());
    }
    let path = one(
        ctx,
        WILDCARD,
        "access-rules",
        "infra/auth/oathkeeper/access-rules.json",
    )?;
    let Some(source) = ctx.repo.read(&path) else {
        return Ok(Vec::new());
    };
    let rules: Value = serde_json::from_str(&source).with_context(|| format!("parse {path}"))?;
    let mut out = Vec::new();
    for rule in rules.as_array().into_iter().flatten() {
        let id = rule["id"].as_str().unwrap_or_default();
        let url = rule["match"]["url"].as_str().unwrap_or_default();
        if wildcard_on && BARE_WILDCARD.is_match(url) {
            let message = format!(
                "{id}: match url `{url}` has a bare wildcard after /api/. List the resources instead: /api/<{{a,b,c}}>"
            );
            out.push(Finding::new(&path, 0, WILDCARD, message));
        }
        let handler = rule["authorizer"]["handler"].as_str().unwrap_or_default();
        if authorizer_on && id.starts_with("ops-") && handler != "remote_json" {
            let message = format!("{id}: authorizer is `{handler}`, expected `remote_json`");
            out.push(Finding::new(&path, 0, AUTHORIZER, message));
        }
    }
    Ok(out)
}

static SLUG: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9]*(-[a-z0-9]+)*$"));
static ROLE_HEADER: LazyLock<Regex> = LazyLock::new(|| re(r"^\|\s*`role`\s*\|\s*Names\s*\|$"));
static ROLE_ROW: LazyLock<Regex> = LazyLock::new(|| re(r"^\|\s*`([a-z0-9-]+)`\s*\|"));
static ENV_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:&cluster_([a-z]+)|platform/([a-z]+)/secrets)"));

fn parse_roles(text: &str) -> BTreeSet<String> {
    let mut roles = BTreeSet::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if ROLE_HEADER.is_match(line) {
            inside = true;
            continue;
        }
        if inside {
            if let Some(m) = ROLE_ROW.captures(line) {
                roles.insert(m[1].to_owned());
            } else if !line.starts_with("| ---") {
                inside = false;
            }
        }
    }
    roles
}

fn host_problem(host: &str, env: &str, roles: &BTreeSet<String>) -> Option<String> {
    if !SLUG.is_match(host) {
        return Some(format!(
            "host `{host}` is not a slug: ^[a-z][a-z0-9]*(-[a-z0-9]+)*$"
        ));
    }
    if host.len() > 63 {
        return Some(format!(
            "host `{host}` is {} characters, and the limit is 63",
            host.len()
        ));
    }
    let parts: Vec<&str> = host.split('-').collect();
    if parts.len() < 3 {
        return Some(format!(
            "host `{host}` is not {{project}}-{{env}}-{{role}}[-{{n}}]"
        ));
    }
    let (project, got_env, role) = (parts[0], parts[1], parts[2]);
    if !(5..=11).contains(&project.len()) {
        return Some(format!(
            "host `{host}`: project slug `{project}` is {} characters, and the limit is 5 to 11",
            project.len()
        ));
    }
    if got_env != env {
        return Some(format!(
            "host `{host}` names env `{got_env}` but lives in inventory/{env}"
        ));
    }
    if !roles.contains(role) {
        return Some(format!(
            "host `{host}`: `{role}` is not a role in the role table"
        ));
    }
    if parts.len() > 4 {
        return Some(format!(
            "host `{host}` has more segments than {{project}}-{{env}}-{{role}}-{{n}}"
        ));
    }
    if parts.len() == 4 && parts[3].parse::<i64>().is_err() {
        return Some(format!(
            "host `{host}`: `{}` is not an ordinal. Names have no descriptive suffix",
            parts[3]
        ));
    }
    None
}

fn collect_hosts(node: &MarkedYaml<'_>, out: &mut Vec<String>) {
    for (key, value) in yaml::pairs(node) {
        if yaml::string(key) == Some("nodes") && matches!(value.data, YamlData::Mapping(_)) {
            out.extend(yaml::pairs(value).filter_map(|(k, _)| yaml::text(k)));
        }
    }
    for child in yaml::children(node) {
        collect_hosts(child, out);
    }
}

/// The direct subdirectories of a directory, from the tracked files.
fn subdirs<'a>(ctx: &'a Ctx<'_>, dir: &str) -> Vec<&'a str> {
    let prefix = format!("{dir}/");
    ctx.repo
        .dirs
        .iter()
        .filter_map(|d| d.strip_prefix(&prefix))
        .filter(|d| !d.contains('/'))
        .collect()
}

/// Resource names follow `{project}-{env}-{role}[-{n}]`, with roles from the table of a decision record.
#[expect(clippy::too_many_lines, reason = "three short scans of one grammar")]
fn naming(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(NAMING) {
        return Ok(Vec::new());
    }
    let roles_doc = one(
        ctx,
        NAMING,
        "roles-doc",
        "docs/adr/0003-naming-and-identifiers.md",
    )?;
    let Some(doc) = ctx.repo.read(&roles_doc) else {
        return Ok(Vec::new());
    };
    let roles = parse_roles(&doc);
    let mut out = Vec::new();
    if roles.is_empty() {
        out.push(Finding::new(&roles_doc, 0, NAMING, "no role table found"));
        return Ok(out);
    }
    let envs = ctx
        .cfg
        .option_strings(NAMING, "environments", &["dev", "staging", "prod"])?;
    let local = one(ctx, NAMING, "local-tier", "local")?;
    let inventory = one(ctx, NAMING, "inventory", "infra/talos/inventory")?;
    for env in subdirs(ctx, &inventory) {
        if !envs.iter().any(|e| e == env) {
            let message = format!(
                "`{env}` is not an environment. Environments: {}",
                envs.join(", ")
            );
            out.push(Finding::new(
                &format!("{inventory}/{env}"),
                0,
                NAMING,
                message,
            ));
            continue;
        }
        let path = format!("{inventory}/{env}/nodes.yml");
        let source = ctx
            .repo
            .read(&path)
            .with_context(|| format!("read {path}"))?;
        let docs = yaml::load(&source).with_context(|| format!("parse {path}"))?;
        let mut hosts = Vec::new();
        for d in &docs {
            collect_hosts(d, &mut hosts);
        }
        if hosts.is_empty() {
            out.push(Finding::new(&path, 0, NAMING, "no `nodes` mapping"));
            continue;
        }
        let mut project: Option<String> = None;
        for host in &hosts {
            if let Some(problem) = host_problem(host, env, &roles) {
                out.push(Finding::new(&path, 0, NAMING, problem));
                continue;
            }
            let slug = host.split('-').next().unwrap_or(host);
            match &project {
                None => project = Some(slug.to_owned()),
                Some(p) if p != slug => {
                    let message = format!(
                        "host `{host}` uses project slug `{slug}`, but this inventory is `{p}`"
                    );
                    out.push(Finding::new(&path, 0, NAMING, message));
                }
                Some(_) => {}
            }
        }
    }
    let known = |env: &str| envs.iter().any(|e| e == env) || env == local;
    let sops = one(ctx, NAMING, "sops", ".sops.yaml")?;
    if let Some(source) = ctx.repo.read(&sops) {
        let mut seen = BTreeSet::new();
        for m in ENV_TOKEN.captures_iter(&source) {
            let env = m.get(1).or_else(|| m.get(2)).map_or("", |g| g.as_str());
            if !known(env) && seen.insert(env.to_owned()) {
                let message = format!(
                    "`{env}` is not an environment. Write env in full: {}, {local}",
                    envs.join(", ")
                );
                out.push(Finding::new(&sops, 0, NAMING, message));
            }
        }
    }
    let overlays = one(ctx, NAMING, "overlays", "infra/gitops/platform")?;
    for name in subdirs(ctx, &overlays) {
        if !known(name) {
            let message = format!(
                "`{name}` is not an environment. Environments: {}, {local}",
                envs.join(", ")
            );
            out.push(Finding::new(
                &format!("{overlays}/{name}"),
                0,
                NAMING,
                message,
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn runbook_paths() {
        use super::runbook_path;
        assert_eq!(runbook_path("docs/guide/a.md"), "docs/guide/a.md");
        assert_eq!(runbook_path("./docs/guide/a.md#step"), "docs/guide/a.md");
        assert_eq!(
            runbook_path("https://github.com/o/r/blob/main/docs/guide/a.md"),
            "docs/guide/a.md"
        );
    }

    use super::*;

    #[test]
    fn a_column_line_is_read_and_a_constraint_is_not() {
        assert_eq!(
            column_name("  email text NOT NULL,").as_deref(),
            Some("email")
        );
        assert_eq!(
            column_name("  \"user_id\" uuid,").as_deref(),
            Some("user_id")
        );
        assert_eq!(column_name("  PRIMARY KEY (id)"), None);
        assert_eq!(column_name("  -- a comment"), None);
    }

    #[test]
    fn audience_matches_exposure() {
        let ops = |a: &str| vec![("/a".to_owned(), a.to_owned())];
        assert_eq!(
            audience_problems(&ops("public"), true),
            Vec::<String>::new()
        );
        assert_eq!(
            audience_problems(&ops("cluster"), false),
            Vec::<String>::new()
        );
        assert_eq!(audience_problems(&ops("public"), false).len(), 1);
        assert_eq!(audience_problems(&ops("cluster"), true).len(), 1);
        assert_eq!(audience_problems(&ops("nope"), false).len(), 1);
    }

    #[test]
    fn host_names() {
        let roles: BTreeSet<String> = ["cp".to_owned()].into();
        assert_eq!(host_problem("acmeco-dev-cp-1", "dev", &roles), None);
        for bad in [
            "Acme-dev-cp",
            "acme-dev-cp",
            "acmeco-prod-cp",
            "acmeco-dev-db",
            "acmeco-dev-cp-x",
            "acmeco-dev-cp-1-2",
            "acmeco-dev",
        ] {
            assert!(host_problem(bad, "dev", &roles).is_some(), "{bad}");
        }
    }

    #[test]
    fn a_wildcard_after_api_is_bare() {
        assert!(BARE_WILDCARD.is_match("https://x/api/<**>"));
        assert!(BARE_WILDCARD.is_match("https://x/api/*"));
        assert!(!BARE_WILDCARD.is_match("https://x/api/<{a,b}>"));
    }
}
