//! The generators and the drift check of a repository.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::files::{TempDir, repo_files, service_of, under};
use super::{detail, exec, ok, step, warn};

pub fn gen_admin() -> Result<()> {
    step("the admin page manifest from services/*/openapi.yaml");
    exec::status("tabkit", &["gen", "admin"], &[])
}

pub fn gen_sqlc() -> Result<()> {
    for config in under("services", "sqlc.yaml") {
        let dir = config.parent().context("a config has a folder")?;
        step(&format!("sqlc: {}", dir.display()));
        exec::status_in(dir, "sqlc", &["generate"], &[])?;
    }
    Ok(())
}

fn sdk_manifest(service: &str) -> String {
    format!(
        r#"{{
  "name": "@sdks/{service}",
  "version": "0.0.0",
  "private": true,
  "description": "Generated TypeScript client types for {service}, per ADR-0303. Do not edit.",
  "type": "module",
  "exports": {{
    ".": "./index.d.ts"
  }},
  "types": "./index.d.ts"
}}
"#
    )
}

pub fn gen_openapi() -> Result<()> {
    for spec in under("services", "openapi.yaml") {
        let service = service_of(&spec);
        let text = spec.to_string_lossy();
        let (go_out, ts_out) = (
            format!("libs/go/sdks/{service}"),
            format!("libs/ts/sdks/{service}"),
        );
        let package: String = service
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        step(&format!("{service}: Go SDK with ogen"));
        fs::create_dir_all(&go_out)?;
        exec::status(
            "go",
            &[
                "tool",
                "ogen",
                "--target",
                &go_out,
                "--package",
                &package,
                "--clean",
                &text,
            ],
            &[],
        )?;
        step(&format!("{service}: TS client"));
        fs::create_dir_all(&ts_out)?;
        let types = format!("{ts_out}/index.d.ts");
        exec::status(
            "node",
            &[
                "--import",
                "./scripts/lib/typescript6.mjs",
                "node_modules/openapi-typescript/bin/cli.js",
                &text,
                "--output",
                &types,
            ],
            &[],
        )?;
        fs::write(format!("{ts_out}/package.json"), sdk_manifest(&service))?;
    }
    // CI installs with a frozen lockfile, which rejects a lockfile that does not know the manifests above.
    let _ = exec::succeeds("bun", &["install", "--linker=hoisted"]);
    Ok(())
}

const RESOLVE: &str = r#".info.x-audience as $svc | (.paths.[].[] | select(tag == "!!map")) |= (.x-audience = (.x-audience // $svc // "cluster"))"#;
const MERGE: &str = "(. as $item ireduce ({}; . * $item))";
const STRIP_EXT: &str =
    r#"(.. | select(tag == "!!map")) |= with_entries(select(.key | test("^x-") | not))"#;
const ENVELOPE: &str = r#".openapi = "3.1.0"
  | .info = {"title": "API reference", "version": "1.0.0"}
  | .servers = [{"url": "/api"}]
  | .tags = ([.paths[][].tags // [] | .[]] | unique | map({"name": .}))"#;
const DROP_EMPTY: &str = r#"del(.paths.* | select(tag == "!!map" and length == 0))"#;
const PRUNE_ORPHANS: &str = r##"([.. | select(tag == "!!map" and has("$ref")) | .["$ref"]]) as $refs
  | .components.schemas |= with_entries(.key as $k | select($refs | any_c(. == "#/components/schemas/" + $k)))"##;
const LEAK: &str = r#"[.. | select(tag == "!!map") | keys.[] | select(test("^x-"))] | length"#;

pub fn gen_openapi_public() -> Result<()> {
    const OUT: &str = "apps/frontend/public/devportal/openapi";
    let work = TempDir::new("portal")?;
    let mut resolved = Vec::new();
    for spec in under("services", "openapi.yaml") {
        let service = service_of(&spec);
        if service == "_template" {
            continue;
        }
        let path = work.0.join(format!("{service}.yaml"));
        fs::write(
            &path,
            exec::output("yq", &[RESOLVE, &spec.to_string_lossy()])?,
        )?;
        resolved.push(path.to_string_lossy().into_owned());
    }
    if Path::new(OUT).exists() {
        fs::remove_dir_all(OUT)?;
    }
    fs::create_dir_all(OUT)?;
    for (label, file, filter) in [
        (
            "dev portal projection, audience >= internal",
            "internal.json",
            r#".x-audience == "cluster""#,
        ),
        (
            "public docs projection, audience == public",
            "public.json",
            r#".x-audience != "public""#,
        ),
    ] {
        step(label);
        let expression = format!(
            "{MERGE}\n  | del(.paths.*.* | select(tag == \"!!map\" and {filter}))\n  | {DROP_EMPTY} | {PRUNE_ORPHANS} | {ENVELOPE} | {STRIP_EXT}"
        );
        let mut args = vec!["ea", "-o=json", expression.as_str()];
        args.extend(resolved.iter().map(String::as_str));
        let target = format!("{OUT}/{file}");
        fs::write(&target, exec::output("yq", &args)?)?;
        if exec::output("yq", &["-o=json", LEAK, &target])?.trim() != "0" {
            bail!("x- extension key leaked into {target}. strip_ext failed");
        }
    }
    Ok(())
}

fn snapshot() -> Result<BTreeMap<String, String>> {
    let files = repo_files()?;
    let mut out = BTreeMap::new();
    for chunk in files.chunks(200) {
        let mut args = vec!["--"];
        args.extend(chunk.iter().map(String::as_str));
        let text = exec::lossy("sha256sum", &args).unwrap_or_default();
        for line in text.lines() {
            if let Some((hash, path)) = line.split_once("  ") {
                out.insert(path.to_string(), hash.to_string());
            }
        }
    }
    Ok(out)
}

fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn ci_gen() -> Result<()> {
    let before = snapshot()?;
    if before.is_empty() {
        bail!("no files found. The listing is broken, and the tree is not empty");
    }
    exec::status("just", &["gen"], &[])?;
    exec::status("just", &["format"], &[])?;
    let paths = changed(&before, &snapshot()?);
    if paths.is_empty() {
        ok("no drift: generated and formatted artifacts are current");
        return Ok(());
    }
    println!(
        "::error::Generated or formatted artifacts are out of date. Run 'just ci-gen' and commit the result."
    );
    for path in paths {
        println!("{path}");
    }
    bail!("generated or formatted artifacts are out of date");
}

const FAKE_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// The `outputHash` of the derivation with this name in the text of a flake.
fn output_hash(flake: &str, name: &str) -> Option<String> {
    let start = flake.find(&format!("name = \"{name}\";"))?;
    let line = flake[start..]
        .lines()
        .find(|l| l.contains("outputHash = \""))?;
    let from = line.find("sha256-")?;
    let rest = &line[from..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The real hash in the error of a build with a fake hash.
fn got_hash(log: &str) -> Option<String> {
    log.lines().find_map(|line| {
        let rest = line.split("got:").nth(1)?.trim();
        rest.starts_with("sha256-")
            .then(|| rest.split_whitespace().next().unwrap_or(rest).to_string())
    })
}

pub fn nix_hashes() -> Result<()> {
    let system = exec::output(
        "nix",
        &[
            "eval",
            "--raw",
            "--impure",
            "--expr",
            "builtins.currentSystem",
        ],
    )?;
    for name in ["go-modules", "helm-charts"] {
        let text = fs::read_to_string("flake.nix")?;
        let current = output_hash(&text, name)
            .with_context(|| format!("no outputHash for {name} in flake.nix"))?;
        fs::write("flake.nix", text.replace(&current, FAKE_HASH))?;
        let build = std::process::Command::new("nix")
            .args(["build", &format!(".#packages.{system}.{name}"), "--no-link"])
            .output()
            .context("cannot run nix")?;
        let log = String::from_utf8_lossy(&build.stderr);
        let got = got_hash(&log);
        let text = fs::read_to_string("flake.nix")?;
        fs::write(
            "flake.nix",
            text.replace(FAKE_HASH, got.as_deref().unwrap_or(&current)),
        )?;
        match got {
            Some(hash) => detail(&format!("{name}: {hash}")),
            None => detail(&format!("{name}: unchanged")),
        }
    }
    ok("flake.nix holds the current hashes");
    Ok(())
}

pub fn perf_rederive() -> Result<()> {
    step("re-deriving the measured numbers");
    detail("committed values, as the ADR set states them:");
    let mut adrs: Vec<_> = fs::read_dir("docs/adr")?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .collect();
    adrs.sort();
    let mut shown = 0;
    for adr in adrs
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
    {
        for (number, line) in fs::read_to_string(adr)?.lines().enumerate() {
            if line.contains("(measured)") && shown < 20 {
                println!("    {}:{}:{line}", adr.display(), number + 1);
                shown += 1;
            }
        }
    }
    println!();
    detail("resource governance, computed from the charts as they stand now:");
    let governance = std::process::Command::new("just")
        .arg("lint-resource-governance")
        .output()
        .context("cannot run just")?;
    for stream in [&governance.stdout, &governance.stderr] {
        for line in String::from_utf8_lossy(stream)
            .lines()
            .filter(|l| l.contains("requests") || l.contains("containers"))
        {
            println!("    {line}");
        }
    }
    println!();
    detail("load suite. This needs a running cluster, per ADR-0601:");
    let load = std::process::Command::new("just")
        .arg("perf-smoke")
        .output()
        .context("cannot run just")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&load.stdout),
        String::from_utf8_lossy(&load.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    for line in &lines[lines.len().saturating_sub(20)..] {
        println!("    {line}");
    }
    if load.status.success() {
        ok("load suite ran");
    } else {
        warn("load suite did not run. It needs a cluster, and a missing cluster is not drift");
    }
    println!();
    warn("compare the two by hand, and change the ADR only when the platform changed");
    warn("a number measured on different hardware is a different measurement, not a regression");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAKE: &str = "a = { name = \"go-modules\";\n outputHash = \"sha256-GO=\";\n};\nb = { name = \"helm-charts\";\n x = 1;\n outputHash = \"sha256-HELM=\";\n};\n";

    #[test]
    fn finds_the_hash_of_each_derivation() {
        assert_eq!(output_hash(FLAKE, "go-modules").unwrap(), "sha256-GO=");
        assert_eq!(output_hash(FLAKE, "helm-charts").unwrap(), "sha256-HELM=");
        assert_eq!(output_hash(FLAKE, "other"), None);
    }

    #[test]
    fn reads_the_real_hash_from_a_build_error() {
        let log = "error: hash mismatch\n  specified: sha256-AAA=\n     got:    sha256-REAL+/=\n";
        assert_eq!(got_hash(log).unwrap(), "sha256-REAL+/=");
        assert_eq!(got_hash("built"), None);
    }

    #[test]
    fn lists_a_changed_a_new_and_a_deleted_file() {
        let before = BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        let after = BTreeMap::from([
            ("a".to_string(), "9".to_string()),
            ("c".to_string(), "3".to_string()),
        ]);
        assert_eq!(changed(&before, &after), vec!["a", "b", "c"]);
    }

    #[test]
    fn the_sdk_manifest_is_valid_json() {
        let value: serde_json::Value = serde_json::from_str(&sdk_manifest("orders")).unwrap();
        assert_eq!(value["name"], "@sdks/orders");
    }
}
