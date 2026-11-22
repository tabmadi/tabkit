//! `kube/resource-governance`: renders every chart and checks its resources against the namespace guardrails, before
//! the API server can refuse a pod.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use saphyr::{MarkedYaml, Scalar, YamlData};

use super::Ctx;
use tabkit_core::finding::Finding;
use tabkit_core::yaml;

const RULE: &str = "kube/resource-governance";
const GIB: f64 = 1_073_741_824.0;
const MEMORY_SUFFIXES: [(&str, f64); 7] = [
    ("Ki", 1024.0),
    ("Mi", 1_048_576.0),
    ("Gi", GIB),
    ("Ti", 1_099_511_627_776.0),
    ("K", 1e3),
    ("M", 1e6),
    ("G", 1e9),
];
const WORKLOADS: [&str; 6] = [
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "Job",
    "CronJob",
    "Pooler",
];

/// One resource map. A key with an empty value is `cpu: null`, which the API server reads as zero.
type Quantities = BTreeMap<String, String>;

#[derive(Default)]
struct Resources {
    requests: Quantities,
    limits: Quantities,
}

/// One container as it will exist in the cluster.
struct Entry {
    label: String,
    namespace: String,
    resources: Resources,
    init: bool,
}

#[derive(Default)]
struct LimitRange {
    enabled: bool,
    default_memory_limit: String,
    default_memory_request: String,
    default_cpu_request: String,
    min_cpu: String,
    min_memory: String,
    max_memory: String,
}

#[derive(Default)]
struct Quota {
    enabled: bool,
    requests_cpu: String,
    requests_memory: String,
    limits_memory: String,
}

fn get<'a, 'b>(node: &'a MarkedYaml<'b>, key: &str) -> Option<&'a MarkedYaml<'b>> {
    yaml::pairs(node)
        .find(|(k, _)| yaml::string(k) == Some(key))
        .map(|(_, v)| v)
}

fn dig<'a, 'b>(node: &'a MarkedYaml<'b>, keys: &[&str]) -> Option<&'a MarkedYaml<'b>> {
    keys.iter().try_fold(node, |n, key| get(n, key))
}

fn text(node: &MarkedYaml<'_>, key: &str) -> String {
    get(node, key).and_then(yaml::text).unwrap_or_default()
}

fn flag(node: &MarkedYaml<'_>, key: &str) -> bool {
    get(node, key).is_some_and(|v| matches!(v.data, YamlData::Value(Scalar::Boolean(true))))
}

fn parse_cpu(value: &str) -> Result<f64> {
    if value.is_empty() {
        return Ok(0.0);
    }
    if let Some(milli) = value.strip_suffix('m') {
        let n: f64 = milli
            .parse()
            .with_context(|| format!("parse cpu `{value}`"))?;
        return Ok(n / 1000.0);
    }
    value
        .parse()
        .with_context(|| format!("parse cpu `{value}`"))
}

fn parse_memory(value: &str) -> Result<f64> {
    if value.is_empty() {
        return Ok(0.0);
    }
    for (suffix, factor) in MEMORY_SUFFIXES {
        if let Some(number) = value.strip_suffix(suffix) {
            let n: f64 = number
                .parse()
                .with_context(|| format!("parse memory `{value}`"))?;
            return Ok(n * factor);
        }
    }
    value
        .parse()
        .with_context(|| format!("parse memory `{value}`"))
}

fn quantities(node: Option<&MarkedYaml<'_>>) -> Quantities {
    node.map(|n| {
        yaml::pairs(n)
            .filter_map(|(k, v)| Some((yaml::text(k)?, yaml::text(v).unwrap_or_default())))
            .collect()
    })
    .unwrap_or_default()
}

fn resources(node: Option<&MarkedYaml<'_>>) -> Resources {
    Resources {
        requests: quantities(node.and_then(|n| get(n, "requests"))),
        limits: quantities(node.and_then(|n| get(n, "limits"))),
    }
}

fn containers_of(doc: &MarkedYaml<'_>, chart: &str, default_ns: &str, out: &mut Vec<Entry>) {
    let kind = text(doc, "kind");
    let name = dig(doc, &["metadata", "name"])
        .and_then(yaml::text)
        .unwrap_or_default();
    let namespace = dig(doc, &["metadata", "namespace"])
        .and_then(yaml::text)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| default_ns.to_owned());
    if kind == "Cluster" {
        out.push(Entry {
            label: format!("{chart}/{name}/postgres"),
            namespace,
            resources: resources(dig(doc, &["spec", "resources"])),
            init: false,
        });
        return;
    }
    if !WORKLOADS.contains(&kind.as_str()) {
        return;
    }
    let Some(pod) = dig(doc, &["spec", "template", "spec"]) else {
        return;
    };
    for (key, init) in [("containers", false), ("initContainers", true)] {
        for container in get(pod, key).map_or(&[][..], yaml::items) {
            out.push(Entry {
                label: format!("{chart}/{name}/{}", text(container, "name")),
                namespace: namespace.clone(),
                resources: resources(get(container, "resources")),
                init,
            });
        }
    }
}

/// A subchart at its pinned version is on disk, so a version bump never renders the stale one.
fn has_subchart(dir: &Path, name: &str, version: &str) -> bool {
    let base = dir.join("charts");
    base.join(name).is_dir() || base.join(format!("{name}-{version}.tgz")).is_file()
}

fn ensure_dependencies(root: &Path, dir: &str) -> Result<()> {
    let chart_dir = root.join(dir);
    let path = chart_dir.join("Chart.yaml");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let docs = yaml::load(&source).with_context(|| format!("parse {}", path.display()))?;
    let missing = docs
        .first()
        .and_then(|d| get(d, "dependencies"))
        .is_some_and(|deps| {
            yaml::items(deps)
                .iter()
                .any(|d| !has_subchart(&chart_dir, &text(d, "name"), &text(d, "version")))
        });
    if !missing {
        return Ok(());
    }
    // `update`, because `build` refuses a repository that has no local name.
    let output = Command::new("helm")
        .current_dir(root)
        .args(["dependency", "update", dir])
        .output()
        .context("run `helm dependency update`")?;
    if !output.status.success() {
        bail!(
            "helm dependency update {dir}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

struct Render<'a> {
    root: &'a Path,
    shared: String,
    namespaces: BTreeMap<String, String>,
    default_ns: String,
}

impl Render<'_> {
    fn chart(&self, name: &str, dir: &str, extra: &[String]) -> Result<Vec<Entry>> {
        ensure_dependencies(self.root, dir)?;
        let namespace = self.namespaces.get(name).unwrap_or(&self.default_ns);
        let mut command = Command::new("helm");
        command.current_dir(self.root).args([
            "template",
            name,
            dir,
            "--namespace",
            namespace,
            "-f",
            &self.shared,
        ]);
        for pair in extra {
            command.args(["--set", pair]);
        }
        let output = command.output().context("run `helm template`")?;
        if !output.status.success() {
            bail!(
                "helm template {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let rendered = String::from_utf8_lossy(&output.stdout);
        let docs = yaml::load(&rendered).map_err(|e| anyhow!("parse the render of {name}: {e}"))?;
        let mut out = Vec::new();
        for doc in &docs {
            containers_of(doc, name, namespace, &mut out);
        }
        Ok(out)
    }
}

fn helm_available() -> bool {
    Command::new("helm").arg("version").output().is_ok()
}

#[expect(clippy::too_many_lines, reason = "the inputs of one render, in order")]
pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(RULE) || !helm_available() {
        return Ok(Vec::new());
    }
    let option = |key: &str, default: &str| -> Result<String> {
        Ok(ctx
            .cfg
            .option_str(RULE, key)?
            .unwrap_or_else(|| default.to_owned()))
    };
    let governance = option(
        "governance",
        "infra/helm/platform/resource-governance/values.yaml",
    )?;
    let Some(source) = ctx.repo.read(&governance) else {
        return Ok(Vec::new());
    };
    let charts = option("charts", "infra/helm/platform")?;
    let service_chart = option("service-chart", "infra/helm/service")?;
    let render = Render {
        root: &ctx.repo.root,
        shared: option("shared-values", "infra/gitops/platform/shared-values.yaml")?,
        namespaces: ctx
            .cfg
            .option_map(RULE, "namespaces")?
            .into_iter()
            .collect(),
        default_ns: option("default-namespace", "platform")?,
    };
    let chart_set: BTreeMap<String, String> =
        ctx.cfg.option_map(RULE, "chart-set")?.into_iter().collect();
    let service_set = ctx.cfg.option_strings(
        RULE,
        "service-set",
        &[
            "name={service}",
            "image.repository=r",
            "image.tag=v",
            "ingress.enabled=false",
            "worker.enabled={worker}",
            "worker.image.repository=r",
            "worker.image.tag=v",
            "worker.versioning.enabled=false",
        ],
    )?;
    let allowed: BTreeMap<String, String> = ctx
        .cfg
        .option_map(RULE, "cpu-limit-allow")?
        .into_iter()
        .collect();

    let mut entries = Vec::new();
    let prefix = format!("{charts}/");
    let chart_names: Vec<&str> = ctx
        .repo
        .dirs
        .iter()
        .filter_map(|d| d.strip_prefix(&prefix))
        .filter(|d| !d.contains('/'))
        .filter(|d| {
            ctx.repo
                .root
                .join(format!("{charts}/{d}/Chart.yaml"))
                .is_file()
        })
        .collect();
    for name in chart_names {
        let extra: Vec<String> = chart_set
            .get(name)
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        entries.extend(render.chart(name, &format!("{charts}/{name}"), &extra)?);
    }
    for service in ctx
        .repo
        .dirs
        .iter()
        .filter_map(|d| d.strip_prefix("services/"))
        .filter(|d| !d.contains('/') && !d.starts_with('_'))
    {
        let worker = ctx.repo.has(&format!("services/{service}/cmd/worker"));
        let extra: Vec<String> = service_set
            .iter()
            .map(|s| {
                s.replace("{service}", service)
                    .replace("{worker}", if worker { "true" } else { "false" })
            })
            .collect();
        let rendered = render
            .chart("service", &service_chart, &extra)
            .with_context(|| format!("service chart for {service}"))?;
        entries.extend(rendered.into_iter().map(|mut e| {
            e.label = e.label.replacen("service/", &format!("{service}/"), 1);
            e
        }));
    }

    let docs = yaml::load(&source).with_context(|| format!("parse {governance}"))?;
    let doc = docs.first().context("the governance file is empty")?;
    let ranges = limit_ranges(doc);
    let quotas = resource_quotas(doc);
    let mut problems = rejections(&entries, &ranges)?;
    problems.extend(coverage(&entries, &ranges));
    problems.extend(cpu_limits(&entries, &allowed));
    problems.extend(quota_overruns(&entries, &ranges, &quotas)?);
    Ok(problems
        .into_iter()
        .map(|p| Finding::new(&governance, 0, RULE, p))
        .collect())
}

fn limit_ranges(doc: &MarkedYaml<'_>) -> BTreeMap<String, LimitRange> {
    get(doc, "limitRanges").map_or_else(BTreeMap::new, |ranges| {
        yaml::pairs(ranges)
            .filter_map(|(k, v)| {
                Some((
                    yaml::text(k)?,
                    LimitRange {
                        enabled: flag(v, "enabled"),
                        default_memory_limit: text(v, "defaultMemoryLimit"),
                        default_memory_request: text(v, "defaultMemoryRequest"),
                        default_cpu_request: text(v, "defaultCPURequest"),
                        min_cpu: text(v, "minCPU"),
                        min_memory: text(v, "minMemory"),
                        max_memory: text(v, "maxMemory"),
                    },
                ))
            })
            .collect()
    })
}

fn resource_quotas(doc: &MarkedYaml<'_>) -> BTreeMap<String, Quota> {
    get(doc, "resourceQuotas").map_or_else(BTreeMap::new, |quotas| {
        yaml::pairs(quotas)
            .filter_map(|(k, v)| {
                Some((
                    yaml::text(k)?,
                    Quota {
                        enabled: flag(v, "enabled"),
                        requests_cpu: text(v, "requestsCPU"),
                        requests_memory: text(v, "requestsMemory"),
                        limits_memory: text(v, "limitsMemory"),
                    },
                ))
            })
            .collect()
    })
}

fn value<'a>(map: &'a Quantities, key: &str) -> &'a str {
    map.get(key).map_or("", String::as_str)
}

fn first_non_empty<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() { b } else { a }
}

/// A declared value outside the bounds of its LimitRange makes the API server refuse the pod.
fn rejections(entries: &[Entry], ranges: &BTreeMap<String, LimitRange>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for e in entries {
        let Some(range) = ranges.get(&e.namespace).filter(|r| r.enabled) else {
            continue;
        };
        for (kind, field, declared, bound, over) in [
            (
                "requests",
                "cpu",
                value(&e.resources.requests, "cpu"),
                range.min_cpu.as_str(),
                false,
            ),
            (
                "requests",
                "memory",
                value(&e.resources.requests, "memory"),
                range.min_memory.as_str(),
                false,
            ),
            (
                "limits",
                "memory",
                value(&e.resources.limits, "memory"),
                range.max_memory.as_str(),
                true,
            ),
        ] {
            if declared.is_empty() || bound.is_empty() {
                continue;
            }
            let parse = if field == "cpu" {
                parse_cpu
            } else {
                parse_memory
            };
            let (got, want) = (
                parse(declared).with_context(|| e.label.clone())?,
                parse(bound).with_context(|| e.label.clone())?,
            );
            if (over && got > want) || (!over && got < want) {
                let relation = if over { "ABOVE" } else { "BELOW" };
                out.push(format!(
                    "{} {kind} {field}={declared} is {relation} the {} LimitRange bound of {bound}. The pod would be rejected",
                    e.label, e.namespace
                ));
            }
        }
    }
    Ok(out)
}

/// Every container is governed, by a declared value or by a default of its namespace.
fn coverage(entries: &[Entry], ranges: &BTreeMap<String, LimitRange>) -> Vec<String> {
    let empty = LimitRange::default();
    let mut out = Vec::new();
    for e in entries {
        let range = ranges.get(&e.namespace);
        let defaulted = range.is_some_and(|r| r.enabled);
        let r = range.unwrap_or(&empty);
        for (what, declared, fallback) in [
            (
                "cpu request",
                value(&e.resources.requests, "cpu"),
                r.default_cpu_request.as_str(),
            ),
            (
                "memory request",
                value(&e.resources.requests, "memory"),
                r.default_memory_request.as_str(),
            ),
            (
                "memory limit",
                value(&e.resources.limits, "memory"),
                r.default_memory_limit.as_str(),
            ),
        ] {
            if declared.is_empty() && (!defaulted || fallback.is_empty()) {
                out.push(format!(
                    "{} has no {what}, and namespace {} supplies no default",
                    e.label, e.namespace
                ));
            }
        }
    }
    out
}

/// A CPU limit is opt-in, and each one that stays has a reason in the config.
fn cpu_limits(entries: &[Entry], allowed: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for e in entries {
        let Some(limit) = e.resources.limits.get("cpu") else {
            continue;
        };
        if limit.is_empty() {
            out.push(format!(
                "{} sets an empty cpu limit, `cpu: null`. The API server reads that as 0. Remove the override and list the container in `cpu-limit-allow`",
                e.label
            ));
        } else if !allowed.contains_key(&e.label) {
            out.push(format!(
                "{} sets a cpu limit, {limit}. Add it to `cpu-limit-allow` with a reason, or remove it",
                e.label
            ));
        }
    }
    out
}

/// The sum of each namespace fits its quota. An init container does not add to the effective request of a pod.
fn quota_overruns(
    entries: &[Entry],
    ranges: &BTreeMap<String, LimitRange>,
    quotas: &BTreeMap<String, Quota>,
) -> Result<Vec<String>> {
    let empty = LimitRange::default();
    let mut sums: BTreeMap<&str, (f64, f64, f64)> = BTreeMap::new();
    for e in entries.iter().filter(|e| !e.init) {
        let r = ranges.get(&e.namespace).unwrap_or(&empty);
        let sum = sums.entry(&e.namespace).or_default();
        sum.0 += parse_cpu(first_non_empty(
            value(&e.resources.requests, "cpu"),
            &r.default_cpu_request,
        ))
        .with_context(|| e.label.clone())?;
        sum.1 += parse_memory(first_non_empty(
            value(&e.resources.requests, "memory"),
            &r.default_memory_request,
        ))
        .with_context(|| e.label.clone())?;
        sum.2 += parse_memory(first_non_empty(
            value(&e.resources.limits, "memory"),
            &r.default_memory_limit,
        ))
        .with_context(|| e.label.clone())?;
    }
    let mut out = Vec::new();
    for (namespace, (cpu, memory, limit)) in sums {
        let Some(quota) = quotas.get(namespace).filter(|q| q.enabled) else {
            continue;
        };
        let cap_cpu = parse_cpu(&quota.requests_cpu)?;
        let cap_memory = parse_memory(&quota.requests_memory)?;
        let cap_limit = parse_memory(&quota.limits_memory)?;
        if cpu > cap_cpu {
            out.push(format!(
                "{namespace}: summed requests.cpu is {cpu:.2} cores, cap is {cap_cpu:.2} cores. Pods would be rejected"
            ));
        }
        for (name, got, want) in [
            ("requests.memory", memory, cap_memory),
            ("limits.memory", limit, cap_limit),
        ] {
            if got > want {
                out.push(format!(
                    "{namespace}: summed {name} is {:.2}Gi, cap is {:.2}Gi. Pods would be rejected",
                    got / GIB,
                    want / GIB
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, requests: &[(&str, &str)], limits: &[(&str, &str)], init: bool) -> Entry {
        let map = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        Entry {
            label: label.to_owned(),
            namespace: "platform".to_owned(),
            resources: Resources {
                requests: map(requests),
                limits: map(limits),
            },
            init,
        }
    }

    fn ranges() -> BTreeMap<String, LimitRange> {
        [(
            "platform".to_owned(),
            LimitRange {
                enabled: true,
                default_memory_limit: "256Mi".to_owned(),
                default_memory_request: "64Mi".to_owned(),
                default_cpu_request: "10m".to_owned(),
                min_cpu: "1m".to_owned(),
                min_memory: "4Mi".to_owned(),
                max_memory: "1Gi".to_owned(),
            },
        )]
        .into()
    }

    #[test]
    fn quantities_parse() {
        assert!((parse_cpu("100m").unwrap() - 0.1).abs() < 1e-9);
        assert!((parse_cpu("2").unwrap() - 2.0).abs() < 1e-9);
        assert!((parse_memory("1Gi").unwrap() - GIB).abs() < 1e-3);
        assert!((parse_memory("1M").unwrap() - 1e6).abs() < 1e-3);
        assert!(parse_memory("x").is_err());
    }

    #[test]
    fn a_value_outside_the_range_is_rejected() {
        let high = [entry("a/b/c", &[], &[("memory", "2Gi")], false)];
        assert_eq!(rejections(&high, &ranges()).unwrap().len(), 1);
        let fine = [entry(
            "a/b/c",
            &[("cpu", "10m")],
            &[("memory", "512Mi")],
            false,
        )];
        assert_eq!(rejections(&fine, &ranges()).unwrap().len(), 0);
    }

    #[test]
    fn a_cpu_limit_needs_a_reason() {
        let allowed: BTreeMap<String, String> =
            [("a/b/ok".to_owned(), "startup".to_owned())].into();
        let entries = [
            entry("a/b/ok", &[], &[("cpu", "1")], false),
            entry("a/b/bad", &[], &[("cpu", "1")], false),
            entry("a/b/null", &[], &[("cpu", "")], false),
        ];
        assert_eq!(cpu_limits(&entries, &allowed).len(), 2);
    }

    #[test]
    fn an_init_container_is_not_summed() {
        let quotas: BTreeMap<String, Quota> = [(
            "platform".to_owned(),
            Quota {
                enabled: true,
                requests_cpu: "1".to_owned(),
                requests_memory: "1Gi".to_owned(),
                limits_memory: "1Gi".to_owned(),
            },
        )]
        .into();
        let init = [entry("a/b/i", &[("cpu", "4")], &[], true)];
        assert_eq!(quota_overruns(&init, &ranges(), &quotas).unwrap().len(), 0);
        let main = [entry("a/b/m", &[("cpu", "4")], &[], false)];
        assert_eq!(quota_overruns(&main, &ranges(), &quotas).unwrap().len(), 1);
    }
}
