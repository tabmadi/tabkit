//! Builds the first-party images of the full tier, one lane at a time for each Dockerfile family.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use super::cluster::Cluster;
use super::{detail, exec, step, values, warn};

const REG: &str = "registry.localhost:5000";
const PUSH_REG: &str = "127.0.0.1:5000";

fn logged(log: &mut String, program: &str, args: &[&str]) -> bool {
    let Ok(out) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    else {
        log.push_str("cannot run ");
        log.push_str(program);
        log.push('\n');
        return false;
    };
    log.push_str(&String::from_utf8_lossy(&out.stdout));
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    out.status.success()
}

/// Builds, pushes, and loads one image. A slow link can break a fetch, and the cached layers make a retry cheap.
fn build_push(
    cluster: &str,
    log: &mut String,
    name: &str,
    dockerfile: &str,
    context: &str,
    build_args: &[String],
) -> Result<()> {
    let (pushed, local) = (
        format!("{PUSH_REG}/{name}:local"),
        format!("{REG}/{name}:local"),
    );
    let contexts = super::buildx::contexts(dockerfile)?;
    for attempt in 1..=3 {
        let mut args = vec!["build", "-t", pushed.as_str(), "-f", dockerfile];
        args.extend(contexts.iter().map(String::as_str));
        args.extend(build_args.iter().map(String::as_str));
        args.push(context);
        if logged(log, "docker", &args)
            && logged(log, "docker", &["push", &pushed])
            && logged(log, "docker", &["tag", &pushed, &local])
            && logged(
                log,
                "kind",
                &["load", "docker-image", &local, "--name", cluster],
            )
            && logged(log, "docker", &["rmi", &local])
        {
            return Ok(());
        }
        log.push_str("  build and push of ");
        log.push_str(name);
        log.push_str(" attempt ");
        log.push_str(&attempt.to_string());
        log.push_str(
            " failed, retrying
",
        );
    }
    bail!("could not build and push {name} after 3 attempts");
}

fn build_identity() -> Vec<String> {
    let mut rev = exec::output("git", &["rev-parse", "--short=12", "HEAD"])
        .map_or_else(|_| "unknown".to_string(), |o| o.trim().to_string());
    let mut built = Command::new("git")
        .args([
            "log",
            "-1",
            "--format=%cd",
            "--date=format-local:%Y-%m-%dT%H:%M:%SZ",
        ])
        .env("TZ", "UTC")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !exec::succeeds("git", &["diff", "--quiet"]) {
        rev.push_str("-dirty");
        built.clear();
    }
    if built.is_empty() {
        built = exec::output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
            .map(|o| o.trim().to_string())
            .unwrap_or_default();
    }
    vec![rev, built]
}

fn lane_services(cluster: &str, log: &mut String, identity: &[String]) -> Result<()> {
    let mut files: Vec<_> = fs::read_dir("infra/gitops/services/local/values")?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    files.sort();
    let common = [
        "--build-arg".to_string(),
        format!("GIT_SHA={}", identity[0]),
        "--build-arg".to_string(),
        "BUILD_VERSION=local".to_string(),
        "--build-arg".to_string(),
        format!("BUILD_TIME={}", identity[1]),
    ];
    for file in files {
        let service = file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let dockerfile = format!("services/{service}/Dockerfile");
        if !Path::new(&dockerfile).is_file() {
            continue;
        }
        let text = fs::read_to_string(&file)?;
        for (kind, wanted) in [
            ("server", values::get(&text, ".server.enabled")? != "false"),
            ("worker", values::get(&text, ".worker.enabled")? == "true"),
        ] {
            if !wanted {
                continue;
            }
            let mut args = vec![
                "--build-arg".to_string(),
                format!("SERVICE={service}"),
                "--build-arg".to_string(),
                format!("APP_CMD={kind}"),
            ];
            args.extend(common.iter().cloned());
            build_push(
                cluster,
                log,
                &format!("{service}-{kind}"),
                &dockerfile,
                ".",
                &args,
            )?;
        }
    }
    Ok(())
}

fn lane_frontend(cluster: &str, log: &mut String, identity: &[String]) -> Result<()> {
    let text = fs::read_to_string("infra/gitops/services/local/values/frontend.yaml")?;
    let origin = values::get(&text, ".env.EDGE_PUBLIC_ORIGIN")?;
    let args = [
        "--build-arg".to_string(),
        format!("SERVICE_VERSION={}", identity[0]),
        "--build-arg".to_string(),
        format!("EDGE_PUBLIC_ORIGIN={origin}"),
    ];
    build_push(
        cluster,
        log,
        "frontend",
        "apps/frontend/Dockerfile",
        ".",
        &args,
    )
}

pub fn stage(cluster: &Cluster) -> Result<()> {
    step(&format!(
        "building and pushing repo images to {REG}: the services, admin, and frontend lanes run at once"
    ));
    let identity = build_identity();
    let name = cluster.name();
    let results: Vec<(&str, Result<()>, String)> = std::thread::scope(|scope| {
        let (name, identity) = (&name, &identity);
        let lanes = [
            (
                "services",
                scope.spawn(move || {
                    let mut log = String::new();
                    let r = lane_services(name, &mut log, identity);
                    (r, log)
                }),
            ),
            (
                "admin",
                scope.spawn(move || {
                    let mut log = String::new();
                    let r = build_push(name, &mut log, "admin", "apps/admin/Dockerfile", ".", &[]);
                    (r, log)
                }),
            ),
            (
                "frontend",
                scope.spawn(move || {
                    let mut log = String::new();
                    let r = lane_frontend(name, &mut log, identity);
                    (r, log)
                }),
            ),
        ];
        lanes
            .into_iter()
            .map(|(lane, handle)| match handle.join() {
                Ok((result, log)) => (lane, result, log),
                Err(_) => (
                    lane,
                    Err(anyhow::anyhow!("the lane stopped")),
                    String::new(),
                ),
            })
            .collect()
    });
    let mut failed = false;
    for (lane, result, log) in results {
        if let Err(error) = result {
            failed = true;
            warn(&format!(
                "the {lane} lane failed: {error}. Its build log follows"
            ));
            eprint!("{log}");
        } else {
            detail(&format!("the {lane} lane is built and pushed"));
        }
    }
    if failed {
        bail!("a repo image did not build. The lane log above gives the reason");
    }
    Ok(())
}
