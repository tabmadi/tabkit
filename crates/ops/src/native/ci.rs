//! The steps that the CI workflows call.

use std::collections::VecDeque;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::buildx::Builder;
use super::files::{TempDir, under};
use super::forge::{Api, CiGit, json as parse, values_changed};
use super::{detail, exec, ok, required, step, values, warn};

fn git_ok(args: &[&str]) -> bool {
    exec::succeeds("git", args)
}

fn affected(args: &[&str]) -> Result<Value> {
    let mut all = vec!["ci-affected"];
    all.extend(args);
    parse(&exec::output("just", &all)?)
}

fn builds_image(root: &str) -> Value {
    let names: Vec<String> = under(root, "Dockerfile")
        .iter()
        .filter_map(|p| {
            p.parent()?
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .filter(|name| !name.starts_with('_'))
        .collect();
    json!(names)
}

fn promoted_commit() -> Option<String> {
    let subject = exec::output(
        "git",
        &[
            "log",
            "-1",
            "--format=%s",
            "--grep=^chore(deploy): promote ",
        ],
    )
    .ok()?;
    let sha: String = subject
        .trim()
        .strip_prefix("chore(deploy): promote ")?
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (!sha.is_empty()
        && git_ok(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
        && git_ok(&["merge-base", "--is-ancestor", &sha, "HEAD"]))
    .then_some(sha)
}

/// The `{service, cmd}` pairs as compact JSON, with `service` first.
fn image_pairs(services: &[Value], exists: &dyn Fn(&str, &str) -> bool) -> String {
    let mut pairs = Vec::new();
    for service in services.iter().filter_map(Value::as_str) {
        for cmd in ["server", "worker"] {
            if exists(service, cmd) {
                pairs.push(format!(
                    "{{\"service\":{},\"cmd\":{}}}",
                    json!(service),
                    json!(cmd)
                ));
            }
        }
    }
    format!("[{}]", pairs.join(","))
}

pub fn affected_matrix() -> Result<()> {
    let before = promoted_commit();
    if let Some(sha) = &before {
        let names = exec::output("git", &["diff", "--name-only", &format!("{sha}...HEAD")])?;
        if names.lines().all(|l| l.starts_with("infra/gitops/")) {
            println!("services=[]\napps=[]\nimages=[]");
            return Ok(());
        }
    }
    let mut manifest = match &before {
        Some(sha) => affected(&["--base", sha])?,
        None => affected(&["--all"])?,
    };
    if manifest["global"] == json!(true) {
        manifest["services"] = builds_image("services");
        manifest["apps"] = builds_image("apps");
    }
    println!("services={}", manifest["services"]);
    println!("apps={}", manifest["apps"]);
    let services = manifest["services"].as_array().cloned().unwrap_or_default();
    let images = image_pairs(&services, &|service, cmd| {
        Path::new(&format!("services/{service}/cmd/{cmd}")).is_dir()
    });
    println!("images={images}");
    Ok(())
}

pub fn affected_adversarial() -> Result<()> {
    let base = std::env::var("BASE_REF")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let work = TempDir::new("affected")?;
    let tree = work.0.join("tree");
    let tree_text = tree.to_string_lossy().into_owned();
    step(&format!("creating a scratch worktree at {base}"));
    exec::output(
        "git",
        &["worktree", "add", "--quiet", "--detach", &tree_text, &base],
    )?;
    let result = probe_services(&tree, &tree_text);
    let _ = exec::output("git", &["worktree", "remove", "--force", &tree_text]);
    let checked = result?;
    ok(&format!(
        "affected-detection selected all {checked} services when each was changed alone"
    ));
    Ok(())
}

fn probe_services(tree: &Path, tree_text: &str) -> Result<usize> {
    let base_sha = exec::output("git", &["-C", tree_text, "rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let (mut failed, mut checked) = (false, 0);
    let mut services: Vec<String> = fs::read_dir("services")?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('_'))
        .collect();
    services.sort();
    for service in services {
        let target = format!("services/{service}/README.md");
        let path = tree.join(&target);
        if !path.is_file() {
            warn(&format!("{service}: no README.md to perturb"));
            failed = true;
            continue;
        }
        writeln!(
            fs::OpenOptions::new().append(true).open(&path)?,
            "\n<!-- affected-detection probe -->"
        )?;
        exec::output("git", &["-C", tree_text, "add", &target])?;
        exec::output(
            "git",
            &[
                "-C",
                tree_text,
                "-c",
                "user.email=ci@local",
                "-c",
                "user.name=ci",
                "commit",
                "--quiet",
                "--no-verify",
                "-m",
                &format!("chore: perturb {service}"),
            ],
        )?;
        let out = Command::new("tabkit")
            .args(["affected", "--base", &base_sha])
            .current_dir(tree)
            .output()
            .context("cannot run tabkit")?;
        let manifest = parse(&String::from_utf8_lossy(&out.stdout))?;
        let named = manifest["services"]
            .as_array()
            .is_some_and(|s| s.iter().any(|v| v == service.as_str()));
        if named || manifest["global"] == json!(true) {
            detail(&format!("{service}: selected"));
        } else {
            warn(&format!(
                "{service}: changed and not selected. CI would skip it"
            ));
            eprintln!("{manifest}");
            failed = true;
        }
        checked += 1;
        exec::output(
            "git",
            &["-C", tree_text, "reset", "--hard", "--quiet", &base_sha],
        )?;
    }
    if failed {
        bail!("affected-detection missed a changed service");
    }
    Ok(checked)
}

fn decide(run: bool, message: &str) -> Result<()> {
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|v| !v.is_empty()) {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(path)?,
            "{message}"
        )?;
    }
    println!("run={run}");
    Ok(())
}

fn base_ref() -> String {
    format!(
        "origin/{}",
        std::env::var("BASE_REF")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "master".to_string())
    )
}

pub fn e2e_gate() -> Result<()> {
    if std::env::var("LABELED").is_ok_and(|v| v == "true") {
        return decide(true, "→ the `e2e-smoke` label is set. Running the suite.");
    }
    let manifest = affected(&["--base", &base_ref(), "--runtime"])?;
    if manifest["global"] == json!(true) {
        return decide(true, "→ a global change. Running the suite.");
    }
    let count = manifest["services"].as_array().map_or(0, Vec::len);
    if count > 1 {
        return decide(
            true,
            &format!("→ {count} services affected. Only an e2e test sees a cross-service change."),
        );
    }
    decide(
        false,
        &format!("✓ {count} service(s) affected and no label. The smoke suite is skipped."),
    )
}

pub fn lighthouse_gate() -> Result<()> {
    let base = base_ref();
    let manifest = affected(&["--base", &base, "--runtime"])?;
    if manifest["global"] == json!(true) {
        return decide(true, "→ a global change. Measuring the frontend.");
    }
    if manifest["apps"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "frontend"))
    {
        return decide(true, "→ apps/frontend changed. Measuring the frontend.");
    }
    let changed = exec::output("git", &["diff", "--name-only", &format!("{base}...HEAD")])?;
    if changed.lines().any(|l| l.starts_with("libs/ts/")) {
        return decide(true, "→ a TypeScript lib changed. Measuring the frontend.");
    }
    decide(
        false,
        "✓ nothing that the frontend serves changed. Lighthouse is skipped.",
    )
}

/// The conclusion of the last run that did not skip, from the runs API.
fn last_conclusion(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["workflow_runs"]
                .as_array()?
                .iter()
                .find(|r| r["conclusion"] != "skipped")?["conclusion"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub fn nightly_gate() -> Result<()> {
    let event = std::env::var("EVENT").unwrap_or_default();
    if event != "schedule" {
        let shown = if event.is_empty() { "manual" } else { &event };
        return decide(
            true,
            &format!("→ {shown} is an explicit request. Running the suite."),
        );
    }
    if !exec::output("git", &["log", "--since=26 hours ago", "--oneline"])?
        .trim()
        .is_empty()
    {
        return decide(
            true,
            "→ new commits since the last nightly. Running the suite.",
        );
    }
    let variable = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let (Some(token), Some(repository), Some(workflow)) = (
        variable("GITHUB_TOKEN"),
        variable("GITHUB_REPOSITORY"),
        variable("WORKFLOW_FILE"),
    ) {
        let config = format!(
            "header = {}\n",
            exec::curl_quote(&format!("Authorization: Bearer {token}"))
        );
        let url = format!(
            "https://api.github.com/repos/{repository}/actions/workflows/{workflow}/runs?status=completed&per_page=20"
        );
        let body = exec::curl(&config, &["-sf", "--max-time", "20", &url])
            .map(|(_, out)| out)
            .unwrap_or_default();
        match last_conclusion(&body).as_str() {
            "success" => {}
            "" => {
                return decide(
                    true,
                    "→ could not read the last run's conclusion. Running, because a pass is not certain.",
                );
            }
            other => {
                return decide(
                    true,
                    &format!(
                        "→ the last completed run was '{other}'. Running, because a failure is not resolved."
                    ),
                );
            }
        }
    }
    if exec::output("date", &["-u", "+%d"])?.trim() == "01" {
        return decide(
            true,
            "→ no new commits, but it is the 1st. Running the monthly floor.",
        );
    }
    decide(
        false,
        "✓ no commits in the last 26h. Skipping tonight's run.",
    )
}

pub fn push_promotion() -> Result<()> {
    let sha = required("SHA", "SHA is unset")?;
    let branch = std::env::var("BRANCH")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "master".to_string());
    if !values_changed()? {
        ok("no values file changed");
        return Ok(());
    }
    let git = CiGit::configure()?;
    git.git(&["add", "infra/gitops"])?;
    let short: String = sha.chars().take(12).collect();
    git.git(&[
        "commit",
        "-q",
        "-m",
        &format!("chore(deploy): promote {short} [skip ci]"),
    ])?;
    let target = format!("HEAD:{branch}");
    for attempt in 1..=3 {
        if git.git(&["push", "-q", "origin", &target]).is_ok() {
            ok(&format!("promotion pushed to {branch}"));
            return Ok(());
        }
        warn(&format!(
            "push rejected on attempt {attempt}. Rebasing onto {branch}"
        ));
        git.git(&["pull", "-q", "--rebase", "origin", &branch])?;
    }
    bail!("could not push the promotion to {branch}");
}

pub fn release_pr() -> Result<()> {
    let version = required("VER", "VER is unset")?;
    let api = Api::from_env()?;
    let base = std::env::var("BASE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "master".to_string());
    let branch = format!("deploy/prod-{version}");
    let title = format!("chore(deploy): promote {version} to prod");
    if !values_changed()? {
        bail!("promote-prod changed no values file");
    }
    let git = CiGit::configure()?;
    git.git(&["switch", "-q", "-c", &branch])?;
    git.git(&["add", "infra/gitops"])?;
    git.git(&["commit", "-q", "-m", &title])?;
    git.git(&["push", "-q", "origin", &branch])?;
    let created = parse(&api.call(
        "POST",
        "/pulls",
        Some(&json!({ "head": branch, "base": base, "title": title })),
    )?)?;
    let Some(number) = created["number"].as_u64() else {
        bail!("the forge returned no pull request number");
    };
    detail(&format!("opened #{number}"));
    let merge = json!({ "Do": "squash", "MergeTitleField": title, "MergeMessageField": "", "merge_when_checks_succeed": true, "delete_branch_after_merge": true });
    api.call("POST", &format!("/pulls/{number}/merge"), Some(&merge))?;
    ok(&format!("#{number} merges when its checks pass"));
    Ok(())
}

pub fn release_publish() -> Result<()> {
    let version = required("VER", "VER is unset")?;
    Api::from_env()?.call(
        "POST",
        "/releases",
        Some(&json!({ "tag_name": version, "name": version })),
    )?;
    ok(&format!("release {version} published"));
    Ok(())
}

pub fn renovate() -> Result<()> {
    let _token = required("RENOVATE_TOKEN", "RENOVATE_TOKEN is unset")?;
    let api = required("GITHUB_API_URL", "GITHUB_API_URL is unset")?;
    let repository = required("GITHUB_REPOSITORY", "GITHUB_REPOSITORY is unset")?;
    let mut envs: Vec<(String, String)> = Vec::new();
    let platform = if api == "https://api.github.com" {
        let id = std::env::var("GITHUB_REPOSITORY_OWNER_ID")
            .context("GITHUB_REPOSITORY_OWNER_ID is unset")?;
        let owner =
            std::env::var("GITHUB_REPOSITORY_OWNER").context("GITHUB_REPOSITORY_OWNER is unset")?;
        envs.push((
            "RENOVATE_GIT_AUTHOR".into(),
            format!("Renovate <{id}+{owner}@users.noreply.github.com>"),
        ));
        "github"
    } else {
        "forgejo"
    };
    envs.push(("RENOVATE_PLATFORM".into(), platform.into()));
    envs.push(("RENOVATE_ENDPOINT".into(), api));
    envs.push(("RENOVATE_REPOSITORIES".into(), repository.clone()));
    envs.push((
        "RENOVATE_CONFIG_FILE".into(),
        ".config/renovate.json".into(),
    ));
    let variable = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let (Some(user), Some(token)) = (variable("DOCKERHUB_USERNAME"), variable("DOCKERHUB_TOKEN"))
    {
        let rules = json!([{ "hostType": "docker", "matchHost": "docker.io", "username": user, "password": token }]);
        envs.push(("RENOVATE_HOST_RULES".into(), rules.to_string()));
    } else {
        warn(
            "DOCKERHUB_USERNAME or DOCKERHUB_TOKEN unset. Docker Hub lookups are anonymous and rate limited",
        );
    }
    step(&format!(
        "running renovate on {repository} through {platform}"
    ));
    let pairs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    exec::status("renovate", &[], &pairs)?;
    ok("renovate finished. The dashboard issue lists what is pending");
    Ok(())
}

pub fn promote_verify() -> Result<()> {
    let sha = required("SHA", "SHA is unset")?;
    let variable = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if variable("ARGOCD_SERVER").is_none() || variable("ARGOCD_AUTH_TOKEN").is_none() {
        warn("ARGOCD_SERVER or ARGOCD_AUTH_TOKEN unset. Prod convergence is not verified");
        detail(&format!(
            "argocd app wait -l env=prod --revision \"{sha}\" --sync --health --timeout 600"
        ));
        return Ok(());
    }
    step(&format!("waiting for prod to converge on {sha}"));
    exec::status(
        "argocd",
        &[
            "app",
            "wait",
            "-l",
            "env=prod",
            "--revision",
            &sha,
            "--sync",
            "--health",
            "--timeout",
            "600",
        ],
        &[],
    )?;
    ok(&format!("prod converged on {sha}"));
    Ok(())
}

/// The parts of `<registry>/<repo>:<tag>`. A short name is a Docker Hub official image.
fn parse_reference(reference: &str) -> Result<(String, String, String)> {
    if reference.contains("@sha256:") {
        bail!("already digest-pinned: {reference}");
    }
    let Some((name, tag)) = reference.rsplit_once(':') else {
        bail!("no tag in reference: {reference}. A floating reference has nothing to pin");
    };
    if name.matches('/').count() >= 2 {
        let (registry, repo) = name.split_once('/').context("a name has a registry")?;
        Ok((registry.to_string(), repo.to_string(), tag.to_string()))
    } else {
        Ok((
            "registry-1.docker.io".to_string(),
            format!("library/{name}"),
            tag.to_string(),
        ))
    }
}

pub fn image_digest(args: &[String]) -> Result<()> {
    let Some(reference) = args.first().filter(|a| !a.is_empty()) else {
        bail!("usage: just image-digest <registry>/<repo>:<tag>");
    };
    let (registry, repo, tag) = parse_reference(reference)?;
    let accept = "Accept: application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.oci.image.manifest.v1+json,application/vnd.docker.distribution.manifest.v2+json";
    eprintln!("→ resolving {reference}");
    let url = format!("https://{registry}/v2/{repo}/manifests/{tag}");
    let (_, head) = exec::curl(
        "",
        &["-fsSL", "-o", "/dev/null", "-D", "-", "-H", accept, &url],
    )?;
    let digest = head.lines().rev().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("docker-content-digest")
            .then(|| value.trim().to_string())
    });
    let Some(digest) = digest.filter(|d| !d.is_empty()) else {
        bail!("registry returned no digest for {reference}");
    };
    let name = reference
        .rsplit_once(':')
        .map_or(reference.as_str(), |(n, _)| n);
    eprintln!("✓ {name}:{tag}@{digest}");
    println!("{digest}");
    Ok(())
}

fn user() -> Result<String> {
    Ok(exec::output("id", &["-un"])?.trim().to_string())
}

fn hosted_cache(scope: &str) -> Vec<String> {
    if std::env::var("RUNNER_ENVIRONMENT").is_ok_and(|v| v == "github-hosted") {
        vec![
            "--cache-from".to_string(),
            format!("type=gha,scope={scope}"),
            "--cache-to".to_string(),
            format!("type=gha,mode=max,scope={scope}"),
        ]
    } else {
        Vec::new()
    }
}

struct Build {
    name: String,
    context: String,
    file: String,
    args: Vec<String>,
}

/// Builds, pushes, and signs one image, and returns its whole log, so two builds never mix their lines.
fn build_one(
    build: &Build,
    builder: &Builder,
    prefix: &str,
    sha: &str,
    work: &Path,
) -> (bool, String) {
    let meta = work.join(format!("{}.json", build.name));
    let mut command = builder.command();
    command
        .args(["--progress", "plain", "--push", "--file", &build.file])
        .args([
            "--tag",
            &format!("{prefix}/{}:{sha}", build.name),
            "--metadata-file",
        ])
        .arg(&meta)
        .args(hosted_cache(&build.name))
        .args(&build.args)
        .arg(&build.context)
        .stdin(Stdio::null());
    let Ok(out) = command.output() else {
        return (false, format!("cannot run docker for {}\n", build.name));
    };
    let mut log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return (false, log);
    }
    let digest = fs::read_to_string(&meta)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v["containerimage.digest"].as_str().map(str::to_string));
    let Some(digest) = digest else {
        log.push_str("the build wrote no image digest\n");
        return (false, log);
    };
    match Command::new("just")
        .args(["ci-sign", &format!("{prefix}/{}@{digest}", build.name)])
        .stdin(Stdio::null())
        .output()
    {
        Ok(sign) => {
            log.push_str(&String::from_utf8_lossy(&sign.stdout));
            log.push_str(&String::from_utf8_lossy(&sign.stderr));
            (sign.status.success(), log)
        }
        Err(e) => {
            log.push_str("cannot run just: ");
            log.push_str(&e.to_string());
            log.push('\n');
            (false, log)
        }
    }
}

fn build_list(kind: &str, items: &[Value], sha: &str) -> Result<Vec<Build>> {
    let time = std::env::var("BUILD_TIME").unwrap_or_default();
    let mut builds = Vec::new();
    for item in items {
        if kind == "services" {
            let service = item["service"].as_str().context("an item has no service")?;
            let cmd = item["cmd"].as_str().context("an item has no cmd")?;
            builds.push(Build {
                name: format!("{service}-{cmd}"),
                context: ".".to_string(),
                file: format!("services/{service}/Dockerfile"),
                args: [
                    "--build-arg".to_string(),
                    format!("APP_CMD={cmd}"),
                    "--build-arg".to_string(),
                    format!("GIT_SHA={sha}"),
                    "--build-arg".to_string(),
                    "BUILD_VERSION=dev".to_string(),
                    "--build-arg".to_string(),
                    format!("BUILD_TIME={time}"),
                ]
                .to_vec(),
            });
        } else {
            let app = item.as_str().context("an item is not an app name")?;
            builds.push(Build {
                name: app.to_string(),
                context: ".".to_string(),
                file: format!("apps/{app}/Dockerfile"),
                args: vec!["--build-arg".to_string(), format!("SERVICE_VERSION={sha}")],
            });
        }
    }
    Ok(builds)
}

pub fn build_images(args: &[String]) -> Result<()> {
    let kind = args.first().map_or("", String::as_str);
    if kind != "services" && kind != "apps" {
        bail!("usage: just ci-build-images services|apps");
    }
    let prefix = required("IMAGE_PREFIX", "IMAGE_PREFIX is unset")?;
    let sha = required("GIT_SHA", "GIT_SHA is unset")?;
    let raw = std::env::var("ITEMS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "[]".to_string());
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        bail!("ITEMS is not a JSON array: {raw}");
    };
    let builder = Builder::ensure(&format!("ci-keep-{}", user()?), false)?;
    let parallel: usize = std::env::var("BUILD_PARALLEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(3);
    let work = TempDir::new("images")?;
    let builds = build_list(kind, &items, &sha)?;
    let names: Vec<String> = builds.iter().map(|b| b.name.clone()).collect();
    let queue = Arc::new(Mutex::new(builds.into_iter().collect::<VecDeque<_>>()));
    let failed = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..parallel.min(names.len().max(1)) {
            let (queue, failed) = (Arc::clone(&queue), Arc::clone(&failed));
            let (builder, prefix, sha, work_path) = (&builder, &prefix, &sha, &work.0);
            scope.spawn(move || {
                loop {
                    let next = queue
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pop_front();
                    let Some(build) = next else { break };
                    println!("→ building {}", build.name);
                    let (success, log) = build_one(&build, builder, prefix, sha, work_path);
                    print!("::group::{}\n{log}::endgroup::\n", build.name);
                    if !success {
                        failed
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(build.name);
                    }
                }
            });
        }
    });
    let mut failed = failed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if !failed.is_empty() {
        failed.sort();
        bail!(
            "these images did not build, push, or sign: {}",
            failed.join(" ")
        );
    }
    ok(&format!("every {kind} image is built, pushed, and signed"));
    Ok(())
}

fn preview_number() -> Result<String> {
    let pr = std::env::var("PREVIEW_PR").unwrap_or_default();
    if pr.is_empty() || !pr.bytes().all(|b| b.is_ascii_digit()) {
        bail!("PREVIEW_PR is not a pull request number: '{pr}'");
    }
    Ok(pr)
}

fn design_build(pr: &str) -> Result<()> {
    let prefix = required("IMAGE_PREFIX", "IMAGE_PREFIX is unset")?;
    let output = required("GITHUB_OUTPUT", "GITHUB_OUTPUT is unset")?;
    let work = TempDir::new("design")?;
    let src = work.0.join("src");
    let src_text = src.to_string_lossy().into_owned();
    exec::output(
        "git",
        &[
            "fetch",
            "-q",
            "--depth=1",
            "origin",
            &format!("+refs/pull/{pr}/head"),
        ],
    )?;
    let sha = exec::output("git", &["rev-parse", "FETCH_HEAD"])?
        .trim()
        .to_string();
    exec::output(
        "git",
        &["worktree", "add", "-q", "--detach", &src_text, &sha],
    )?;
    let result = design_image(pr, &prefix, &sha, &src_text, &work.0);
    let _ = exec::output("git", &["worktree", "remove", "--force", &src_text]);
    let reference = result?;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(output)?,
        "sha={sha}"
    )?;
    ok(&format!("pushed and signed {reference}"));
    Ok(())
}

fn design_image(pr: &str, prefix: &str, sha: &str, src: &str, work: &Path) -> Result<String> {
    let reference = format!("{prefix}/design/frontend:pr-{pr}-{sha}");
    let builder = Builder::ensure(&format!("ci-design-{}", user()?), true)?;
    step(&format!("building {reference}"));
    let meta = work.join("meta.json");
    let meta_text = meta.to_string_lossy().into_owned();
    let dockerfile = format!("{src}/apps/frontend/Dockerfile");
    let version = format!("SERVICE_VERSION={sha}");
    let cache = hosted_cache("design-frontend");
    let mut command = builder.command();
    command.args([
        "--progress",
        "plain",
        "--push",
        "--file",
        &dockerfile,
        "--tag",
        &reference,
        "--metadata-file",
        &meta_text,
    ]);
    command.args(cache).args([
        "--build-arg",
        "NEXT_PUBLIC_FIXTURES=1",
        "--build-arg",
        "NEXT_PUBLIC_DESIGN_BUILD=1",
        "--build-arg",
        &version,
        src,
    ]);
    let status = command.status().context("cannot run Docker Buildx")?;
    if !status.success() {
        bail!("Docker Buildx failed with {status}");
    }
    let digest = parse(&fs::read_to_string(&meta)?)?["containerimage.digest"]
        .as_str()
        .context("the build wrote no image digest")?
        .to_string();
    exec::status(
        "just",
        &["ci-sign", &format!("{prefix}/design/frontend@{digest}")],
        &[],
    )?;
    Ok(reference)
}

const MARKER: &str = "<!-- design-preview -->";

fn design_comment(pr: &str) -> Result<()> {
    let api = Api::from_env()?;
    let sha = required("PREVIEW_SHA", "PREVIEW_SHA is unset")?;
    let values_text = fs::read_to_string("infra/gitops/platform/dev/values.yaml")?;
    let host = values::get(&values_text, ".envHost")?;
    let url = format!("https://design-pr-{pr}.ops.{host}/");
    let body = format!(
        "{MARKER}\nThe design preview of this pull request is at {url}\n\nIt shows commit `{sha}` in fixture mode. Log in as an operator to open it. Argo CD deletes it when this pull request closes or the `run/design-preview` label goes, per ADR-0701."
    );
    let payload = json!({ "body": body });
    let comments = parse(&api.call("GET", &format!("/issues/{pr}/comments?limit=100"), None)?)?;
    let existing = comments
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|c| c["body"].as_str().is_some_and(|b| b.starts_with(MARKER)))
        })
        .and_then(|c| c["id"].as_u64());
    match existing {
        Some(id) => api.call("PATCH", &format!("/issues/comments/{id}"), Some(&payload))?,
        None => api.call("POST", &format!("/issues/{pr}/comments"), Some(&payload))?,
    };
    ok(&format!("posted {url} on #{pr}"));
    Ok(())
}

pub fn design_preview(args: &[String]) -> Result<()> {
    let verb = args.first().map_or("", String::as_str);
    if verb != "build" && verb != "comment" {
        bail!("usage: just ci-design-preview build|comment");
    }
    let pr = preview_number()?;
    if verb == "build" {
        design_build(&pr)
    } else {
        design_comment(&pr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_only_the_commands_that_exist() {
        let services = [json!("orders"), json!("catalog")];
        let pairs = image_pairs(&services, &|service, cmd| {
            cmd == "server" || service == "orders"
        });
        assert_eq!(
            pairs,
            r#"[{"service":"orders","cmd":"server"},{"service":"orders","cmd":"worker"},{"service":"catalog","cmd":"server"}]"#
        );
    }

    #[test]
    fn the_last_run_skips_a_skipped_run() {
        let body = r#"{"workflow_runs":[{"conclusion":"skipped"},{"conclusion":"failure"},{"conclusion":"success"}]}"#;
        assert_eq!(last_conclusion(body), "failure");
        assert_eq!(last_conclusion("not json"), "");
    }

    #[test]
    fn reads_an_image_reference() {
        assert_eq!(
            parse_reference("ghcr.io/a/b:1").unwrap(),
            ("ghcr.io".into(), "a/b".into(), "1".into())
        );
        assert_eq!(
            parse_reference("postgres:19").unwrap(),
            (
                "registry-1.docker.io".into(),
                "library/postgres".into(),
                "19".into()
            )
        );
        assert!(parse_reference("ghcr.io/a/b@sha256:abc").is_err());
        assert!(parse_reference("postgres").is_err());
    }

    #[test]
    fn builds_each_kind_with_its_context_and_arguments() {
        let services = build_list(
            "services",
            &[json!({"service":"orders","cmd":"worker"})],
            "abc",
        )
        .unwrap();
        assert_eq!(services[0].name, "orders-worker");
        assert!(services[0].args.contains(&"APP_CMD=worker".to_string()));
        let apps = build_list("apps", &[json!("admin"), json!("frontend")], "abc").unwrap();
        assert!(apps.iter().all(|a| a.context == "."));
        assert_eq!(apps[0].file, "apps/admin/Dockerfile");
    }
}
