//! `helm/*`, `gitops/*`, `service/*`, `project/*`, and the file-based `edge/*` rules: the layout of a service platform.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use saphyr::{MarkedYaml, Scalar, YamlData};
use serde_json::Value;

use super::Ctx;
use tabkit_core::finding::Finding;
use tabkit_core::yaml;

const HELM_FALLBACK: &str = "helm/dependency-fallback";
const APP_VERSION: &str = "helm/app-version";
const IDENTITY: &str = "project/identity";
const AUTH_INLINE: &str = "edge/auth-inline";
const L7_ROUTES: &str = "edge/l7-routes";
const STRIP_HEADERS: &str = "edge/strip-headers";
const ENV_SCOPE: &str = "gitops/env-scope";
const PARITY: &str = "gitops/parity";
const PORTS: &str = "service/ports";
const CONTRACT: &str = "service/contract";
const DATABASES: &str = "service/databases";
const WORKSPACE: &str = "service/workspace-copies";
const NODE_SCOPE: &str = "service/node-scope";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = helm_fallback(ctx)?;
    out.extend(app_versions(ctx)?);
    out.extend(identity(ctx)?);
    out.extend(auth_inline(ctx)?);
    out.extend(l7_routes(ctx)?);
    out.extend(strip_headers(ctx)?);
    out.extend(env_scope(ctx)?);
    out.extend(parity(ctx)?);
    out.extend(ports(ctx)?);
    out.extend(contract(ctx)?);
    out.extend(databases(ctx)?);
    out.extend(workspace_copies(ctx)?);
    out.extend(node_scope(ctx));
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

fn dig<'a, 'b>(node: &'a MarkedYaml<'b>, keys: &[&str]) -> Option<&'a MarkedYaml<'b>> {
    keys.iter().try_fold(node, |n, key| get(n, key))
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

/// The deployable services: each directory under `services/` without a leading underscore.
fn services<'a>(ctx: &'a Ctx<'_>) -> Vec<&'a str> {
    subdirs(ctx, "services")
        .into_iter()
        .filter(|s| !s.starts_with('_'))
        .collect()
}

fn is_file(ctx: &Ctx<'_>, path: &str) -> bool {
    ctx.repo.root.join(path).is_file()
}

/// `helm dependency build` needs a registered repository, so a script falls back to `update`.
fn helm_fallback(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(HELM_FALLBACK) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(HELM_FALLBACK, "paths", &["scripts/**/*.sh"])?;
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| paths.matches(f)) {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("dependency build") || line.trim_start().starts_with('#') {
                continue;
            }
            let next = lines.get(i + 1).copied().unwrap_or("");
            if !line.contains("dependency update") && !next.contains("dependency update") {
                out.push(Finding::new(
                    path,
                    i + 1,
                    HELM_FALLBACK,
                    "`helm dependency build` has no `update` fallback",
                ));
            }
        }
    }
    Ok(out)
}

/// A simple path of keys, such as `.image.tag`, in the first document of a YAML file.
fn yaml_value(ctx: &Ctx<'_>, file: &str, expression: &str) -> Option<String> {
    let text = ctx.repo.read(file)?;
    let docs = yaml::load(&text).ok()?;
    let keys: Vec<&str> = expression.trim_start_matches('.').split('.').collect();
    yaml::text(dig(docs.first()?, &keys)?)
}

fn dependency_version(ctx: &Ctx<'_>, dir: &str, name: &str) -> Option<String> {
    let text = ctx.repo.read(&format!("{dir}/Chart.yaml"))?;
    let docs = yaml::load(&text).ok()?;
    let version = yaml::items(get(docs.first()?, "dependencies")?)
        .iter()
        .find(|d| get(d, "name").and_then(yaml::string) == Some(name))
        .and_then(|d| get(d, "version"))
        .and_then(yaml::text)?;
    let archive = ctx
        .repo
        .root
        .join(format!("{dir}/charts/{name}-{version}.tgz"));
    if !archive.is_file() {
        return None;
    }
    let output = Command::new("tar")
        .arg("-xzOf")
        .arg(&archive)
        .arg(format!("{name}/Chart.yaml"))
        .output()
        .ok()?;
    let chart = String::from_utf8(output.stdout).ok()?;
    let docs = yaml::load(&chart).ok()?;
    yaml::text(get(docs.first()?, "appVersion")?)
}

fn source_version(ctx: &Ctx<'_>, dir: &str, from: &str) -> Option<String> {
    let (kind, rest) = from.split_once(':')?;
    match kind {
        "dependency" => dependency_version(ctx, dir, rest),
        "yaml" => {
            let (file, expression) = rest.split_once(':')?;
            yaml_value(ctx, file, expression)
        }
        "arg" => {
            let (file, name) = rest.split_once(':')?;
            let text = ctx.repo.read(file)?;
            let pattern = Regex::new(&format!(
                r#"(?m)^ARG {}="?([^" \n]+)"?"#,
                regex::escape(name)
            ))
            .ok()?;
            pattern.captures(&text).map(|m| m[1].to_owned())
        }
        _ => None,
    }
    .filter(|v| !v.is_empty())
}

/// The `appVersion` of each chart names the release that the chart deploys.
fn app_versions(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(APP_VERSION) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(APP_VERSION, "paths", &["infra/helm/**/Chart.yaml"])?;
    let mut out = Vec::new();
    for chart in ctx
        .repo
        .files
        .iter()
        .filter(|f| paths.matches(f) && !f.contains("/charts/"))
    {
        let text = ctx
            .repo
            .read(chart)
            .with_context(|| format!("read {chart}"))?;
        let docs = yaml::load(&text).with_context(|| format!("parse {chart}"))?;
        let Some(doc) = docs.first() else {
            continue;
        };
        let from = dig(doc, &["annotations", "app-version-from"]).and_then(yaml::text);
        let app = get(doc, "appVersion")
            .and_then(yaml::text)
            .unwrap_or_default();
        let Some(from) = from.filter(|f| !f.is_empty()) else {
            out.push(Finding::new(
                chart,
                0,
                APP_VERSION,
                "no app-version-from annotation",
            ));
            continue;
        };
        if from == "none" {
            continue;
        }
        let dir = chart.strip_suffix("/Chart.yaml").unwrap_or(chart);
        let Some(want) = source_version(ctx, dir, &from) else {
            let message = format!("no version found at {from}");
            out.push(Finding::new(chart, 0, APP_VERSION, message));
            continue;
        };
        if app.strip_prefix('v').unwrap_or(&app) != want.strip_prefix('v').unwrap_or(&want) {
            let shown = if app.is_empty() { "is empty" } else { &app };
            let message = format!("appVersion {shown}, but {from} is {want}");
            out.push(Finding::new(chart, 0, APP_VERSION, message));
        }
    }
    Ok(out)
}

/// The tracked text files that carry a literal, without the files that name it on purpose.
fn files_with(ctx: &Ctx<'_>, literal: &str, exempt: &tabkit_core::config::Paths) -> usize {
    ctx.repo
        .files
        .iter()
        .filter(|f| !exempt.matches(f))
        .filter(|f| ctx.repo.read(f).is_some_and(|t| t.contains(literal)))
        .count()
}

/// A project no longer carries the identity of the copy it started from.
#[expect(clippy::too_many_lines, reason = "the two states of a copy, in order")]
fn identity(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(IDENTITY) {
        return Ok(Vec::new());
    }
    let footprints = ctx.cfg.option_strings(IDENTITY, "footprints", &[])?;
    let init = one(ctx, IDENTITY, "init-script", "scripts/project-init.sh")?;
    let project = one(ctx, IDENTITY, "project-file", "infra/project.yaml")?;
    let inventory = one(ctx, IDENTITY, "inventory", "infra/talos/inventory")?;
    let apex = one(ctx, IDENTITY, "apex", "example.com")?;
    let exempt = ctx.paths(
        IDENTITY,
        "exempt",
        &[
            "template/**",
            "**/node_modules/**",
            "**/*.local.md",
            ".config/tabkit.toml",
        ],
    )?;
    let mut out = Vec::new();
    if !is_file(ctx, &init) {
        let slug = ctx.repo.read(&project).and_then(|t| {
            t.lines().find_map(|l| {
                let mut fields = l.split_whitespace();
                (fields.next() == Some("slug:"))
                    .then(|| fields.next().map(str::to_owned))
                    .flatten()
            })
        });
        let Some(slug) = slug else {
            out.push(Finding::new(
                &project,
                0,
                IDENTITY,
                "the file carries no slug",
            ));
            return Ok(out);
        };
        let prefix = format!("{inventory}/");
        let named = ctx
            .repo
            .files
            .iter()
            .filter(|f| f.starts_with(&prefix))
            .any(|f| {
                ctx.repo
                    .read(f)
                    .is_some_and(|t| t.contains(&format!("{slug}-")))
            });
        if !named {
            let message = format!(
                "no node is named `{slug}-<role>`. The inventory still has the names of the copy"
            );
            out.push(Finding::new(&inventory, 0, IDENTITY, message));
        }
        for footprint in &footprints {
            let hits = files_with(ctx, footprint, &exempt);
            if hits > 0 {
                let message = format!("{hits} files still carry `{footprint}`");
                out.push(Finding::new(&project, 0, IDENTITY, message));
            }
        }
        return Ok(out);
    }
    let origin = Command::new("git")
        .arg("-C")
        .arg(&ctx.repo.root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    let Some(origin) = origin
        .map(|o| o.trim().to_owned())
        .filter(|o| !o.is_empty())
    else {
        return Ok(out);
    };
    let repo_name = origin
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(&origin)
        .trim_end_matches(".git");
    let Some(module) = ctx.repo.read("go.mod").and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("module ").map(|m| m.trim().to_owned()))
    }) else {
        return Ok(out);
    };
    let mut segments = module.rsplit('/');
    let last = segments.next().unwrap_or(&module);
    let versioned =
        last.len() > 1 && last.starts_with('v') && last[1..].chars().all(|c| c.is_ascii_digit());
    let module_name = if versioned {
        segments.next().unwrap_or(last)
    } else {
        last
    };
    if module_name != repo_name {
        let message = format!(
            "the repository is `{repo_name}`, and the module path says `{module_name}`. {} files carry the module path and {} carry `{apex}`. Adopt an identity before the first push",
            files_with(ctx, &module, &exempt),
            files_with(ctx, &apex, &exempt)
        );
        out.push(Finding::new("go.mod", 0, IDENTITY, message));
    }
    Ok(out)
}

static AUTH_MARKERS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)accessRules|identitySchemas|default_schema_id|authenticators:|access_rules:|selfservice:|cookie_session|ory_kratos_session",
    )
});
static AUTH_BYPASS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)DEV_AUTH|AUTH_BYPASS|BYPASS_AUTH|dev-?auth|auth-?bypass|fake[-_ ]?session|NODE_ENV.*(session|auth|identity|bypass)|(session|auth|identity|bypass).*NODE_ENV",
    )
});

/// Authentication is defined in one place, and the frontend has one auth path.
fn auth_inline(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(AUTH_INLINE) {
        return Ok(Vec::new());
    }
    let values = one(
        ctx,
        AUTH_INLINE,
        "values",
        "infra/helm/platform/ory/values.yaml",
    )?;
    let Some(text) = ctx.repo.read(&values) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if AUTH_MARKERS.is_match(line) && !line.trim_start().starts_with('#') {
            out.push(Finding::new(
                &values,
                i + 1,
                AUTH_INLINE,
                "auth config is inline. It lives only in the auth directory",
            ));
        }
    }
    for path in ctx.cfg.option_strings(
        AUTH_INLINE,
        "required",
        &[
            "infra/auth/kratos/values.yaml",
            "infra/auth/oathkeeper/values.yaml",
            "infra/auth/oathkeeper/access-rules.json",
            "infra/auth/kratos/identity-schemas/user.v1.json",
        ],
    )? {
        if !is_file(ctx, &path) {
            out.push(Finding::new(
                &path,
                0,
                AUTH_INLINE,
                "the canonical auth file is missing",
            ));
        }
    }
    let source = ctx.paths(AUTH_INLINE, "source", &["apps/frontend/src/**"])?;
    for path in ctx.repo.files.iter().filter(|f| source.matches(f)) {
        let Some(text) = ctx.repo.read(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            let comment =
                trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*");
            if !comment && AUTH_BYPASS.is_match(line) {
                out.push(Finding::new(
                    path,
                    i + 1,
                    AUTH_INLINE,
                    "development-only auth code. The frontend has one auth path",
                ));
            }
        }
    }
    Ok(out)
}

static PATH_PARAM: LazyLock<Regex> = LazyLock::new(|| re(r"\{[^}]+\}"));

/// A limited caller uses only `x-audience: cluster` operations, and no allow on the whole port hides the rule.
fn l7_routes(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(L7_ROUTES) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(
        L7_ROUTES,
        "paths",
        &["infra/gitops/services/*/values/*.yaml"],
    )?;
    let mut out = Vec::new();
    for file in ctx.repo.files.iter().filter(|f| paths.matches(f)) {
        let Some(service) = file
            .rsplit('/')
            .next()
            .and_then(|n| n.strip_suffix(".yaml"))
        else {
            continue;
        };
        let text = ctx
            .repo
            .read(file)
            .with_context(|| format!("read {file}"))?;
        let docs = yaml::load(&text).with_context(|| format!("parse {file}"))?;
        let Some(policy) = docs.first().and_then(|d| get(d, "networkPolicy")) else {
            continue;
        };
        let groups = get(policy, "allowFromRoutes").map_or(&[][..], yaml::items);
        if groups.is_empty() {
            continue;
        }
        let spec = format!("services/{service}/openapi.yaml");
        let Some(spec_text) = ctx.repo.read(&spec) else {
            let message = format!("allowFromRoutes needs {spec}, which does not exist");
            out.push(Finding::new(file, 0, L7_ROUTES, message));
            continue;
        };
        let spec_docs = yaml::load(&spec_text).with_context(|| format!("parse {spec}"))?;
        let mut cluster = BTreeSet::new();
        if let Some(api) = spec_docs.first().and_then(|d| get(d, "paths")) {
            for (path, methods) in yaml::pairs(api) {
                let path = yaml::text(path).unwrap_or_default();
                for (method, operation) in yaml::pairs(methods) {
                    if get(operation, "x-audience").and_then(yaml::string) == Some("cluster") {
                        cluster.insert(format!(
                            "{} {}",
                            yaml::text(method).unwrap_or_default().to_uppercase(),
                            PATH_PARAM.replace_all(&path, "[^/]+")
                        ));
                    }
                }
            }
        }
        let allow_from: Vec<String> = get(policy, "allowFrom")
            .map(|a| yaml::items(a).iter().filter_map(yaml::text).collect())
            .unwrap_or_default();
        for group in groups {
            for route in get(group, "routes").map_or(&[][..], yaml::items) {
                let text_of = |key: &str| get(route, key).and_then(yaml::text).unwrap_or_default();
                let wanted = format!("{} {}", text_of("method"), text_of("path"));
                if !cluster.contains(&wanted) {
                    let message =
                        format!("`{wanted}` is not an x-audience: cluster operation in {spec}");
                    out.push(Finding::new(file, 0, L7_ROUTES, message));
                }
            }
            let caller = get(group, "name").and_then(yaml::text).unwrap_or_default();
            if allow_from.contains(&caller) {
                let message = format!(
                    "`{caller}` is in allowFrom and allowFromRoutes. The allow on the whole port hides the routes"
                );
                out.push(Finding::new(file, 0, L7_ROUTES, message));
            }
        }
    }
    Ok(out)
}

/// A forward-auth route applies the header strip before it, so a client cannot inject an identity.
fn strip_headers(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(STRIP_HEADERS) {
        return Ok(Vec::new());
    }
    let manifests = ctx.paths(STRIP_HEADERS, "manifests", &["infra/gateway/*.yaml"])?;
    let chart = one(ctx, STRIP_HEADERS, "chart", "infra/helm/service")?;
    let forward = one(
        ctx,
        STRIP_HEADERS,
        "forward-auth",
        "oathkeeper-forward-auth",
    )?;
    let strip = one(ctx, STRIP_HEADERS, "strip", "strip-identity-headers")?;
    let mut sources: Vec<(String, String)> = ctx
        .repo
        .files
        .iter()
        .filter(|f| manifests.matches(f))
        .filter_map(|f| Some((f.clone(), ctx.repo.read(f)?)))
        .collect();
    if ctx.repo.has(&chart) {
        let rendered = Command::new("helm")
            .current_dir(&ctx.repo.root)
            .args(["template", "svc", &chart])
            .args(["--set", "name=svc", "--set", "image.repository=svc"])
            .args(["--set", "image.tag=dev", "--set", "ingress.enabled=true"])
            .args(["--set", "ingress.host=example.com"])
            .args(["--set", "ingress.resources={svc}"])
            .output();
        match rendered {
            // A repository can run tabkit without Helm, and then the chart is not an input.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            other => {
                let output = other.context("run `helm template`")?;
                anyhow::ensure!(
                    output.status.success(),
                    "helm template {chart}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                sources.push((
                    chart.clone(),
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    for (path, text) in &sources {
        let docs = yaml::load(text).with_context(|| format!("parse {path}"))?;
        for doc in &docs {
            if get(doc, "kind").and_then(yaml::string) != Some("IngressRoute") {
                continue;
            }
            let name = dig(doc, &["metadata", "name"])
                .and_then(yaml::text)
                .unwrap_or_default();
            for route in dig(doc, &["spec", "routes"]).map_or(&[][..], yaml::items) {
                let names: Vec<String> = get(route, "middlewares")
                    .map(|m| {
                        yaml::items(m)
                            .iter()
                            .filter_map(|x| get(x, "name").and_then(yaml::text))
                            .collect()
                    })
                    .unwrap_or_default();
                let Some(at) = names.iter().position(|n| *n == forward) else {
                    continue;
                };
                match names.iter().position(|n| *n == strip) {
                    None => {
                        let message = format!("{name}: forward-auth route without {strip}");
                        out.push(Finding::new(path, 0, STRIP_HEADERS, message));
                    }
                    Some(s) if s > at => {
                        let message = format!("{name}: {strip} must come before forward-auth");
                        out.push(Finding::new(path, 0, STRIP_HEADERS, message));
                    }
                    Some(_) => {}
                }
            }
        }
    }
    Ok(out)
}

static ENV_ENTRY: LazyLock<Regex> = LazyLock::new(|| re(r"\{ *env: *([a-z0-9-]+)"));

/// The bootstrap directory of an environment names only that environment.
fn env_scope(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(ENV_SCOPE) {
        return Ok(Vec::new());
    }
    let root = one(ctx, ENV_SCOPE, "root", "infra/gitops")?;
    let mut out = Vec::new();
    for dir in subdirs(ctx, &root) {
        let Some(want) = dir.strip_suffix("-bootstrap") else {
            continue;
        };
        let prefix = format!("{root}/{dir}/");
        let mut found = BTreeSet::new();
        for path in ctx.repo.files.iter().filter(|f| f.starts_with(&prefix)) {
            if let Some(text) = ctx.repo.read(path) {
                found.extend(ENV_ENTRY.captures_iter(&text).map(|m| m[1].to_owned()));
            }
        }
        for env in found.iter().filter(|e| e.as_str() != want) {
            let message = format!("generates env `{env}`, but the directory is for `{want}`");
            out.push(Finding::new(
                &format!("{root}/{dir}"),
                0,
                ENV_SCOPE,
                message,
            ));
        }
    }
    Ok(out)
}

/// The path of every leaf that is a value. A `false` or `null` leaf is no value, as in the `paths(scalars)` of jq.
fn leaf_paths(node: &MarkedYaml<'_>, prefix: &str, out: &mut BTreeSet<String>) {
    let join = |key: &str| {
        if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match &node.data {
        YamlData::Mapping(_) => {
            for (key, value) in yaml::pairs(node) {
                leaf_paths(value, &join(&yaml::text(key).unwrap_or_default()), out);
            }
        }
        YamlData::Sequence(items) => {
            for (i, item) in items.iter().enumerate() {
                leaf_paths(item, &join(&i.to_string()), out);
            }
        }
        YamlData::Value(Scalar::Null | Scalar::Boolean(false)) => {}
        _ => {
            out.insert(prefix.to_owned());
        }
    }
}

/// A deployed values key exists in the local overlay, or an allowlist gives the reason it cannot.
fn parity(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(PARITY) {
        return Ok(Vec::new());
    }
    let overlay = one(ctx, PARITY, "overlay", "infra/gitops/platform")?;
    let allowlist = one(
        ctx,
        PARITY,
        "allowlist",
        "infra/gitops/parity-allowlist.txt",
    )?;
    let local_path = format!("{overlay}/local/values.yaml");
    let (Some(local), Some(allowed)) = (ctx.repo.read(&local_path), ctx.repo.read(&allowlist))
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut prefixes = Vec::new();
    for (i, line) in allowed.lines().enumerate() {
        let trimmed = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.contains('#') {
            let message = format!("`{line}` carries no reason");
            out.push(Finding::new(&allowlist, i + 1, PARITY, message));
        }
        if !trimmed.starts_with('#')
            && let Some(first) = trimmed.split_whitespace().next()
        {
            prefixes.push(first.to_owned());
        }
    }
    let leaves = |text: &str, path: &str| -> Result<BTreeSet<String>> {
        let docs = yaml::load(text).with_context(|| format!("parse {path}"))?;
        let mut set = BTreeSet::new();
        if let Some(doc) = docs.first() {
            leaf_paths(doc, "", &mut set);
        }
        Ok(set)
    };
    let local_paths = leaves(&local, &local_path)?;
    for env in subdirs(ctx, &overlay).into_iter().filter(|e| *e != "local") {
        let file = format!("{overlay}/{env}/values.yaml");
        let Some(text) = ctx.repo.read(&file) else {
            continue;
        };
        for path in leaves(&text, &file)?.difference(&local_paths) {
            let listed = prefixes
                .iter()
                .any(|p| path == p || path.starts_with(&format!("{p}.")));
            if !listed {
                let message = format!(
                    "`{path}` is absent from the local overlay and unlisted in {allowlist}"
                );
                out.push(Finding::new(&file, 0, PARITY, message));
            }
        }
    }
    Ok(out)
}

static PORT_ENTRY: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^([a-z0-9-]+):([0-9]+)$"));
static JUST_PORT: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?m)^export PORT := env\("PORT", "([0-9]+)"#));

/// The path of the port registry and its service and port pairs.
type Registry = (String, Vec<(String, String)>);

/// The `service:port` lines of the local port registry.
fn port_registry(ctx: &Ctx<'_>) -> Result<Option<Registry>> {
    let path = one(ctx, PORTS, "registry", "scripts/lib/ports.sh")?;
    Ok(ctx.repo.read(&path).map(|text| {
        let entries = PORT_ENTRY
            .captures_iter(&text)
            .map(|m| (m[1].to_owned(), m[2].to_owned()))
            .collect();
        (path, entries)
    }))
}

/// Each service binds its own registered port, and no service takes a reserved one.
fn ports(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(PORTS) {
        return Ok(Vec::new());
    }
    let Some((registry, entries)) = port_registry(ctx)? else {
        return Ok(Vec::new());
    };
    let reserved = ctx.cfg.option_strings(PORTS, "reserved", &["8080"])?;
    let mut out = Vec::new();
    let mut by_port: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (service, port) in &entries {
        by_port.entry(port).or_default().push(service);
    }
    for (port, services) in by_port.iter().filter(|(_, s)| s.len() > 1) {
        let message = format!(
            "port {port} is registered to more than one service: {}",
            services.join(" ")
        );
        out.push(Finding::new(&registry, 0, PORTS, message));
    }
    for port in reserved.iter().filter(|p| by_port.contains_key(p.as_str())) {
        let message = format!("{port} is reserved for the local edge. Choose another port");
        out.push(Finding::new(&registry, 0, PORTS, message));
    }
    let lookup = |service: &str| entries.iter().find(|(s, _)| s == service).map(|(_, p)| p);
    for service in services(ctx) {
        let justfile = format!("services/{service}/justfile");
        if !ctx.repo.has(&format!("services/{service}/cmd/server")) {
            if lookup(service).is_some() {
                let message = format!("{service} has no cmd/server but registers a local port");
                out.push(Finding::new(&registry, 0, PORTS, message));
            }
            continue;
        }
        let Some(registered) = lookup(service) else {
            let message = format!("{service} has no entry in the port registry");
            out.push(Finding::new(&registry, 0, PORTS, message));
            continue;
        };
        let declared = ctx
            .repo
            .read(&justfile)
            .and_then(|t| JUST_PORT.captures(&t).map(|m| m[1].to_owned()));
        match declared {
            None => out.push(Finding::new(
                &justfile,
                0,
                PORTS,
                "exports no PORT. It would ignore the registry",
            )),
            Some(port) if port != *registered => {
                let message = format!("the registry says {registered}, and this file binds {port}");
                out.push(Finding::new(&justfile, 0, PORTS, message));
            }
            Some(_) => {}
        }
    }
    Ok(out)
}

/// Every service provides the same artifacts, because the platform finds services through them.
fn contract(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(CONTRACT) {
        return Ok(Vec::new());
    }
    let values_root = one(ctx, CONTRACT, "values-root", "infra/gitops/services")?;
    let linter = one(ctx, CONTRACT, "linter-config", ".config/golangci.yml")?;
    let linter_text = ctx.repo.read(&linter).unwrap_or_default();
    let envs = subdirs(ctx, &values_root);
    let registry = port_registry(ctx)?;
    let mut out = Vec::new();
    for service in services(ctx) {
        let dir = format!("services/{service}");
        let justfile_path = format!("{dir}/justfile");
        let justfile = ctx.repo.read(&justfile_path).unwrap_or_default();
        for file in [
            "justfile",
            ".env.example",
            "Dockerfile",
            "README.md",
            "requirements.md",
        ] {
            if !is_file(ctx, &format!("{dir}/{file}")) {
                out.push(Finding::new(&dir, 0, CONTRACT, format!("missing {file}")));
            }
        }
        let has_server = ctx.repo.has(&format!("{dir}/cmd/server"));
        let mut tasks = vec![
            if has_server { "server" } else { "worker" },
            "test",
            "lint",
            "build",
        ];
        if is_file(ctx, &format!("{dir}/openapi.yaml")) || ctx.repo.has(&format!("{dir}/queries")) {
            tasks.push("generate");
        }
        if ctx.repo.has(&format!("{dir}/migrations")) {
            tasks.push("migrate");
        }
        for task in tasks {
            let recipe = re(&format!("(?m)^{task}( [^:\n]*)?:"));
            if !recipe.is_match(&justfile) {
                let message = format!("justfile has no {task} recipe");
                out.push(Finding::new(&dir, 0, CONTRACT, message));
            }
        }
        if has_server && !is_file(ctx, &format!("{dir}/slo.yaml")) {
            out.push(Finding::new(
                &dir,
                0,
                CONTRACT,
                "no slo.yaml. A service that answers requests needs an availability and a latency SLI",
            ));
        }
        if !linter_text.contains(&format!("service-isolation-{service}:")) {
            let message = format!(
                "no `service-isolation-{service}` depguard rule in {linter}. Nothing stops another service from importing it"
            );
            out.push(Finding::new(&dir, 0, CONTRACT, message));
        }
        if has_server
            && let Some((path, entries)) = &registry
            && !entries.iter().any(|(s, _)| s == service)
        {
            let message = format!("no local port in {path}");
            out.push(Finding::new(&dir, 0, CONTRACT, message));
        }
        for env in &envs {
            if is_file(ctx, &format!("{values_root}/{env}/values/{service}.yaml")) {
                continue;
            }
            let opt_out = re(&format!(r"platform/not-deployed: *{}", regex::escape(env)));
            if !opt_out.is_match(&justfile) {
                let message = format!(
                    "no {values_root}/{env}/values/{service}.yaml. Add the file, or declare `# platform/not-deployed: {env}` in its justfile"
                );
                out.push(Finding::new(&dir, 0, CONTRACT, message));
            }
        }
    }
    Ok(out)
}

static CREATE_DATABASE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?m)^[ \t]*CREATE DATABASE[ \t]+([a-z0-9_]+)"));

/// Every service that owns a schema has a database, a Secret, a network path, and unique migration versions.
fn databases(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(DATABASES) {
        return Ok(Vec::new());
    }
    let values = one(
        ctx,
        DATABASES,
        "values",
        "infra/helm/platform/postgres/values.yaml",
    )?;
    let secret = one(
        ctx,
        DATABASES,
        "local-secret",
        "infra/gitops/platform/local/secrets/platform.enc.yaml",
    )?;
    let policy = one(
        ctx,
        DATABASES,
        "network-policy",
        "infra/helm/platform/network-policies/templates/10-datastores.yaml",
    )?;
    let readmes = ctx.paths(
        DATABASES,
        "secret-docs",
        &["infra/gitops/platform/*/secrets/README.md"],
    )?;
    let Some(text) = ctx.repo.read(&values) else {
        return Ok(Vec::new());
    };
    let docs = yaml::load(&text).with_context(|| format!("parse {values}"))?;
    let mut declared = BTreeSet::new();
    if let Some(initdb) = docs.first().and_then(|d| dig(d, &["cluster", "initdb"])) {
        declared.extend(get(initdb, "database").and_then(yaml::text));
        if let Some(list) = get(initdb, "databases") {
            declared.extend(yaml::items(list).iter().filter_map(yaml::text));
        }
        if let Some(list) = get(initdb, "postInitApplicationSQL") {
            for statement in yaml::items(list).iter().filter_map(yaml::text) {
                declared.extend(
                    CREATE_DATABASE
                        .captures_iter(&statement)
                        .map(|m| m[1].to_owned()),
                );
            }
        }
    }
    declared.retain(|d: &String| !d.trim().is_empty());
    let mut out = Vec::new();
    if declared.is_empty() {
        out.push(Finding::new(&values, 0, DATABASES, "no databases parsed"));
        return Ok(out);
    }
    let secret_text = ctx.repo.read(&secret).unwrap_or_default();
    let policy_text = ctx.repo.read(&policy).unwrap_or_default();
    let doc_files: Vec<(&String, String)> = ctx
        .repo
        .files
        .iter()
        .filter(|f| readmes.matches(f))
        .filter_map(|f| Some((f, ctx.repo.read(f)?)))
        .collect();
    for service in services(ctx) {
        let prefix = format!("services/{service}/migrations/");
        let migrations: Vec<&str> = ctx
            .repo
            .files
            .iter()
            .filter_map(|f| f.strip_prefix(&prefix))
            .filter(|n| !n.contains('/') && n.ends_with(".sql"))
            .collect();
        if migrations.is_empty() {
            continue;
        }
        let dir = format!("services/{service}");
        if !declared.contains(service) {
            let message = format!(
                "ships migrations but no database named `{service}` is created in {values}"
            );
            out.push(Finding::new(&dir, 0, DATABASES, message));
        }
        let entry = format!("name: {service}-db");
        if !secret_text.contains(&entry) {
            let message = format!("owns a schema but {secret} carries no {service}-db Secret");
            out.push(Finding::new(&dir, 0, DATABASES, message));
        }
        if !policy_text.contains(&format!("app.kubernetes.io/name: {service} }}")) {
            let message = format!("owns a schema but {policy} does not admit it to Postgres");
            out.push(Finding::new(&dir, 0, DATABASES, message));
        }
        for (path, text) in &doc_files {
            if !text.contains(&entry) {
                let message = format!("owns a schema but {path} documents no {service}-db entry");
                out.push(Finding::new(&dir, 0, DATABASES, message));
            }
        }
        let mut seen = BTreeSet::new();
        let mut reported = BTreeSet::new();
        for name in &migrations {
            let version = name.split('_').next().unwrap_or(name);
            if !seen.insert(version) && reported.insert(version) {
                let message = format!(
                    "two migrations share version {version}, so they cannot apply to a fresh database"
                );
                out.push(Finding::new(&dir, 0, DATABASES, message));
            }
        }
    }
    Ok(out)
}

/// Every workspace member is copied into the image build.
fn workspace_copies(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(WORKSPACE) {
        return Ok(Vec::new());
    }
    let dockerfile = one(ctx, WORKSPACE, "dockerfile", "apps/frontend/Dockerfile")?;
    let (Some(manifest), Some(docker)) =
        (ctx.repo.read("package.json"), ctx.repo.read(&dockerfile))
    else {
        return Ok(Vec::new());
    };
    let parsed: Value = serde_json::from_str(&manifest).context("parse package.json")?;
    let mut out = Vec::new();
    for member in parsed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if member.contains('*') {
            let parent = member.rsplit_once('/').map_or(member, |(p, _)| p);
            let pattern = regex::escape(parent);
            let copy = re(&format!("(?m)^COPY {pattern}/? {pattern}/?$"));
            if !copy.is_match(&docker) {
                let message = format!(
                    "`{member}` matches workspace members but this file never copies {parent}/"
                );
                out.push(Finding::new(&dockerfile, 0, WORKSPACE, message));
            }
        } else if !docker.contains(&format!("COPY {member}/package.json")) {
            let message = format!(
                "`{member}` is a workspace member but this file never copies its package.json"
            );
            out.push(Finding::new(&dockerfile, 0, WORKSPACE, message));
        }
    }
    Ok(out)
}

/// npm stays inside the one Node workspace, and the other workspaces run on Bun.
fn node_scope(ctx: &Ctx<'_>) -> Vec<Finding> {
    if !ctx.cfg.enabled(NODE_SCOPE) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for path in ctx.repo.files.iter().filter(|f| {
        (f.as_str() == "package-lock.json" || f.ends_with("/package-lock.json"))
            && !f.starts_with("test/e2e/")
            && !f.contains("node_modules/")
    }) {
        out.push(Finding::new(
            path,
            0,
            NODE_SCOPE,
            "an npm lockfile outside test/e2e",
        ));
    }
    if ctx
        .repo
        .read("package.json")
        .is_some_and(|t| t.contains("\"test/e2e"))
    {
        out.push(Finding::new(
            "package.json",
            0,
            NODE_SCOPE,
            "test/e2e is in the Bun workspace. It stays a Node island",
        ));
    }
    if !is_file(ctx, "test/e2e/package.json") {
        out.push(Finding::new(
            "test/e2e",
            0,
            NODE_SCOPE,
            "missing test/e2e/package.json",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_false_or_null_leaf_is_no_value() {
        let docs =
            yaml::load("a:\n  b: 1\n  c: false\n  d: null\n  e: [x, true]\n  f: {}\n").unwrap();
        let mut set = BTreeSet::new();
        leaf_paths(&docs[0], "", &mut set);
        let got: Vec<&str> = set.iter().map(String::as_str).collect();
        assert_eq!(got, ["a.b", "a.e.0", "a.e.1"]);
    }

    #[test]
    fn a_database_statement_is_read() {
        let found: Vec<String> = CREATE_DATABASE
            .captures_iter("  CREATE DATABASE orders;\nSELECT 1;\nCREATE DATABASE orgs TEMPLATE x;")
            .map(|m| m[1].to_owned())
            .collect();
        assert_eq!(found, ["orders", "orgs"]);
    }

    #[test]
    fn a_bypass_is_found_and_a_csp_branch_is_not() {
        assert!(AUTH_BYPASS.is_match("if (process.env.NODE_ENV === 'development') skipAuth()"));
        assert!(!AUTH_BYPASS.is_match("const csp = process.env.NODE_ENV === 'development'"));
    }
}
