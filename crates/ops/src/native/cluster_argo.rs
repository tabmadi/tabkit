//! Argo CD on the full tier: its credential, the root application, and the wait for convergence.

use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::cluster::Cluster;
use super::files::TempDir;
use super::{exec, ok, step, values, warn};

fn host_of(url: &str) -> Option<&str> {
    url.strip_prefix("https://")?.split('/').next()
}

fn credential_from_git(host: &str) -> (String, String) {
    let Ok(mut child) = Command::new("git")
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Default::default();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = write!(stdin, "protocol=https\nhost={host}\n\n");
    }
    let Ok(out) = child.wait_with_output() else {
        return Default::default();
    };
    if !out.status.success() {
        return Default::default();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_default()
            .to_string()
    };
    (field("username="), field("password="))
}

pub fn stage_repo_creds(cluster: &Cluster) -> Result<()> {
    let root = fs::read_to_string("infra/gitops/local-bootstrap/root-application.yaml")?;
    let url = values::get(&root, ".spec.source.repoURL")?;
    let host = host_of(&url).unwrap_or(&url).to_string();
    let mut user = std::env::var("ARGOCD_REPO_USERNAME").unwrap_or_default();
    let mut password = std::env::var("ARGOCD_REPO_PASSWORD").unwrap_or_default();
    if password.is_empty() {
        (user, password) = credential_from_git(&host);
    }
    if password.is_empty() {
        return Ok(());
    }
    step(&format!("giving Argo CD a credential for https://{host}/"));
    let username = if user.is_empty() {
        "git".to_string()
    } else {
        user
    };
    // A manifest on standard input keeps the password out of the process list.
    let manifest = serde_json::json!({
        "apiVersion": "v1", "kind": "Secret",
        "metadata": { "name": "forge-creds", "namespace": "argocd", "labels": { "argocd.argoproj.io/secret-type": "repo-creds" } },
        "type": "Opaque",
        "stringData": { "type": "git", "url": format!("https://{host}/"), "username": username, "password": password }
    });
    cluster.apply(None, &manifest.to_string())
}

/// The argocd CLI in core mode, on a private kubeconfig, so the context of the engineer stays as it is.
struct Core {
    kubeconfig: String,
    _work: TempDir,
}

impl Core {
    fn new(cluster: &Cluster) -> Result<Self> {
        let work = TempDir::new("argo")?;
        let file = work.0.join("kubeconfig");
        fs::write(
            &file,
            cluster.kout(&["config", "view", "--minify", "--flatten"])?,
        )?;
        let kubeconfig = file.to_string_lossy().into_owned();
        exec::output(
            "kubectl",
            &[
                "--kubeconfig",
                &kubeconfig,
                "config",
                "set-context",
                "--current",
                "--namespace",
                "argocd",
            ],
        )?;
        Ok(Self {
            kubeconfig,
            _work: work,
        })
    }

    fn status(&self, args: &[&str]) -> bool {
        let mut all = vec!["--core"];
        all.extend(args);
        Command::new("argocd")
            .args(all)
            .env("KUBECONFIG", &self.kubeconfig)
            .status()
            .is_ok_and(|s| s.success())
    }

    fn output(&self, args: &[&str]) -> Result<String> {
        let mut all = vec!["--core"];
        all.extend(args);
        exec::output_env("argocd", &all, &[("KUBECONFIG", &self.kubeconfig)])
    }

    fn names(&self) -> Result<Vec<String>> {
        Ok(self
            .output(&["app", "list", "-o", "name"])?
            .lines()
            .map(str::to_string)
            .collect())
    }
}

fn op_ended(cluster: &Cluster, app: &str) -> bool {
    cluster
        .kout(&[
            "-n",
            "argocd",
            "get",
            "application",
            app,
            "-o",
            "jsonpath={.status.operationState.phase}",
        ])
        .is_ok_and(|phase| phase.trim() != "Running" && phase.trim() != "Terminating")
}

fn dump_unhealthy(cluster: &Cluster, core: &Core) {
    warn("applications that did not reach Synced and Healthy:");
    if let Ok(list) = core.output(&["app", "list", "-o", "wide"]) {
        for (index, line) in list.lines().enumerate() {
            let words: Vec<&str> = line.split_whitespace().collect();
            if index == 0 || words.get(1) != Some(&"Synced") || words.get(2) != Some(&"Healthy") {
                eprintln!("    {line}");
            }
        }
    }
    let namespaces = cluster
        .kout(&["get", "ns", "-o", "name"])
        .unwrap_or_default();
    for ns in namespaces
        .lines()
        .map(|l| l.trim_start_matches("namespace/"))
    {
        let pods = cluster
            .kout(&["-n", ns, "get", "pods", "--no-headers"])
            .unwrap_or_default();
        let bad: Vec<&str> = pods
            .lines()
            .filter(|l| !matches!(l.split_whitespace().nth(2), Some("Running" | "Completed")))
            .collect();
        if bad.is_empty() {
            continue;
        }
        warn(&format!("  namespace {ns}:"));
        for line in bad {
            eprintln!("      {line}");
        }
        let events = cluster
            .kout(&["-n", ns, "get", "events", "--sort-by=.lastTimestamp"])
            .unwrap_or_default();
        let matches: Vec<&str> = events
            .lines()
            .filter(|l| {
                ["warn", "fail", "error", "back-off"]
                    .iter()
                    .any(|w| l.to_lowercase().contains(w))
            })
            .collect();
        for line in &matches[matches.len().saturating_sub(8)..] {
            eprintln!("      {line}");
        }
    }
}

fn wait_apps(cluster: &Cluster, core: &Core, timeout: u32, apps: &[String]) -> Result<()> {
    let text = timeout.to_string();
    let wait = |budget: &str| {
        let mut args = vec!["app", "wait"];
        args.extend(apps.iter().map(String::as_str));
        args.extend(["--sync", "--health", "--operation", "--timeout", budget]);
        core.status(&args)
    };
    if wait(&text) {
        return Ok(());
    }
    warn(&format!(
        "[{}] did not converge in {timeout}s. Terminating their operations, which are probably stale from a cluster-stop, and syncing again",
        apps.join(" ")
    ));
    for app in apps {
        let _ = core.status(&["app", "terminate-op", app]);
    }
    for app in apps {
        cluster.wait_for(&format!("{app}'s operation to end"), 120, || {
            op_ended(cluster, app)
        })?;
        let _ = core.status(&["app", "sync", app, "--timeout", &text]);
    }
    if wait(&(timeout / 2).to_string()) {
        return Ok(());
    }
    dump_unhealthy(cluster, core);
    bail!(
        "ArgoCD did not converge. The applications and pod events above give the reason. `kubectl --context {} -n <ns> describe pod <pod>` gives the rest.",
        cluster.ctx()
    );
}

/// An operation that began before this `up` is a sync that `cluster-stop` froze.
fn stale(line: &str, started: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let (name, phase, began) = (words.next()?, words.next()?, words.next()?);
    (phase == "Running" && began < started).then(|| name.to_string())
}

fn restart_stale_ops(cluster: &Cluster, core: &Core, started: &str) -> Result<()> {
    let list = cluster.kout(&["-n", "argocd", "get", "applications", "-o", r#"jsonpath={range .items[*]}{.metadata.name} {.status.operationState.phase} {.status.operationState.startedAt}{"\n"}{end}"#])?;
    for name in list.lines().filter_map(|l| stale(l, started)) {
        warn(&format!(
            "{name}: its sync began before this cluster-up. Terminating it and syncing again"
        ));
        let _ = core.status(&["app", "terminate-op", &name]);
        cluster.wait_for(&format!("{name}'s operation to end"), 120, || {
            op_ended(cluster, &name)
        })?;
        core.output(&["app", "sync", &name, "--async"])
            .context("cannot sync again")?;
    }
    Ok(())
}

pub fn stage_rootapp(cluster: &Cluster, started: &str) -> Result<()> {
    step("applying the local root application");
    cluster.kq(&[
        "apply",
        "-f",
        "infra/gitops/local-bootstrap/root-application.yaml",
    ])?;
    let core = Core::new(cluster)?;
    step("waiting for ArgoCD to converge. The first run is slow");
    restart_stale_ops(cluster, &core, started)?;
    wait_apps(cluster, &core, 1800, &["local-root".to_string()])?;
    loop {
        let apps = core.names()?;
        wait_apps(cluster, &core, 900, &apps)?;
        if core.names()? == apps {
            break;
        }
    }
    ok("all ArgoCD applications Synced and Healthy");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_older_running_sync_is_stale() {
        let started = "2026-11-22T10:00:00Z";
        assert_eq!(
            stale("app Running 2026-11-22T09:00:00Z", started).as_deref(),
            Some("app")
        );
        assert_eq!(stale("app Running 2026-11-22T11:00:00Z", started), None);
        assert_eq!(stale("app Succeeded 2026-11-22T09:00:00Z", started), None);
        assert_eq!(stale("app", started), None);
    }

    #[test]
    fn reads_the_host_of_a_forge_url() {
        assert_eq!(host_of("https://github.com/acme/app"), Some("github.com"));
        assert_eq!(host_of("git://x/y"), None);
    }
}
