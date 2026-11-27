//! The egress settings of this machine, per ADR-1300 of tabkit: one file, one bypass list, and the setup check.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::buildx::{docker_config, read_config};
use super::{detail, exec, ok, step, warn};

/// The egress file of this machine. It is absent on a machine with direct internet.
pub const FILE: &str = "/etc/tabkit/egress.env";
/// The drop-in that loads the file into a systemd unit.
const DROPIN: &str = "tabkit-egress.conf";
/// The base of every bypass list: loopback, the private ranges, the cluster domains, and the local test domain.
const BASE: &str = "localhost,127.0.0.1,::1,.localhost,.localtest.me,.svc,.cluster.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,fc00::/7";
const LEVELS: [&str; 4] = ["direct", "proxy", "internal", "airgap"];

/// The egress of this machine: the process environment first, then the egress file.
pub struct Egress {
    pub level: String,
    pub proxy: Option<String>,
    pub bypass: String,
    /// The registry that mirrors every upstream under a path of its host name, such as `zot:5000/docker.io`.
    pub mirror: Option<String>,
}

fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .or_else(|| v.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
                .unwrap_or(v);
            (k.trim().to_owned(), v.to_owned())
        })
        .collect()
}

fn merge(lists: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for entry in lists
        .iter()
        .flat_map(|l| l.split(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out.join(",")
}

impl Egress {
    fn from(env: &dyn Fn(&str) -> Option<String>, file: &[(String, String)]) -> Self {
        let get = |names: &[&str]| {
            names.iter().find_map(|n| env(n)).or_else(|| {
                names.iter().find_map(|n| {
                    file.iter()
                        .find(|(k, v)| k == n && !v.is_empty())
                        .map(|(_, v)| v.clone())
                })
            })
        };
        let proxy = get(&["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]);
        let site = get(&["NO_PROXY", "no_proxy"]).unwrap_or_default();
        let level = get(&["TABKIT_EGRESS"])
            .unwrap_or_else(|| if proxy.is_some() { "proxy" } else { "direct" }.to_owned());
        Self {
            level,
            proxy,
            bypass: merge(&[BASE, &site]),
            mirror: get(&["TABKIT_REGISTRY_MIRROR"]).map(|m| m.trim_end_matches('/').to_owned()),
        }
    }

    /// Reads the egress of this machine.
    pub fn load() -> Self {
        let file = parse(&fs::read_to_string(FILE).unwrap_or_default());
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self::from(&env, &file)
    }

    /// The bypass list with more entries, such as an address of this host.
    pub fn bypass_with(&self, extra: &[&str]) -> String {
        let mut lists = vec![self.bypass.as_str()];
        lists.extend(extra);
        merge(&lists)
    }

    /// The variables for a child process, in both cases.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        if let Some(proxy) = &self.proxy {
            for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                vars.push((name.to_owned(), proxy.clone()));
            }
        }
        for name in ["NO_PROXY", "no_proxy"] {
            vars.push((name.to_owned(), self.bypass.clone()));
        }
        vars
    }

    fn shell(&self) -> String {
        let quote = |v: &str| format!("'{}'", v.replace('\'', r"'\''"));
        let mut head = format!("export TABKIT_EGRESS={}\n", quote(&self.level));
        if let Some(mirror) = &self.mirror {
            head = head + "export TABKIT_REGISTRY_MIRROR=" + &quote(mirror) + "\n";
        }
        self.env().iter().fold(head, |out, (k, v)| {
            out + "export " + k + "=" + &quote(v) + "\n"
        })
    }

    /// The Helm values that give the Argo CD repo-server this egress, or none without a proxy.
    pub fn argocd_values(&self) -> Option<String> {
        let proxy = self.proxy.as_ref()?;
        let want = json!(via_network(proxy, "kind").unwrap_or_else(|| proxy.clone()));
        let bypass = json!(self.bypass);
        Some(format!(
            "argo-cd:\n  repoServer:\n    env:\n      - name: HTTPS_PROXY\n        value: {want}\n      - name: HTTP_PROXY\n        value: {want}\n      - name: NO_PROXY\n        value: {bypass}\n"
        ))
    }
}

/// The full name of an image, with its registry. A name with no registry is on Docker Hub.
pub fn full_name(reference: &str) -> String {
    let first = reference.split('/').next().unwrap_or_default();
    let has_registry = reference.contains('/')
        && (first.contains('.') || first.contains(':') || first == "localhost");
    if has_registry {
        reference.to_owned()
    } else if reference.contains('/') {
        format!("docker.io/{reference}")
    } else {
        format!("docker.io/library/{reference}")
    }
}

/// The range of the host in a proxy URL when the host is a loopback name.
fn loopback_host(url: &str) -> Option<std::ops::Range<usize>> {
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
    (host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "[::1]"))
        .then_some(host_start..host_start + host.len())
}

fn with_gateway(url: &str, gateway: &str) -> String {
    let mut url = url.to_owned();
    if let Some(host) = loopback_host(&url) {
        url.replace_range(host, gateway);
    }
    url
}

/// The proxy as a container on a Docker network sees it.
fn via_network(url: &str, network: &str) -> Option<String> {
    if loopback_host(url).is_none() {
        return Some(url.to_owned());
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
    Some(with_gateway(url, gateway))
}

struct Report {
    apply: bool,
    gaps: u32,
    install: Vec<(PathBuf, &'static str)>,
}

impl Report {
    fn gap(&mut self, message: &str) {
        self.gaps += 1;
        warn(message);
    }
}

fn write_private(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
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

/// A folder outside the repository for the files that a person installs as root.
fn staging() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"),
            PathBuf::from,
        )
        .join("tabkit")
}

fn step_file(report: &mut Report, egress: &Egress) -> Result<()> {
    step("step 1: the egress file of this machine");
    if Path::new(FILE).is_file() {
        ok(&format!("{FILE} is present, level {}", egress.level));
        return Ok(());
    }
    report.gap(&format!(
        "no {FILE}. The daemons do not see the egress of the shell"
    ));
    if report.apply {
        let file = staging().join("egress.env");
        write_private(&file, egress.shell().replace("export ", ""))?;
        report.install.push((file, FILE));
    }
    Ok(())
}

fn unit_loads_file(unit: &str) -> bool {
    exec::lossy("systemctl", &["show", unit, "-p", "EnvironmentFiles"])
        .is_some_and(|out| out.contains(FILE))
}

fn step_daemons(report: &mut Report) -> Result<()> {
    step("step 2: the Nix daemon and Docker load the egress file");
    for unit in ["nix-daemon", "docker"] {
        if !exec::succeeds("systemctl", &["cat", &format!("{unit}.service")]) {
            detail(&format!("{unit}.service is not installed"));
        } else if unit_loads_file(unit) {
            ok(&format!("{unit} loads {FILE}"));
        } else {
            report.gap(&format!("{unit} does not load {FILE}"));
            if report.apply {
                let file = staging().join(format!("{unit}-{DROPIN}"));
                write_private(&file, format!("[Service]\nEnvironmentFile=-{FILE}\n"))?;
                report.install.push((
                    file,
                    if unit == "docker" {
                        "/etc/systemd/system/docker.service.d/tabkit-egress.conf"
                    } else {
                        "/etc/systemd/system/nix-daemon.service.d/tabkit-egress.conf"
                    },
                ));
            }
        }
    }
    Ok(())
}

fn step_build(report: &mut Report, egress: &Egress) -> Result<()> {
    step("step 3: docker build, for RUN steps inside a build container");
    let Some(proxy) = &egress.proxy else {
        ok("no proxy, so a build step needs no settings");
        return Ok(());
    };
    let file = docker_config();
    let Some(want) = via_network(proxy, "bridge") else {
        report.gap("cannot read the docker bridge gateway. Is the daemon running");
        return Ok(());
    };
    let mut current = read_config()?;
    let desired = [
        ("httpProxy", &want),
        ("httpsProxy", &want),
        ("noProxy", &egress.bypass),
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
        let mut contents = serde_json::to_vec_pretty(&current)?;
        contents.push(b'\n');
        write_private(&file, contents)?;
        ok(&format!("wrote build proxy settings to {}", file.display()));
    } else {
        report.gap("the build proxy settings differ. Run just egress-setup");
    }
    Ok(())
}

fn step_substituters(report: &mut Report, egress: &Egress) {
    step("step 4: the Nix substituters");
    if !matches!(egress.level.as_str(), "internal" | "airgap") {
        ok(&format!(
            "level {} may read the public caches",
            egress.level
        ));
        return;
    }
    let substituters = exec::lossy(
        "nix",
        &[
            "--extra-experimental-features",
            "nix-command",
            "config",
            "show",
            "substituters",
        ],
    )
    .unwrap_or_default();
    if substituters.contains("cache.nixos.org") {
        report.gap(&format!(
            "level {} reads cache.nixos.org. Set only the internal caches in /etc/nix/nix.conf",
            egress.level
        ));
    } else {
        ok("only internal caches");
    }
}

fn setup(args: &[String]) -> Result<()> {
    let apply = match args {
        [] => true,
        [option] if option == "--check" => false,
        _ => bail!("usage: just egress-setup [--check]"),
    };
    let egress = Egress::load();
    if !LEVELS.contains(&egress.level.as_str()) {
        bail!(
            "TABKIT_EGRESS is '{}'. Use one of {}",
            egress.level,
            LEVELS.join(", ")
        );
    }
    if egress.level == "proxy" && egress.proxy.is_none() {
        bail!("level proxy needs HTTPS_PROXY");
    }
    if egress
        .proxy
        .as_ref()
        .is_some_and(|p| p.chars().any(char::is_whitespace))
    {
        bail!("percent encode spaces and control characters in the proxy URL");
    }
    if egress.level == "direct" && !Path::new(FILE).is_file() {
        ok("direct egress, and no egress file. Nothing to set up");
        return Ok(());
    }
    let mut report = Report {
        apply,
        gaps: 0,
        install: Vec::new(),
    };
    step_file(&mut report, &egress)?;
    step_daemons(&mut report)?;
    step_build(&mut report, &egress)?;
    step_substituters(&mut report, &egress);
    if !report.install.is_empty() {
        println!();
        step("these files need root. Install them with:");
        for (from, to) in &report.install {
            detail(&format!("sudo install -Dm644 {} {to}", from.display()));
        }
        detail("sudo systemctl daemon-reload && sudo systemctl restart nix-daemon docker");
        detail("then run again: just egress-setup --check");
    }
    println!();
    if report.gaps > 0 {
        bail!(
            "{} gap(s) above. Details are in docs/egress.md",
            report.gaps
        );
    }
    ok("egress setup complete");
    Ok(())
}

/// `tabkit-ops egress env` prints the shell exports, and `tabkit-ops egress setup` checks this machine.
pub fn run(args: &[String]) -> Result<()> {
    match args.split_first() {
        Some((verb, [])) if verb == "env" => {
            print!("{}", Egress::load().shell());
            Ok(())
        }
        Some((verb, rest)) if verb == "setup" => setup(rest),
        _ => bail!("usage: tabkit-ops egress env | setup [--check]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn egress(env: &[(&str, &str)], file: &str) -> Egress {
        let env = |name: &str| {
            env.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        };
        Egress::from(&env, &parse(file))
    }

    #[test]
    fn the_file_holds_the_settings() {
        let e = egress(
            &[],
            "# site\nTABKIT_EGRESS=proxy\nHTTPS_PROXY='http://proxy.corp:3128'\nNO_PROXY=forge.internal,localhost\n",
        );
        assert_eq!(e.level, "proxy");
        assert_eq!(e.proxy.as_deref(), Some("http://proxy.corp:3128"));
        assert!(e.bypass.starts_with("localhost,127.0.0.1"));
        assert!(e.bypass.ends_with(",forge.internal"));
        assert_eq!(e.bypass.split(',').filter(|x| *x == "localhost").count(), 1);
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let e = egress(
            &[("HTTPS_PROXY", "http://other:8080")],
            "HTTPS_PROXY=http://proxy.corp:3128\n",
        );
        assert_eq!(e.proxy.as_deref(), Some("http://other:8080"));
        assert_eq!(e.level, "proxy");
    }

    #[test]
    fn no_settings_is_direct() {
        let e = egress(&[], "");
        assert_eq!(e.level, "direct");
        assert!(e.proxy.is_none());
        assert_eq!(e.bypass, BASE);
        assert!(e.argocd_values().is_none());
    }

    #[test]
    fn the_shell_exports_quote_each_value() {
        let shell = egress(&[], "HTTPS_PROXY=http://u:p'w@proxy:3128\n").shell();
        assert!(shell.contains(r"export HTTPS_PROXY='http://u:p'\''w@proxy:3128'"));
        assert!(shell.contains("export no_proxy='localhost,"));
    }

    #[test]
    fn knows_a_loopback_proxy() {
        assert_eq!(
            with_gateway("http://127.0.0.1:3128", "172.18.0.1"),
            "http://172.18.0.1:3128"
        );
        assert_eq!(
            with_gateway("https://user:password@[::1]:3128/", "172.18.0.1"),
            "https://user:password@172.18.0.1:3128/"
        );
        assert!(loopback_host("http://proxy.corp:8080").is_none());
    }

    #[test]
    fn the_repo_values_hold_the_proxy() {
        let text = egress(
            &[],
            "HTTPS_PROXY=http://proxy.corp:3128\nNO_PROXY=forge.internal\n",
        )
        .argocd_values()
        .unwrap_or_default();
        assert_eq!(
            text.matches(r#"value: "http://proxy.corp:3128""#).count(),
            2
        );
        assert!(text.contains(",forge.internal\""));
    }
}
