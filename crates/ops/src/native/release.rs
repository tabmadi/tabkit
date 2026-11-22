//! The release, the scans, and the steps around them.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::files::{TempDir, under};
use super::{detail, exec, ok, sign, step, values, warn};

/// The next CalVer and its SemVer form. A new month resets the last number.
fn next_version(year: &str, month: &str, last: Option<&str>) -> Result<(String, String)> {
    let mut micro = 0_u32;
    if let Some((prefix, number)) = last.and_then(|tag| tag.rsplit_once('.'))
        && prefix == format!("v{year}.{month}")
    {
        micro = number
            .parse::<u32>()
            .context("the last tag has no number")?
            + 1;
    }
    let plain: u32 = month.parse().context("the month is not a number")?;
    Ok((
        format!("v{year}.{month}.{micro}"),
        format!("{year}.{plain}.{micro}"),
    ))
}

fn charts(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            charts(&path, out)?;
        } else if path.file_name().is_some_and(|n| n == "Chart.yaml") {
            out.push(path);
        }
    }
    Ok(())
}

fn stamp_charts(semver: &str) -> Result<usize> {
    let mut files = Vec::new();
    charts(Path::new("infra/helm"), &mut files)?;
    files.sort();
    for chart in &files {
        let text = fs::read_to_string(chart)?;
        let (version, app) = (
            values::get(&text, ".version")?,
            values::get(&text, ".appVersion")?,
        );
        values::set(chart, ".version", semver)?;
        if !app.is_empty() && app == version {
            values::set(chart, ".appVersion", semver)?;
        }
    }
    Ok(files.len())
}

fn stamp_packages(semver: &str) -> Result<bool> {
    let mut stamped = false;
    for package in under("apps", "package.json") {
        let text = package.to_string_lossy().into_owned();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&package)?).with_context(|| text.clone())?;
        if value.get("version").is_none() {
            continue;
        }
        // jq keeps the order of the keys, so the diff shows only the version.
        let new = exec::output("jq", &["--arg", "v", semver, ".version = $v", &text])?;
        fs::write(&package, new)?;
        stamped = true;
        detail(&format!("stamped {text}"));
    }
    Ok(stamped)
}

pub fn release() -> Result<()> {
    let dry = std::env::var("DRY_RUN").is_ok_and(|v| !v.is_empty());
    let year = exec::output("date", &["-u", "+%Y"])?.trim().to_string();
    let month = exec::output("date", &["-u", "+%m"])?.trim().to_string();
    let tags = exec::output(
        "git",
        &[
            "tag",
            "-l",
            "v[0-9][0-9][0-9][0-9].[0-9][0-9].*",
            "--sort=-v:refname",
        ],
    )?;
    let last = tags.lines().next();
    let (version, semver) = next_version(&year, &month, last)?;
    step(&format!(
        "releasing {version}, previous: {}",
        last.unwrap_or("none")
    ));
    detail(&format!("stamped version fields: {semver}"));
    step("listing the design-only pages");
    exec::status("tabkit", &["check", "--only", "frontend/fixture"], &[])?;
    if exec::succeeds(
        "git",
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{version}"),
        ],
    ) {
        bail!("{version} already exists. A release tag is immutable");
    }
    let count = stamp_charts(&semver)?;
    detail(&format!("stamped {count} chart(s)"));
    if stamp_packages(&semver)? {
        exec::output("bun", &["install", "--lockfile-only"])?;
        detail("refreshed bun.lock");
    }
    fs::write(".template-version", format!("{version}\n"))?;
    detail("stamped .template-version");
    step("rendering CHANGELOG.md");
    let changelog = exec::output("cog", &["changelog"])?;
    fs::write("CHANGELOG.md", &changelog)?;
    ok(&format!(
        "CHANGELOG.md written, {} lines",
        changelog.lines().count()
    ));
    if dry {
        warn("DRY_RUN set. Not committing, tagging, or pushing");
        detail(&format!("would commit: chore(release): {version}"));
        detail(&format!("would tag:    {version}"));
        detail(
            "the stamped fields, bun.lock, and CHANGELOG.md stay in the working tree to inspect.",
        );
        detail(
            "  git checkout infra/helm apps/*/package.json bun.lock CHANGELOG.md   discards them",
        );
        detail("  git checkout .template-version 2>/dev/null || rm -f .template-version");
        return Ok(());
    }
    exec::status("git", &["add", "-A"], &[])?;
    exec::status(
        "git",
        &["commit", "-m", &format!("chore(release): {version}")],
        &[],
    )?;
    exec::status("git", &["tag", "-a", &version, "-m", &version], &[])?;
    exec::status(
        "git",
        &["push", "origin", "HEAD", &format!("refs/tags/{version}")],
        &[],
    )?;
    ok(&format!("released {version}"));
    Ok(())
}

fn maps(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let kind = path.symlink_metadata()?;
        if kind.is_dir() {
            maps(&path, out)?;
        } else if kind.is_file() && path.to_string_lossy().ends_with(".js.map") {
            out.push(path);
        }
    }
    Ok(())
}

pub fn frontend_sourcemaps(args: &[String]) -> Result<()> {
    const BUILD: &str = "apps/frontend/.next";
    let variable = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let release = args
        .first()
        .filter(|a| !a.is_empty())
        .cloned()
        .or_else(|| variable("GIT_SHA"))
        .or_else(|| {
            exec::output("git", &["rev-parse", "--short", "HEAD"])
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let out = format!("apps/frontend/sourcemaps-{release}.tar.zst");
    if !Path::new(BUILD).is_dir() {
        bail!("no build at {BUILD}. Run the production build first");
    }
    step(&format!("collecting source maps for release {release}"));
    let mut files = Vec::new();
    maps(Path::new(BUILD), &mut files)?;
    if files.is_empty() {
        bail!("no .js.map files in {BUILD}. Check that productionBrowserSourceMaps is on");
    }
    let work = TempDir::new("maps")?;
    let list = work.0.join("list");
    let names: Vec<String> = files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    fs::write(&list, names.join("\0") + "\0")?;
    exec::status(
        "tar",
        &[
            "--null",
            "-T",
            &list.to_string_lossy(),
            "-c",
            "--zstd",
            "-f",
            &out,
        ],
        &[],
    )?;
    detail(&format!("archived {} maps to {out}", files.len()));
    for file in &files {
        fs::remove_file(file)?;
    }
    ok(&format!(
        "collected {} source maps and removed them from the build output",
        files.len()
    ));
    let Some(registry) = variable("SOURCEMAP_REGISTRY").or_else(|| variable("IMAGE_REGISTRY"))
    else {
        warn(&format!(
            "SOURCEMAP_REGISTRY is unset. The archive stays at {out} and is not stored"
        ));
        warn("a release whose maps are only on this machine cannot be symbolicated later");
        return Ok(());
    };
    let reference = format!("{registry}/frontend-sourcemaps:{release}");
    step(&format!("pushing {out} to {reference}"));
    let mut command = vec!["push", reference.as_str()];
    if ["registry.localhost:", "127.0.0.1:", "localhost:"]
        .iter()
        .any(|p| registry.starts_with(p))
    {
        command.push("--plain-http");
    }
    let revision = format!("org.opencontainers.image.revision={release}");
    let layer = format!("{out}:application/zstd");
    command.extend([
        "--artifact-type", "application/vnd.platform.sourcemaps.v1+zstd",
        "--annotation", &revision,
        "--annotation", "org.opencontainers.image.description=Browser source maps, kept for each release, per ADR-0503. Never served to a browser.",
        &layer,
    ]);
    exec::status("oras", &command, &[])?;
    ok(&format!("source maps for {release} stored at {reference}"));
    detail(&format!("retrieve with: oras pull {reference}"));
    Ok(())
}

const SCAN_SKIP: [&str; 14] = [
    "--skip-dirs",
    "**/charts",
    "--skip-dirs",
    "**/node_modules",
    "--skip-dirs",
    "**/.next",
    "--skip-dirs",
    "**/dist",
    "--skip-dirs",
    "**/test-results",
    "--skip-dirs",
    "**/playwright-report",
    "--skip-files",
    "**/*.dockerignore",
];

fn trivy(front: &[&str]) -> Result<()> {
    let mut args = vec!["fs"];
    args.extend(front);
    args.push("--no-progress");
    args.extend(SCAN_SKIP);
    args.push(".");
    exec::status("trivy", &args, &[])
}

pub fn scan(args: &[String]) -> Result<()> {
    if args.first().is_some_and(|a| a == "report") {
        step("every dependency finding, fixed or not, at every severity");
        trivy(&["--scanners", "vuln"])?;
        step("every misconfiguration, at every severity");
        return trivy(&["--scanners", "misconfig"]);
    }
    let severity = std::env::var("TRIVY_SEVERITY")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "HIGH,CRITICAL".to_string());
    step(&format!("scanning dependencies, severity {severity}"));
    trivy(&[
        "--scanners",
        "vuln",
        "--severity",
        &severity,
        "--ignore-unfixed",
        "--exit-code",
        "1",
    ])?;
    ok(&format!("no fixable {severity} findings"));
    step(&format!(
        "scanning manifests and Dockerfiles, severity {severity}"
    ));
    trivy(&[
        "--scanners",
        "misconfig",
        "--severity",
        &severity,
        "--exit-code",
        "1",
        "--ignorefile",
        ".config/trivyignore.yaml",
    ])?;
    ok(&format!("no {severity} misconfigurations"));
    Ok(())
}

pub fn act_local(args: &[String]) -> Result<()> {
    let variable = |name: &str, default: &str| {
        std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let (proxy, image, event, env_file) = (
        variable("ACT_PROXY", ""),
        variable("ACT_IMAGE", "catthehacker/ubuntu:act-latest"),
        variable("ACT_EVENT", "pull_request"),
        variable("ACT_ENV_FILE", ".env"),
    );
    let mut no_proxy = "127.0.0.1,localhost".to_string();
    let route = exec::lossy("ip", &["-4", "route", "get", "1.1.1.1"]).unwrap_or_default();
    let words: Vec<&str> = route.split_whitespace().collect();
    if let Some(ip) = words.windows(2).find(|w| w[0] == "src").map(|w| w[1]) {
        no_proxy.push(',');
        no_proxy.push_str(ip);
    }
    let mut command: Vec<String> = vec![
        event.clone(),
        "-P".into(),
        format!("ubuntu-latest={image}"),
        "-P".into(),
        format!("ubuntu-24.04={image}"),
        "--network".into(),
        "host".into(),
    ];
    if !proxy.is_empty() {
        step(&format!("checking proxy {proxy}"));
        if !exec::succeeds(
            "curl",
            &[
                "-sS",
                "-o",
                "/dev/null",
                "-m",
                "10",
                "-x",
                &proxy,
                "https://cache.nixos.org/nix-cache-info",
            ],
        ) {
            bail!("proxy {proxy} is unreachable. A job needs it to download Nix and the toolchain");
        }
        for pair in [
            format!("HTTP_PROXY={proxy}"),
            format!("HTTPS_PROXY={proxy}"),
            format!("NO_PROXY={no_proxy}"),
        ] {
            command.push("--env".into());
            command.push(pair);
        }
    }
    if Path::new(&env_file).is_file() {
        command.push("--secret-file".into());
        command.push(env_file);
    }
    command.push("-e".into());
    command.push(format!(".github/act/{event}.json"));
    command.extend(args.iter().cloned());
    step(&format!("running act {event} on {image}"));
    let refs: Vec<&str> = command.iter().map(String::as_str).collect();
    exec::status("act", &refs, &[])
}

pub fn dns(args: &[String]) -> Result<()> {
    const CONFIG: &str = "infra/dns/dnsconfig.js";
    const CREDS: &str = "infra/dns/creds.json";
    const SECRET: &str = "infra/dns/secrets.enc.yaml";
    let [verb] = args else {
        bail!("usage: tabkit-ops dns <check|apply>");
    };
    if verb != "check" && verb != "apply" {
        bail!("usage: tabkit-ops dns <check|apply>");
    }
    let creds: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(CREDS)?).context("a bad creds file")?;
    if creds["zone"]["TYPE"] == "NONE" {
        if verb != "check" {
            bail!(
                "{CREDS} names no provider yet. Replace its 'zone' entry. See infra/dns/README.md"
            );
        }
        ok(&format!(
            "{CREDS} names no provider yet. Nothing to compare"
        ));
        return Ok(());
    }
    let mut secrets = Vec::new();
    if Path::new(SECRET).is_file() {
        let plain = exec::output("sops", &["--decrypt", SECRET]).with_context(|| {
            format!("cannot decrypt {SECRET}. Your age key is not a recipient of it")
        })?;
        secrets = sign::all_string_data(&plain)?;
    }
    let envs: Vec<(&str, &str)> = secrets
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if verb == "check" {
        step(&format!("comparing {CONFIG} with the provider"));
        if exec::status(
            "dnscontrol",
            &[
                "preview",
                "--expect-no-changes",
                "--config",
                CONFIG,
                "--creds",
                CREDS,
            ],
            &envs,
        )
        .is_err()
        {
            bail!("the provider differs from {CONFIG}. Run 'just dns-apply'");
        }
    } else {
        warn(&format!(
            "apply deletes provider records that {CONFIG} does not declare"
        ));
        step(&format!("reconciling the provider with {CONFIG}"));
        exec::status(
            "dnscontrol",
            &["push", "--config", CONFIG, "--creds", CREDS],
            &envs,
        )?;
    }
    ok(&format!("the provider matches {CONFIG}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_month_counts_up() {
        assert_eq!(
            next_version("2026", "08", Some("v2026.08.3")).unwrap(),
            ("v2026.08.4".into(), "2026.8.4".into())
        );
    }

    #[test]
    fn a_new_month_resets_and_semver_has_no_leading_zero() {
        assert_eq!(
            next_version("2026", "09", Some("v2026.08.3")).unwrap(),
            ("v2026.09.0".into(), "2026.9.0".into())
        );
        assert_eq!(
            next_version("2026", "11", None).unwrap(),
            ("v2026.11.0".into(), "2026.11.0".into())
        );
    }
}
