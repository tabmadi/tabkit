//! Sets up this machine for a firewalled network, once, before any cluster command.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::buildx::{docker_config, read_config};
use super::{detail, exec, ok, step, warn};

pub(super) const PROXY_VALUES: &str = "infra/local/proxy.local.yaml";
pub(super) const PROXY_ENV: &str = "infra/local/proxy.local.env";
const DROPIN: &str = "/etc/systemd/system/docker.service.d/http-proxy.conf";
const NO_PROXY: &str = "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,fc00::/7,.svc,.svc.cluster.local,.cluster.local,127.0.0.1,::1,localhost,.localhost,.localtest.me";

struct Proxy {
    url: String,
    bypass: String,
    loopback_host: Option<std::ops::Range<usize>>,
}

impl Proxy {
    fn parse(url: &str) -> Self {
        let start = url.find("://").map_or(0, |index| index + 3);
        let authority = url[start..].split('/').next().unwrap_or_default();
        let host_start = start + authority.rfind('@').map_or(0, |index| index + 1);
        let host_port = &authority[host_start - start..];
        let host = if host_port.starts_with('[') {
            host_port
                .find(']')
                .map_or(host_port, |end| &host_port[..=end])
        } else {
            host_port.split(':').next().unwrap_or_default()
        };
        let loopback_host = (host.eq_ignore_ascii_case("localhost")
            || matches!(host, "127.0.0.1" | "[::1]"))
        .then_some(host_start..host_start + host.len());
        let mut bypass: Vec<String> = NO_PROXY.split(',').map(str::to_owned).collect();
        for name in ["NO_PROXY", "no_proxy"] {
            if let Ok(value) = std::env::var(name) {
                for entry in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if !bypass.iter().any(|existing| existing == entry) {
                        bypass.push(entry.to_owned());
                    }
                }
            }
        }
        Self {
            url: url.to_string(),
            bypass: bypass.join(","),
            loopback_host,
        }
    }

    /// The proxy as a container on a docker network sees it.
    fn via_network(&self, network: &str) -> Option<String> {
        if self.loopback_host.is_none() {
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
        Some(self.with_gateway(gateway))
    }

    fn with_gateway(&self, gateway: &str) -> String {
        let mut url = self.url.clone();
        if let Some(host) = &self.loopback_host {
            url.replace_range(host.clone(), gateway);
        }
        url
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
    ok("the daemon has an HTTPS proxy");
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
        write_private(
            PROXY_ENV,
            format!(
                "# Written by `just proxy-setup`. Machine-local, and absent on a direct network.\n# tabkit-ops loads it, so every cluster command uses this machine's egress,\n# and no cluster command names a proxy.\nHTTP_PROXY={0}\nHTTPS_PROXY={0}\nNO_PROXY={1}\nhttp_proxy={0}\nhttps_proxy={0}\nno_proxy={1}\n",
                proxy.url, proxy.bypass
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

fn write_private(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    let temporary = path.with_extension(format!("proxy-tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = file
        .write_all(contents.as_ref())
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.with_context(|| format!("cannot write {}", path.display()))
}

fn step_build(report: &mut Report, proxy: &Proxy) -> Result<()> {
    step("step 2: docker build, for RUN steps inside a build container");
    let file = docker_config();
    let Some(want) = proxy.via_network("bridge") else {
        report.gap("cannot read the docker bridge gateway; is the daemon running?");
        return Ok(());
    };
    let mut current = read_config()?;
    let desired = [
        ("httpProxy", &want),
        ("httpsProxy", &want),
        ("noProxy", &proxy.bypass),
    ];
    if desired
        .iter()
        .all(|(key, value)| current["proxies"]["default"][key].as_str() == Some(value.as_str()))
    {
        ok("the build proxy matches");
    } else if report.apply {
        let mut section = &mut current;
        for key in ["proxies", "default"] {
            section = section
                .as_object_mut()
                .context("invalid Docker CLI proxy settings")?
                .entry(key)
                .or_insert_with(|| json!({}));
        }
        let settings = section
            .as_object_mut()
            .context("invalid Docker CLI proxy settings")?;
        for (key, value) in desired {
            settings.insert(key.to_owned(), Value::String(value.clone()));
        }
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut contents = serde_json::to_vec_pretty(&current)?;
        contents.push(b'\n');
        write_private(&file, contents)?;
        ok(&format!("wrote build proxy settings to {}", file.display()));
    } else {
        report.gap("the build proxy settings differ. Run just proxy-setup");
    }
    Ok(())
}

fn step_resolver() {
    step("step 3: docker's embedded resolver");
    let base = std::env::var("CLUSTER")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "platform".to_string());
    for node in [
        format!("{base}-full-control-plane"),
        format!("{base}-control-plane"),
    ] {
        if exec::succeeds("docker", &["inspect", &node]) {
            if exec::succeeds("docker", &["exec", &node, "getent", "hosts", "github.com"]) {
                ok("the node resolves github.com");
            } else {
                detail(
                    "the node cannot resolve github.com. The proxy resolves public names for HTTP requests",
                );
            }
            return;
        }
    }
    detail("no kind node is running");
}

fn repo_values(want: &str, bypass: &str) -> String {
    let want = json!(want);
    let bypass = json!(bypass);
    format!(
        "# Written by `just proxy-setup`. Machine-local and ignored by Git.\nargo-cd:\n  repoServer:\n    env:\n      - name: HTTPS_PROXY\n        value: {want}\n      - name: HTTP_PROXY\n        value: {want}\n      - name: NO_PROXY\n        value: {bypass}\n"
    )
}

fn step_repo_server(report: &mut Report, proxy: &Proxy) -> Result<()> {
    step("step 4: ArgoCD's repo-server, for git and chart repositories, full tier only");
    let want = proxy.via_network("kind");
    match &want {
        None => report.gap("cannot read the kind network gateway"),
        Some(value) if report.apply => {
            write_private(PROXY_VALUES, repo_values(value, &proxy.bypass))?;
            ok(&format!("wrote {PROXY_VALUES}. Every cluster-up full applies it"));
        }
        Some(_) if Path::new(PROXY_VALUES).is_file() => ok(&format!("{PROXY_VALUES} present")),
        Some(_) => report.gap(&format!("no {PROXY_VALUES}. A full tier's repo-server will not reach git or the chart repositories")),
    }
    let base = std::env::var("CLUSTER")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "platform".to_string());
    let context = format!("kind-{base}-full");
    if !exec::succeeds(
        "kubectl",
        &[
            "--context",
            &context,
            "get",
            "deploy",
            "argocd-repo-server",
            "-n",
            "argocd",
        ],
    ) {
        detail("no repo-server running. The overlay applies at the next cluster-up full");
        return Ok(());
    }
    let Some(want) = want else {
        return Ok(());
    };
    let current = exec::lossy(
        "kubectl",
        &[
            "--context",
            &context,
            "-n",
            "argocd",
            "get",
            "deploy",
            "argocd-repo-server",
            "-o",
            "json",
        ],
    )
    .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
    .unwrap_or_default();
    let env = current["spec"]["template"]["spec"]["containers"][0]["env"].as_array();
    let matches = [
        ("HTTPS_PROXY", want.as_str()),
        ("HTTP_PROXY", want.as_str()),
        ("NO_PROXY", proxy.bypass.as_str()),
    ]
    .iter()
    .all(|(name, value)| {
        env.is_some_and(|env| {
            env.iter().any(|item| {
                item["name"].as_str() == Some(*name) && item["value"].as_str() == Some(*value)
            })
        })
    });
    if matches {
        ok("repo-server proxy settings match");
    } else if report.apply {
        step("applying the repo-server proxy");
        exec::output(
            "helm",
            &[
                "upgrade",
                "argocd",
                "infra/helm/platform/argocd",
                "-n",
                "argocd",
                "--kube-context",
                &context,
                "--reuse-values",
                "--timeout",
                "8m",
                "-f",
                PROXY_VALUES,
            ],
        )?;
        exec::output(
            "kubectl",
            &[
                "--context",
                &context,
                "-n",
                "argocd",
                "rollout",
                "status",
                "deploy/argocd-repo-server",
                "--timeout=180s",
            ],
        )?;
        ok("repo-server proxy settings applied");
        detail("unblock an app with: argocd app get <app> --core --hard-refresh");
    } else {
        report.gap("the repo-server proxy settings differ. Run just proxy-setup");
    }
    Ok(())
}

pub(super) fn from_env() -> Option<String> {
    ["HTTPS_PROXY", "https_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

pub fn run(args: &[String]) -> Result<()> {
    let apply = match args {
        [] => true,
        [option] if option == "--check" => false,
        _ => bail!("usage: just proxy-setup [--check]"),
    };
    let url = from_env().unwrap_or_else(docker_proxy);
    if url.is_empty() {
        ok("no proxy configured anywhere this script can see");
        detail("If your network reaches ghcr.io, github.com, and the chart repos directly,");
        detail("none of this applies. Run 'just cluster-up' as it is.");
        detail("If it does not, export HTTPS_PROXY and run this again.");
        return Ok(());
    }
    if url.chars().any(char::is_whitespace) {
        bail!("percent encode spaces and control characters in the proxy URL");
    }
    let proxy = Proxy::parse(&url);
    step("checking the configured proxy");
    detail(if proxy.loopback_host.is_some() {
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
    step_resolver();
    step_repo_server(&mut report, &proxy)?;
    if report.needs_root && report.apply {
        let dir = Path::new("infra/local");
        fs::create_dir_all(dir)?;
        let file = dir.join("proxy.local.docker.conf");
        write_private(
            &file,
            format!(
                "[Service]\nEnvironment=\"HTTP_PROXY={0}\"\nEnvironment=\"HTTPS_PROXY={0}\"\nEnvironment=\"NO_PROXY={1}\"\n",
                proxy.url.replace('%', "%%"),
                proxy.bypass.replace('%', "%%")
            ),
        )?;
        println!();
        step("step 1 needs root. The file is written for you. Paste this:");
        detail(&format!("sudo install -Dm600 {} {DROPIN}", file.display()));
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
        assert_eq!(local.with_gateway("172.18.0.1"), "http://172.18.0.1:3128");
        assert_eq!(
            Proxy::parse("https://user:password@[::1]:3128/").with_gateway("172.18.0.1"),
            "https://user:password@172.18.0.1:3128/"
        );
        assert!(
            Proxy::parse("http://proxy.corp:8080")
                .loopback_host
                .is_none()
        );
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
        let text = repo_values("http://172.18.0.1:3128", "localhost,forge.internal");
        assert_eq!(
            text.matches(r#"value: "http://172.18.0.1:3128""#).count(),
            2
        );
        assert!(text.contains(r#"value: "localhost,forge.internal""#));
    }
}
