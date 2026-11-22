//! The gates that lint a repository through pinned tools.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tabkit_core::yaml;

use super::files::{TempDir, repo_files, service_of, shell_files, under};
use super::{detail, exec, ok, step, warn};

fn no_scripts() -> anyhow::Error {
    anyhow::anyhow!(
        "no shell scripts found. This repository has dozens, so the listing is broken, and the tree is not empty"
    )
}

pub fn format_shell() -> Result<()> {
    let files = shell_files()?;
    if files.is_empty() {
        return Err(no_scripts());
    }
    let mut args = vec!["-w", "-i", "2"];
    args.extend(files.iter().map(String::as_str));
    exec::status("shfmt", &args, &[])?;
    ok(&format!("shfmt formatted {} scripts", files.len()));
    Ok(())
}

pub fn lint_shell() -> Result<()> {
    step("shellcheck: linting shell scripts");
    let files = shell_files()?;
    if files.is_empty() {
        return Err(no_scripts());
    }
    let mut args = vec![
        "--rcfile",
        ".config/shellcheckrc",
        "-x",
        "--severity=warning",
    ];
    args.extend(files.iter().map(String::as_str));
    exec::status("shellcheck", &args, &[])?;
    ok(&format!("shellcheck clean, {} scripts", files.len()));
    Ok(())
}

fn copy_tree(files: &[String], target: &Path) -> Result<()> {
    for file in files {
        let source = Path::new(file);
        let dest = target.join(file);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let meta = source.symlink_metadata()?;
        if meta.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(source)?, &dest)?;
        } else if meta.is_file() {
            fs::copy(source, &dest).with_context(|| format!("cannot copy {file}"))?;
        }
    }
    Ok(())
}

pub fn lint_secrets() -> Result<()> {
    let root = std::env::current_dir()?;
    let config = root
        .join(".config/gitleaks.toml")
        .to_string_lossy()
        .into_owned();
    let ignore = root.join(".gitleaksignore").to_string_lossy().into_owned();
    let common = [
        "--config",
        &config,
        "--gitleaks-ignore-path",
        &ignore,
        "--redact",
        "--no-banner",
    ];
    if std::env::var("GITLEAKS_HISTORY").is_ok_and(|v| !v.is_empty()) {
        step("scanning full git history for plaintext secrets");
        let mut args = vec!["git"];
        args.extend(common);
        exec::status("gitleaks", &args, &[])?;
    } else {
        step("scanning the tracked files for plaintext secrets");
        let work = TempDir::new("secrets")?;
        copy_tree(&repo_files()?, &work.0)?;
        let mut args = vec!["dir", "."];
        args.extend(common);
        exec::status_in(&work.0, "gitleaks", &args, &[])?;
    }
    ok("no plaintext secrets found");
    Ok(())
}

pub fn lint_sql() -> Result<()> {
    let mut targets: Vec<String> = under("services", "migrations")
        .into_iter()
        .chain(under("services", "internal/store/queries"))
        .filter(|p| p.is_dir())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    targets.sort();
    if targets.is_empty() {
        ok("no SQL targets to lint yet");
        return Ok(());
    }
    step(&format!(
        "linting {} SQL target(s) with sqruff",
        targets.len()
    ));
    let mut args = vec!["lint", "--config", ".config/sqruff.cfg"];
    args.extend(targets.iter().map(String::as_str));
    exec::status("sqruff", &args, &[])?;
    step("checking personal-data columns are tagged");
    exec::status("tabkit", &["check", "--only", "sql/pii-tag"], &[])
}

pub fn lint_openapi() -> Result<()> {
    let specs: Vec<String> = under("services", "openapi.yaml")
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if specs.is_empty() {
        ok("no OpenAPI specs yet");
        return Ok(());
    }
    step(&format!(
        "linting {} OpenAPI spec(s) with vacuum",
        specs.len()
    ));
    let mut args = vec![
        "lint",
        "--ruleset",
        ".config/openapi-ruleset.yaml",
        "--fail-severity",
        "error",
    ];
    args.extend(specs.iter().map(String::as_str));
    exec::status("vacuum", &args, &[])?;
    step("checking resource-prefix ownership across the /api namespace");
    exec::status("tabkit", &["check", "--only", "api/prefixes"], &[])?;
    step("checking the shared components have not diverged");
    exec::status("tabkit", &["gen", "shared-components", "--check"], &[])
}

/// A private IPv4 address of RFC 1918.
pub fn is_private(address: &str) -> bool {
    let parts: Vec<u8> = address.split('.').filter_map(|p| p.parse().ok()).collect();
    if parts.len() != 4 || address.split('.').count() != 4 {
        return false;
    }
    parts[0] == 10
        || (parts[0] == 192 && parts[1] == 168)
        || (parts[0] == 172 && (16..=31).contains(&parts[1]))
}

fn field<'a, 'b>(
    node: &'a saphyr::MarkedYaml<'b>,
    name: &str,
) -> Option<&'a saphyr::MarkedYaml<'b>> {
    yaml::pairs(node)
        .find(|(k, _)| yaml::string(k) == Some(name))
        .map(|(_, v)| v)
}

/// The findings of one environment: its inventory text and the text of its values file.
fn etcd_findings(
    inventory: &str,
    values: &str,
    inventory_path: &str,
    values_path: &str,
) -> Result<Vec<String>> {
    let (mut want, mut out) = (BTreeSet::new(), Vec::new());
    let (mut missing, docs) = (Vec::new(), yaml::load(inventory)?);
    for (name, node) in docs
        .first()
        .and_then(|d| field(d, "nodes"))
        .into_iter()
        .flat_map(yaml::pairs)
    {
        if field(node, "role").and_then(yaml::text).as_deref() != Some("controlplane") {
            continue;
        }
        match field(node, "privateAddress")
            .and_then(yaml::text)
            .filter(|a| !a.is_empty())
        {
            Some(address) => {
                if !is_private(&address) {
                    out.push(format!(
                        "{inventory_path}: {address} is not a private address"
                    ));
                }
                want.insert(address);
            }
            None => missing.push(yaml::text(name).unwrap_or_default()),
        }
    }
    if !missing.is_empty() {
        out.push(format!(
            "{inventory_path}: no privateAddress on: {}",
            missing.join(" ")
        ));
    }
    let docs = yaml::load(values)?;
    let got: BTreeSet<String> = docs
        .first()
        .and_then(|d| field(d, "etcdMetrics"))
        .and_then(|m| field(m, "addresses"))
        .map(yaml::items)
        .unwrap_or_default()
        .iter()
        .filter_map(yaml::text)
        .collect();
    if want != got {
        out.push(format!(
            "{values_path}: etcdMetrics.addresses differs from {inventory_path}"
        ));
    }
    Ok(out)
}

pub fn lint_etcd_addresses() -> Result<()> {
    let mut failed = false;
    for inventory in under("infra/talos/inventory", "nodes.yml") {
        let env = inventory
            .parent()
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let values = format!("infra/gitops/platform/{env}/values.yaml");
        if !Path::new(&values).is_file() {
            continue;
        }
        let inventory_path = inventory.to_string_lossy();
        for finding in etcd_findings(
            &fs::read_to_string(&inventory)?,
            &fs::read_to_string(&values)?,
            &inventory_path,
            &values,
        )? {
            warn(&finding);
            failed = true;
        }
    }
    if failed {
        bail!("the etcd metric addresses do not match the inventory");
    }
    ok("the etcd metric addresses match the inventory");
    Ok(())
}

pub fn lint_api_breaking() -> Result<()> {
    let base = std::env::var("BASE_REF")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "master".to_string());
    let base = base.trim_start_matches("refs/heads/").to_string();
    let Some(reference) = [format!("origin/{base}"), base.clone()]
        .into_iter()
        .find(|c| {
            exec::succeeds(
                "git",
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{c}^{{commit}}"),
                ],
            )
        })
    else {
        warn(&format!(
            "no such base ref: {base}. Skipping breaking-change detection"
        ));
        return Ok(());
    };
    let specs = under("services", "openapi.yaml");
    if specs.is_empty() {
        ok("no OpenAPI specs yet");
        return Ok(());
    }
    step(&format!(
        "comparing {} spec(s) against {reference}",
        specs.len()
    ));
    let work = TempDir::new("api")?;
    let mut breaking = false;
    for spec in &specs {
        let text = spec.to_string_lossy();
        let object = format!("{reference}:{text}");
        let service = service_of(spec);
        if !exec::succeeds("git", &["cat-file", "-e", &object]) {
            detail(&format!("{service}: new spec, nothing to compare"));
            continue;
        }
        let old = work.0.join(text.replace('/', "_"));
        fs::write(&old, exec::output("git", &["show", &object])?)?;
        let raw = exec::lossy(
            "oasdiff",
            &[
                "breaking",
                &old.to_string_lossy(),
                &text,
                "--format",
                "json",
            ],
        )
        .unwrap_or_default();
        let findings: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
        if findings.is_empty() {
            continue;
        }
        breaking = true;
        warn(&format!("{service}: {} breaking change(s)", findings.len()));
        for f in &findings {
            let value = |key: &str| f[key].as_str().unwrap_or("null").to_string();
            println!(
                "    {} {}: {} [{}]",
                value("operation"),
                value("path"),
                value("text"),
                value("id")
            );
        }
    }
    if breaking {
        warn(
            "breaking API changes above. An intended change is fine, and an unnoticed one is not, per ADR-0303",
        );
        if let Some(path) = std::env::var_os("GITHUB_OUTPUT").filter(|v| !v.is_empty()) {
            use std::io::Write as _;
            writeln!(
                fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)?,
                "breaking=true"
            )?;
        }
    } else {
        ok(&format!("no breaking API changes against {reference}"));
    }
    Ok(())
}

pub fn test_alerts() -> Result<()> {
    const DIR: &str = "infra/observability/alerts";
    let tests = format!("{DIR}/tests/alerts.test.yaml");
    let mut rules: Vec<String> = fs::read_dir(DIR)
        .with_context(|| format!("no rule files in {DIR}"))?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "yaml")
                && p.file_name().is_some_and(|n| n != "Chart.yaml")
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    rules.sort();
    if rules.is_empty() {
        bail!("no rule files in {DIR}");
    }
    step(&format!(
        "checking the syntax of {} rule files",
        rules.len()
    ));
    let mut args = vec!["check", "rules"];
    args.extend(rules.iter().map(String::as_str));
    exec::output("promtool", &args)?;

    let work = TempDir::new("alerts")?;
    fs::create_dir(work.0.join("rules"))?;
    let mut alerts = BTreeSet::new();
    for rule in &rules {
        let text = fs::read_to_string(rule)?;
        for doc in yaml::load(&text)? {
            for group in field(&doc, "groups").map(yaml::items).unwrap_or_default() {
                for item in field(group, "rules").map(yaml::items).unwrap_or_default() {
                    alerts.extend(field(item, "alert").and_then(yaml::text));
                }
            }
        }
        let stripped = exec::output("yq", &["del(.groups[].rules[].annotations)", rule])?;
        let name = Path::new(rule)
            .file_name()
            .context("a rule file has a name")?;
        fs::write(work.0.join("rules").join(name), stripped)?;
    }
    fs::copy(&tests, work.0.join("alerts.test.yaml"))?;

    step("checking that each alert has a test that fires it");
    let text = fs::read_to_string(&tests)?;
    let mut fired = BTreeSet::new();
    for doc in yaml::load(&text)? {
        for test in field(&doc, "tests").map(yaml::items).unwrap_or_default() {
            for check in field(test, "alert_rule_test")
                .map(yaml::items)
                .unwrap_or_default()
            {
                if field(check, "exp_alerts").is_some_and(|e| !yaml::items(e).is_empty()) {
                    fired.extend(field(check, "alertname").and_then(yaml::text));
                }
            }
        }
    }
    let missing: Vec<&str> = alerts.difference(&fired).map(String::as_str).collect();
    if !missing.is_empty() {
        bail!("no firing test in {tests} for: {}", missing.join(" "));
    }
    step("running the alert tests");
    exec::status_in(
        &work.0,
        "promtool",
        &["test", "rules", "alerts.test.yaml"],
        &[],
    )?;
    ok("every alert fires after its wait and stays quiet on healthy input");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_a_private_address() {
        for address in ["10.0.0.1", "192.168.1.1", "172.16.0.1", "172.31.255.1"] {
            assert!(is_private(address), "{address}");
        }
        for address in ["172.32.0.1", "8.8.8.8", "10.0.0", "10.0.0.256", "a.b.c.d"] {
            assert!(!is_private(address), "{address}");
        }
    }

    const INVENTORY: &str = "nodes:\n  cp-1: { role: controlplane, privateAddress: 10.0.0.1 }\n  w-1: { role: worker, privateAddress: 10.0.0.9 }\n";

    #[test]
    fn matching_addresses_have_no_finding() {
        let values = "etcdMetrics:\n  addresses: [10.0.0.1]\n";
        assert_eq!(
            etcd_findings(INVENTORY, values, "i", "v").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reports_a_mismatch_a_public_address_and_a_missing_one() {
        let values = "etcdMetrics:\n  addresses: [10.0.0.2]\n";
        assert_eq!(etcd_findings(INVENTORY, values, "i", "v").unwrap().len(), 1);
        let public = "nodes:\n  a: { role: controlplane, privateAddress: 8.8.8.8 }\n  b: { role: controlplane }\n";
        let found =
            etcd_findings(public, "etcdMetrics: { addresses: [8.8.8.8] }\n", "i", "v").unwrap();
        assert!(found.iter().any(|f| f.contains("not a private")));
        assert!(found.iter().any(|f| f.contains("no privateAddress on: b")));
    }
}
