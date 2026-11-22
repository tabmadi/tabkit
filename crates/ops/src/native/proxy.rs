//! Sets up this machine for a firewalled network, once, before any cluster command.

use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use super::{detail, exec, ok, step, warn};

const PROXY_VALUES: &str = "infra/local/proxy.local.yaml";
const PROXY_ENV: &str = "infra/local/proxy.local.env";
const DROPIN: &str = "/etc/systemd/system/docker.service.d/http-proxy.conf";
const NO_PROXY: &str = "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,fc00::/7,.svc,.svc.cluster.local,.cluster.local,127.0.0.1,localhost,.localtest.me";

struct Proxy {
    url: String,
    port: String,
    loopback: bool,
}

impl Proxy {
    fn parse(url: &str) -> Self {
        let hostport = url.split_once("://").map_or(url, |(_, rest)| rest);
        let port = hostport.rsplit(':').next().unwrap_or_default().to_string();
        let host = hostport.split(':').next().unwrap_or_default();
        Self {
            url: url.to_string(),
            port,
            loopback: matches!(host, "127.0.0.1" | "localhost") || hostport.starts_with("::1"),
        }
    }

    /// The proxy as a container on a docker network sees it.
    fn via_network(&self, network: &str) -> Option<String> {
        if !self.loopback {
            return Some(self.url.clone());
        }
        let raw = exec::lossy(
            "docker",
            &[
                "network",
                "inspect",
                network,
                "-f",
                "{{range .IPAM.Config}}{{.Gateway}} {{end}}",
            ],
        )?;
        let gateway = raw
            .split_whitespace()
            .find(|g| g.chars().next().is_some_and(|c| c.is_ascii_digit()) && g.contains('.'))?;
        Some(format!("http://{gateway}:{}", self.port))
    }
}

struct Report {
    apply: bool,
    gaps: u32,
    needs_root: bool,
}

impl Report {
    fn gap(&mut self, message: &str) {
        self.gaps += 1;
        warn(message);
    }
}

fn docker_proxy() -> String {
    exec::lossy("docker", &["info", "--format", "{{.HTTPSProxy}}"])
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn step_daemon(report: &mut Report) {
    step("step 1: the docker daemon, for image pulls and zot's own egress");
    if !exec::succeeds("docker", &["version", "--format", "x"])
        && exec::lossy("docker", &["--version"]).is_none()
    {
        report.gap("docker not found; steps 1-4 cannot be checked");
        return;
    }
    let daemon = docker_proxy();
    if daemon.is_empty() {
        report.gap("the daemon has no HTTPS proxy. Image pulls go direct and hang");
        report.needs_root = true;
        return;
    }
    ok(&format!("daemon proxied at {daemon}"));
    let has_proxies = fs::read_to_string("/etc/docker/daemon.json")
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v.get("proxies").is_some());
    if has_proxies && Path::new(DROPIN).is_file() {
        detail(&format!(
            "both /etc/docker/daemon.json and {DROPIN} set one. daemon.json is what you see above"
        ));
    }
}

fn step_env(report: &mut Report, proxy: &Proxy) -> Result<()> {
    step("step 1b: the environment that cluster-up runs kind in");
    if report.apply {
        if let Some(parent) = Path::new(PROXY_ENV).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            PROXY_ENV,
            format!(
                "# Written by `just proxy-setup`. Machine-local, and absent on a direct network.\n# tabkit-ops loads it, so every cluster command uses this machine's egress,\n# and no cluster command names a proxy.\nHTTP_PROXY={0}\nHTTPS_PROXY={0}\nNO_PROXY={NO_PROXY}\n",
                proxy.url
            ),
        )?;
        ok(&format!("wrote {PROXY_ENV}. cluster-up loads it"));
    } else if Path::new(PROXY_ENV).is_file() {
        ok(&format!("{PROXY_ENV} present"));
    } else {
        report.gap(&format!(
            "no {PROXY_ENV}. 'kind create' will stop on an EOF from the node's own API call"
        ));
    }
    Ok(())
}

fn step_build(report: &mut Report, proxy: &Proxy) -> Result<()> {
    step("step 2: docker build, for RUN steps inside a build container");
    let dir = std::env::var("DOCKER_CONFIG")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.docker", std::env::var("HOME").unwrap_or_default()));
    let file = Path::new(&dir).join("config.json");
    let Some(want) = proxy.via_network("bridge") else {
        report.gap("cannot read the docker bridge gateway; is the daemon running?");
        return Ok(());
    };
    let current: serde_json::Value = fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    let have = current["proxies"]["default"]["httpsProxy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if have == want {
        ok(&format!("build proxy set to {want}"));
    } else if report.apply {
        fs::create_dir_all(&dir)?;
        if !file.is_file() {
            fs::write(&file, "{}\n")?;
        }
        // jq keeps the order of the keys in the Docker config of the engineer.
        let body =
            json!({ "httpProxy": want, "httpsProxy": want, "noProxy": NO_PROXY }).to_string();
        let new = exec::output(
            "jq",
            &[
                "--argjson",
                "p",
                &body,
                ".proxies.default = $p",
                &file.to_string_lossy(),
            ],
        )?;
        fs::write(&file, new)?;
        ok(&format!(
            "wrote proxies.default = {want} to {}",
            file.display()
        ));
    } else {
        let shown = if have.is_empty() { "<unset>" } else { &have };
        report.gap(&format!("build proxy is '{shown}', should be {want}"));
    }
    Ok(())
}

fn step_resolver(report: &mut Report) {
    step("step 3: docker's embedded resolver");
    let base = std::env::var("CLUSTER")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "platform".to_string());
    let node = format!("{base}-control-plane");
    if !exec::succeeds("docker", &["inspect", &node]) {
        detail(&format!(
            "no '{node}' container. Nothing to check until a cluster exists"
        ));
    } else if exec::succeeds("docker", &["exec", &node, "getent", "hosts", "github.com"]) {
        ok("the node resolves github.com");
    } else {
        report.gap(
            "the node cannot resolve github.com while the host can. The daemon resolver is stale",
        );
        detail("sudo systemctl restart docker");
    }
}

fn repo_values(want: &str) -> String {
    format!(
        "# Written by `just proxy-setup`. Machine-local: the address is this host's\n# docker gateway. Not committed, and absent on a direct network.\nargo-cd:\n  repoServer:\n    env:\n      - name: HTTPS_PROXY\n        value: {want}\n      - name: HTTP_PROXY\n        value: {want}\n      - name: NO_PROXY\n        value: {NO_PROXY}\n"
    )
}

fn step_repo_server(report: &mut Report, proxy: &Proxy) -> Result<()> {
    step("step 4: ArgoCD's repo-server, for git and chart repositories, full tier only");
    let want = proxy.via_network("kind");
    match &want {
        None => report.gap("cannot read the kind network gateway"),
        Some(value) if report.apply => {
            fs::write(PROXY_VALUES, repo_values(value))?;
            ok(&format!("wrote {PROXY_VALUES}. Every cluster-up full applies it"));
        }
        Some(_) if Path::new(PROXY_VALUES).is_file() => ok(&format!("{PROXY_VALUES} present")),
        Some(_) => report.gap(&format!("no {PROXY_VALUES}. A full tier's repo-server will not reach git or the chart repositories")),
    }
    if !exec::succeeds(
        "kubectl",
        &["get", "deploy", "argocd-repo-server", "-n", "argocd"],
    ) {
        detail("no repo-server running. The overlay applies at the next cluster-up full");
        return Ok(());
    }
    let Some(want) = want else {
        return Ok(());
    };
    let have = exec::lossy("kubectl", &["-n", "argocd", "get", "deploy", "argocd-repo-server", "-o", r#"jsonpath={range .spec.template.spec.containers[0].env[?(@.name=="HTTPS_PROXY")]}{.value}{end}"#]).unwrap_or_default();
    if have == want {
        ok(&format!("repo-server proxied at {want}"));
    } else if report.apply {
        step("applying the repo-server proxy");
        let escaped = NO_PROXY.replace(',', "\\,");
        exec::output(
            "helm",
            &[
                "upgrade",
                "argocd",
                "infra/helm/platform/argocd",
                "-n",
                "argocd",
                "--reuse-values",
                "--timeout",
                "8m",
                "--set",
                "argo-cd.repoServer.env[0].name=HTTPS_PROXY",
                "--set",
                &format!("argo-cd.repoServer.env[0].value={want}"),
                "--set",
                "argo-cd.repoServer.env[1].name=HTTP_PROXY",
                "--set",
                &format!("argo-cd.repoServer.env[1].value={want}"),
                "--set",
                "argo-cd.repoServer.env[2].name=NO_PROXY",
                "--set-string",
                &format!("argo-cd.repoServer.env[2].value={escaped}"),
            ],
        )?;
        exec::output(
            "kubectl",
            &[
                "-n",
                "argocd",
                "rollout",
                "status",
                "deploy/argocd-repo-server",
                "--timeout=180s",
            ],
        )?;
        ok(&format!("repo-server proxied at {want}"));
        detail("unblock an app with: argocd app get <app> --core --hard-refresh");
    } else {
        let shown = if have.is_empty() { "no proxy" } else { &have };
        report.gap(&format!("repo-server has {shown}, should be {want}"));
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let apply = args.first().is_none_or(|a| a != "--check");
    let from_env = ["HTTPS_PROXY", "https_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()));
    let url = from_env.unwrap_or_else(docker_proxy);
    if url.is_empty() {
        ok("no proxy configured anywhere this script can see");
        detail("If your network reaches ghcr.io, github.com, and the chart repos directly,");
        detail("none of this applies. Run 'just cluster-up' as it is.");
        detail("If it does not, export HTTPS_PROXY and run this again.");
        return Ok(());
    }
    let proxy = Proxy::parse(&url);
    step(&format!("proxy: {url}"));
    detail(if proxy.loopback {
        "loopback: each layer below needs a different address for the same proxy"
    } else {
        "routable: the same address works from every layer"
    });
    let mut report = Report {
        apply,
        gaps: 0,
        needs_root: false,
    };
    step_daemon(&mut report);
    step_env(&mut report, &proxy)?;
    step_build(&mut report, &proxy)?;
    step_resolver(&mut report);
    step_repo_server(&mut report, &proxy)?;
    if report.needs_root {
        let dir = std::env::temp_dir().join(format!("tabkit-proxy-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let file = dir.join("http-proxy.conf");
        fs::write(
            &file,
            format!(
                "[Service]\nEnvironment=\"HTTP_PROXY={0}\"\nEnvironment=\"HTTPS_PROXY={0}\"\nEnvironment=\"NO_PROXY={NO_PROXY}\"\n",
                proxy.url
            ),
        )?;
        println!();
        step("step 1 needs root. The file is written for you. Paste this:");
        detail(&format!("sudo install -Dm644 {} {DROPIN}", file.display()));
        detail("sudo systemctl daemon-reload && sudo systemctl restart docker");
        detail("then run again: just proxy-setup");
    }
    println!();
    if report.gaps > 0 {
        bail!(
            "{} gap(s) above. Details are in docs/guide/http-proxy.md",
            report.gaps
        );
    }
    ok("proxy setup complete. 'just cluster-up' works unchanged from here");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_a_loopback_proxy() {
        let local = Proxy::parse("http://127.0.0.1:3128");
        assert!(local.loopback);
        assert_eq!(local.port, "3128");
        assert!(!Proxy::parse("http://proxy.corp:8080").loopback);
    }

    #[test]
    fn a_routable_proxy_is_the_same_from_every_network() {
        assert_eq!(
            Proxy::parse("http://proxy.corp:8080")
                .via_network("kind")
                .as_deref(),
            Some("http://proxy.corp:8080")
        );
    }

    #[test]
    fn the_repo_values_hold_the_proxy() {
        let text = repo_values("http://172.18.0.1:3128");
        assert_eq!(text.matches("value: http://172.18.0.1:3128").count(), 2);
    }
}
