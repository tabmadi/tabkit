//! The local cluster: its identity, its stages, and its verbs.

use std::fs;
use std::io::Write as _;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::files::TempDir;
use super::{deploy, detail, exec, ok, secrets, step, warn};

pub const REGISTRY: &str = "registry.localhost";
const KIND_CONFIG: &str = "infra/local/kind.yaml";
const PROXY_ENV: &str = "infra/local/proxy.local.env";
const MARKER: &str = "TABKIT_CLUSTER_ENV";
const PRELUDE: [&str; 8] = [
    "registry",
    "warm",
    "cluster",
    "cni",
    "coredns",
    "namespaces",
    "edge",
    "glue",
];
const STAGES_BASE: [&str; 7] = [
    "postgres",
    "certs",
    "priority",
    "middlewares",
    "secrets",
    "ory",
    "identities",
];
const STAGES_FULL: [&str; 7] = [
    "kyverno",
    "sopskey",
    "images",
    "argocd",
    "rootapp",
    "populatezot",
    "identities",
];

/// The platform charts that the base tier installs. The image allow-list reads this set.
pub const BASE_CHARTS: [&str; 6] = [
    "cert-manager",
    "cilium",
    "namespaces",
    "ory",
    "resource-governance",
    "traefik",
];

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub fn sleep(seconds: u64) {
    std::thread::sleep(Duration::from_secs(seconds));
}

/// The `KEY=value` lines of the proxy file of this machine.
fn proxy_env() -> Vec<(String, String)> {
    fs::read_to_string(PROXY_ENV)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn valid_tier(tier: &str) -> Result<()> {
    if tier != "base" && tier != "full" {
        bail!("'{tier}' is not a tier. Use \"base\" or \"full\"");
    }
    Ok(())
}

fn tier_name(base: &str, tier: &str) -> String {
    if tier == "full" {
        format!("{base}-full")
    } else {
        base.to_string()
    }
}

fn clusters() -> Vec<String> {
    exec::lossy("kind", &["get", "clusters"])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn node_running(name: &str) -> bool {
    exec::lossy(
        "docker",
        &[
            "inspect",
            "-f",
            "{{.State.Running}}",
            &format!("{name}-control-plane"),
        ],
    )
    .is_some_and(|o| o.trim() == "true")
}

/// The tier that is up. A running tier wins over a created one, and an unclear answer is `None`.
fn pick_tier(present: &[&'static str], running: &[&'static str]) -> Option<&'static str> {
    match (running, present) {
        ([one], _) | ([], [one]) => Some(one),
        _ => None,
    }
}

fn detect_tier(base: &str) -> Option<&'static str> {
    let names = clusters();
    let (mut present, mut running) = (Vec::new(), Vec::new());
    for tier in ["base", "full"] {
        let name = tier_name(base, tier);
        if names.contains(&name) {
            present.push(tier);
            if node_running(&name) {
                running.push(tier);
            }
        }
    }
    pick_tier(&present, &running)
}

/// Starts this process again once with the proxy of the machine and the tier, so every child tool inherits them.
fn reexec_with(tier: &str) -> Result<()> {
    if variable(MARKER).is_some() {
        return Ok(());
    }
    let error = Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .envs(proxy_env())
        .env("TIER", tier)
        .env(MARKER, "1")
        .exec();
    bail!("cannot start again: {error}");
}

/// The cluster that a command acts on.
#[derive(Clone)]
pub struct Cluster {
    pub base: String,
    pub tier: String,
    pub ns: String,
    pub domain: String,
    force: Vec<String>,
    up_started: String,
}

impl Cluster {
    /// A command that takes the tier from the environment, and then from the cluster that is up.
    pub fn detect() -> Result<Self> {
        let base = variable("CLUSTER").unwrap_or_else(|| "platform".to_string());
        let tier = match variable("TIER") {
            Some(tier) => tier,
            None => detect_tier(&base).unwrap_or("base").to_string(),
        };
        Self::with_tier(&tier)
    }

    pub fn with_tier(tier: &str) -> Result<Self> {
        valid_tier(tier)?;
        reexec_with(tier)?;
        Ok(Self {
            base: variable("CLUSTER").unwrap_or_else(|| "platform".to_string()),
            tier: tier.to_string(),
            ns: variable("NS").unwrap_or_else(|| "platform".to_string()),
            domain: variable("DOMAIN").unwrap_or_else(|| "dev.localtest.me".to_string()),
            force: variable("FORCE")
                .map(|f| f.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            up_started: exec::output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])?
                .trim()
                .to_string(),
        })
    }

    pub fn name(&self) -> String {
        tier_name(&self.base, &self.tier)
    }

    fn name_of(&self, tier: &str) -> String {
        tier_name(&self.base, tier)
    }

    pub fn ctx(&self) -> String {
        format!("kind-{}", self.name())
    }

    fn other_tier(&self) -> &'static str {
        if self.tier == "full" { "base" } else { "full" }
    }

    pub fn up_hint(&self) -> &'static str {
        if self.tier == "full" {
            "just cluster-up full"
        } else {
            "just cluster-up"
        }
    }

    fn forced(&self, stage: &str) -> bool {
        self.force.iter().any(|f| f == stage)
    }

    /// kubectl with its output on the terminal.
    pub fn k(&self, args: &[&str]) -> Result<()> {
        let ctx = self.ctx();
        exec::status("kubectl", &with_context(&ctx, args), &[])
    }

    /// kubectl with its standard output dropped.
    pub fn kq(&self, args: &[&str]) -> Result<()> {
        self.kout(args).map(|_| ())
    }

    pub fn kout(&self, args: &[&str]) -> Result<String> {
        let ctx = self.ctx();
        exec::output("kubectl", &with_context(&ctx, args))
    }

    pub fn kok(&self, args: &[&str]) -> bool {
        let ctx = self.ctx();
        exec::succeeds("kubectl", &with_context(&ctx, args))
    }

    /// `kubectl apply -f -` with this manifest.
    pub fn apply(&self, namespace: Option<&str>, manifest: &str) -> Result<()> {
        let ctx = self.ctx();
        let mut args = vec!["--context", ctx.as_str()];
        if let Some(namespace) = namespace {
            args.extend(["-n", namespace]);
        }
        args.extend(["apply", "-f", "-"]);
        pipe_in("kubectl", &args, manifest)
    }

    /// helm with its output on the terminal.
    pub fn h(&self, args: &[&str]) -> Result<()> {
        let ctx = self.ctx();
        let mut all = vec!["--kube-context", ctx.as_str()];
        all.extend(args);
        exec::status("helm", &all, &[])
    }

    fn hok(&self, args: &[&str]) -> bool {
        let ctx = self.ctx();
        let mut all = vec!["--kube-context", ctx.as_str()];
        all.extend(args);
        exec::succeeds("helm", &all)
    }

    fn hout(&self, args: &[&str]) -> Result<String> {
        let ctx = self.ctx();
        let mut all = vec!["--kube-context", ctx.as_str()];
        all.extend(args);
        exec::output("helm", &all)
    }

    pub fn exists(&self, name: &str) -> bool {
        clusters().iter().any(|c| c == name)
    }

    pub fn require(&self) -> Result<()> {
        require_tools(&["kubectl"])?;
        if !self.kok(&["cluster-info"]) {
            bail!(
                "cluster {} is not reachable. Run '{}' first",
                self.ctx(),
                self.up_hint()
            );
        }
        Ok(())
    }

    /// The Argo application of a service. The ApplicationSet names it with or without the file extension.
    pub fn argo_service_app(&self, service: &str) -> Option<String> {
        [
            format!("local-service-{service}"),
            format!("local-service-{service}.yaml"),
        ]
        .into_iter()
        .find(|name| self.kok(&["-n", "argocd", "get", "application.argoproj.io", name]))
    }

    fn other_hint(&self, verb: &str) {
        let other = self.other_tier();
        if self.exists(&self.name_of(other)) {
            detail(&format!(
                "the {other} tier is up. To use it, run 'just cluster:{verb} -- {other}'"
            ));
        }
    }

    pub fn wait_for(&self, what: &str, budget: u64, mut probe: impl FnMut() -> bool) -> Result<()> {
        let mut waited = 0;
        while !probe() {
            if waited >= budget {
                bail!("timed out after {budget}s waiting for {what}");
            }
            sleep(2);
            waited += 2;
        }
        Ok(())
    }

    fn run_stage(&self, stage: &str) -> Result<()> {
        match stage {
            "registry" => self.stage_registry(),
            "warm" => self.stage_warm(),
            "cluster" => self.stage_cluster(),
            "cni" => self.stage_cni(),
            "coredns" => self.stage_coredns(),
            "namespaces" => self.stage_namespaces(),
            "edge" => self.stage_edge(),
            "glue" => self.stage_glue(None),
            "postgres" => self.stage_postgres(),
            "certs" => self.stage_certs(),
            "priority" => self.stage_priority(),
            "middlewares" => self.stage_middlewares(),
            "secrets" => self.stage_secrets(),
            "ory" => self.stage_ory(),
            "identities" => exec::status("bash", &["scripts/identity-seed.sh"], &[]),
            "kyverno" => self.stage_kyverno(),
            "sopskey" => self.stage_sopskey(),
            "images" => super::cluster_images::stage(self),
            "argocd" => self.stage_argocd(),
            "rootapp" => super::cluster_argo::stage_rootapp(self, &self.up_started),
            "populatezot" => {
                self.stage_populate();
                Ok(())
            }
            other => bail!("no stage {other}"),
        }
    }

    fn ci_job() -> String {
        let part = |name: &str| std::env::var(name).unwrap_or_default();
        format!(
            "{}-{}-{}",
            part("GITHUB_RUN_ID"),
            part("GITHUB_JOB"),
            part("GITHUB_RUN_ATTEMPT")
        )
    }

    fn registry_job() -> String {
        exec::lossy(
            "docker",
            &[
                "inspect",
                "-f",
                r#"{{index .Config.Labels "platform.ci-job"}}"#,
                REGISTRY,
            ],
        )
        .unwrap_or_default()
        .trim()
        .to_string()
    }

    fn ci_owned(name: &str) -> bool {
        if variable("CI").is_none() {
            return true;
        }
        variable("RUNNER_TEMP")
            .and_then(|dir| fs::read_to_string(Path::new(&dir).join("kind-cluster")).ok())
            .is_some_and(|text| text.lines().any(|l| l == name))
    }

    /// A CI job waits for another job on the same runner, and never moves its registry or cluster.
    fn ci_wait_for_runner(&self) -> Result<()> {
        let mut waited = 0;
        loop {
            let held = Self::registry_job();
            let other_job = !held.is_empty() && held != Self::ci_job();
            if !other_job
                && !self.exists(&self.name_of("base"))
                && !self.exists(&self.name_of("full"))
            {
                return Ok(());
            }
            if waited >= 3600 {
                bail!("another job has held this runner's registry and edge for an hour");
            }
            if waited == 0 {
                step("waiting for another job to release this runner's registry and edge");
            }
            sleep(30);
            waited += 30;
        }
    }

    fn stage_registry(&self) -> Result<()> {
        if variable("CI").is_some() {
            self.ci_wait_for_runner()?;
        }
        let runtime = variable("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".to_string());
        let creds = format!("{runtime}/zot-sync-creds-{REGISTRY}.json");
        if Path::new(&creds).is_dir() {
            fs::remove_dir_all(&creds)?;
        }
        let body = if let (Some(user), Some(token)) =
            (variable("DOCKERHUB_USERNAME"), variable("DOCKERHUB_TOKEN"))
        {
            detail(&format!("docker hub sync authenticated as {user}"));
            serde_json::json!({ "registry-1.docker.io": { "username": user, "password": token } })
                .to_string()
        } else {
            detail(
                "docker hub sync is anonymous. The pull limit for each IP applies, and this runner shares its IP",
            );
            "{}".to_string()
        };
        fs::write(&creds, format!("{body}\n"))?;
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&creds, fs::Permissions::from_mode(0o600))?;

        let root = std::env::current_dir()?;
        let cache = variable("XDG_CACHE_HOME")
            .unwrap_or_else(|| format!("{}/.cache", std::env::var("HOME").unwrap_or_default()));
        let data = variable("ZOT_DATA").unwrap_or_else(|| format!("{cache}/zot/{REGISTRY}"));
        let config = format!("{}/infra/local/zot-config.yaml", root.display());
        let mut want = vec![
            format!("{config}:/etc/zot/config.yaml"),
            format!("{creds}:/etc/zot/sync-creds.json"),
            format!("{data}:/var/lib/zot"),
        ];
        want.sort();
        if exec::succeeds("docker", &["inspect", REGISTRY]) {
            let raw = exec::output(
                "docker",
                &[
                    "inspect",
                    "-f",
                    "{{range .Mounts}}{{.Source}}:{{.Destination}}{{\"\\n\"}}{{end}}",
                    REGISTRY,
                ],
            )?;
            let mut have: Vec<String> = raw
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            have.sort();
            if have != want {
                step(&format!("recreating '{REGISTRY}' against this checkout"));
                exec::output("docker", &["rm", "-f", REGISTRY])?;
            }
        }
        if exec::succeeds("docker", &["inspect", REGISTRY]) {
            if exec::output("docker", &["inspect", "-f", "{{.State.Running}}", REGISTRY])?.trim()
                != "true"
            {
                step(&format!("starting the local registry '{REGISTRY}'"));
                exec::output("docker", &["start", REGISTRY])?;
            }
        } else {
            step(&format!(
                "creating the local registry '{REGISTRY}:5000', which is zot"
            ));
            fs::create_dir_all(&data)?;
            let user = format!(
                "{}:{}",
                exec::output("id", &["-u"])?.trim(),
                exec::output("id", &["-g"])?.trim()
            );
            let label = format!("platform.ci-job={}", Self::ci_job());
            let (config_mount, creds_mount, data_mount) = (
                format!("{config}:/etc/zot/config.yaml:ro"),
                format!("{creds}:/etc/zot/sync-creds.json:ro"),
                format!("{data}:/var/lib/zot"),
            );
            let image = crate::zot_image().context("the shell library names no registry image")?;
            let mut args = vec!["run", "-d", "--restart=always", "--name", REGISTRY];
            if variable("CI").is_some() {
                args.extend(["--label", &label]);
            }
            args.extend([
                "-p",
                "127.0.0.1:5000:5000",
                "--user",
                &user,
                "-v",
                &config_mount,
                "-v",
                &creds_mount,
                "-v",
                &data_mount,
                &image,
                "serve",
                "/etc/zot/config.yaml",
            ]);
            exec::output("docker", &args)?;
        }
        let mut waited = 0;
        while exec::lossy(
            "curl",
            &[
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--noproxy",
                "*",
                "--max-time",
                "5",
                "http://127.0.0.1:5000/v2/",
            ],
        )
        .unwrap_or_default()
            != "200"
        {
            if waited >= 30 {
                let logs = Command::new("docker")
                    .args(["logs", "--tail", "15", REGISTRY])
                    .output()
                    .context("cannot run docker")?;
                bail!(
                    "the registry '{REGISTRY}' is not serving on :5000:\n{}",
                    String::from_utf8_lossy(&logs.stderr)
                );
            }
            sleep(1);
            waited += 1;
        }
        Ok(())
    }

    fn stage_warm(&self) -> Result<()> {
        const REFS: &str = "infra/local/image-refs.txt";
        let text = fs::read_to_string(REFS)
            .with_context(|| format!("{REFS} is missing. Run 'just gen-image-allowlist'"))?;
        let tier = variable("WARM_TIER").unwrap_or_else(|| self.tier.clone());
        let wanted = warm_list(&text, &tier);
        if wanted.is_empty() {
            bail!("{REFS} names no image for the {tier} tier. Run 'just gen-image-allowlist'");
        }
        step(&format!(
            "warming the registry with {} third-party image(s) for the {tier} tier",
            wanted.len()
        ));
        let (mut warmed, mut fetched, mut missed, mut missed_paths) =
            (0, 0, Vec::new(), Vec::new());
        for reference in &wanted {
            let image = ImageRef::parse(reference);
            if image.cached() {
                warmed += 1;
                continue;
            }
            detail(&format!("· {reference}"));
            let status = image.fetch();
            if status == "200" {
                fetched += 1;
            } else {
                missed.push(format!("{reference}, HTTP {status}"));
                missed_paths.push(image.path.clone());
            }
        }
        ok(&format!(
            "registry warm: {warmed} already cached, {fetched} fetched"
        ));
        if missed.is_empty() {
            return Ok(());
        }
        for line in &missed {
            eprintln!("    · {line}");
        }
        report_missed(&missed_paths);
        bail!(
            "{} image(s) above could not be cached. The nodes pull only from\n  this registry, so the cluster cannot start without them. Run it again to retry.\n  If it still fails, check egress with 'just proxy-setup --check'.",
            missed.len()
        );
    }

    fn stage_cluster(&self) -> Result<()> {
        let name = self.name();
        let other = self.name_of(self.other_tier());
        if self.exists(&other) && node_running(&other) {
            let stop = if self.tier == "full" { "base" } else { "full" };
            bail!(
                "cluster '{other}' is running and holds the edge ports 8080 and 8443.\n  Run one tier at a time:\n    just cluster-stop {stop}"
            );
        }
        if self.exists(&name) {
            let nodes = exec::output("kind", &["get", "nodes", "--name", &name])?;
            let nodes: Vec<&str> = nodes.lines().collect();
            let stopped = nodes.iter().any(|n| {
                exec::lossy("docker", &["inspect", "-f", "{{.State.Running}}", n])
                    .is_some_and(|o| o.trim() == "false")
            });
            if stopped {
                step(&format!(
                    "starting {} stopped node(s) of '{name}'",
                    nodes.len()
                ));
                for node in &nodes {
                    exec::output("docker", &["start", node])?;
                }
                // During a cold start, the API server can answer Forbidden until its RBAC is loaded.
                self.wait_for("the API server", 180, || self.kok(&["get", "nodes"]))?;
                self.k(&[
                    "wait",
                    "--for=condition=Ready",
                    "node",
                    "--all",
                    "--timeout=300s",
                ])?;
            }
        } else {
            step(&format!(
                "creating kind cluster '{name}' from {KIND_CONFIG}"
            ));
            exec::status(
                "kind",
                &[
                    "create",
                    "cluster",
                    "--name",
                    &name,
                    "--config",
                    KIND_CONFIG,
                ],
                &[],
            )?;
            if let (Some(_), Some(dir)) = (variable("CI"), variable("RUNNER_TEMP")) {
                fs::write(Path::new(&dir).join("kind-cluster"), format!("{name}\n"))?;
            }
        }
        if exec::succeeds("docker", &["network", "inspect", "kind"]) {
            let networks = exec::output(
                "docker",
                &[
                    "inspect",
                    REGISTRY,
                    "--format",
                    "{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
                ],
            )?;
            if !networks.split_whitespace().any(|n| n == "kind") {
                step(&format!("attaching {REGISTRY} to the kind network"));
                exec::output("docker", &["network", "connect", "kind", REGISTRY])?;
            }
        }
        exec::output("kubectl", &["config", "use-context", &self.ctx()])?;
        Ok(())
    }

    fn stage_cni(&self) -> Result<()> {
        let ready = self
            .kout(&[
                "get",
                "node",
                "-o",
                r#"jsonpath={.items[0].status.conditions[?(@.type=="Ready")].status}"#,
            ])
            .unwrap_or_default();
        if !self.forced("cni")
            && self.hok(&["-n", "kube-system", "status", "cilium"])
            && ready.contains("True")
        {
            return Ok(());
        }
        let apiserver = format!("{}-control-plane", self.name());
        step(&format!(
            "installing Cilium with apiserver {apiserver}:6443"
        ));
        chart_deps("infra/helm/platform/cilium")?;
        let host = format!("cilium.k8sServiceHost={apiserver}");
        self.h(&[
            "upgrade",
            "--install",
            "cilium",
            "infra/helm/platform/cilium",
            "-n",
            "kube-system",
            "--set",
            "cilium.operator.replicas=1",
            "--set",
            &host,
            "--set",
            "cilium.k8sServicePort=6443",
            "--timeout",
            "5m",
        ])?;
        self.k(&[
            "wait",
            "--for=condition=Ready",
            "node",
            "--all",
            "--timeout=600s",
        ])?;
        self.kq(&[
            "-n",
            "kube-system",
            "patch",
            "svc",
            "hubble-peer",
            "--type",
            "merge",
            "-p",
            r#"{"spec":{"publishNotReadyAddresses":true}}"#,
        ])?;
        ok("Cilium installed, node Ready");
        Ok(())
    }

    fn stage_coredns(&self) -> Result<()> {
        let corefile = self.kout(&[
            "-n",
            "kube-system",
            "get",
            "configmap",
            "coredns",
            "-o",
            "jsonpath={.data.Corefile}",
        ])?;
        if !self.forced("coredns") && corefile.contains(&format!("name exact {}", self.domain)) {
            return Ok(());
        }
        step(&format!("rewriting {} to the edge in CoreDNS", self.domain));
        let patched = patch_corefile(&corefile, &self.domain);
        let literal = format!("Corefile={patched}");
        let manifest = self.kout(&[
            "-n",
            "kube-system",
            "create",
            "configmap",
            "coredns",
            &format!("--from-literal={literal}"),
            "--dry-run=client",
            "-o",
            "yaml",
        ])?;
        self.apply(None, &manifest)?;
        self.kq(&["-n", "kube-system", "rollout", "restart", "deploy/coredns"])?;
        self.k(&[
            "-n",
            "kube-system",
            "rollout",
            "status",
            "deploy/coredns",
            "--timeout=180s",
        ])
    }

    fn stage_namespaces(&self) -> Result<()> {
        step("applying the namespaces and their pod-security profile");
        let manifest = self.hout(&[
            "template",
            "namespaces",
            "infra/helm/platform/namespaces",
            "-f",
            "infra/gitops/platform/local/values.yaml",
        ])?;
        self.apply(None, &manifest)
    }

    fn stage_edge(&self) -> Result<()> {
        if !self.forced("edge")
            && self.hok(&["-n", "kube-system", "status", "traefik"])
            && self.kok(&[
                "-n",
                "kube-system",
                "rollout",
                "status",
                "ds/traefik",
                "--timeout=0",
            ])
        {
            return Ok(());
        }
        step("installing Traefik, the edge controller");
        chart_deps("infra/helm/platform/traefik")?;
        self.h(&[
            "upgrade",
            "--install",
            "traefik",
            "infra/helm/platform/traefik",
            "-n",
            "kube-system",
            "--timeout",
            "5m",
        ])?;
        self.k(&[
            "-n",
            "kube-system",
            "rollout",
            "status",
            "ds/traefik",
            "--timeout=300s",
        ])
    }

    /// Points the edge at a process on the host. Without a target, it applies the shared glue for the frontend.
    pub fn stage_glue(&self, target: Option<(&str, &str)>) -> Result<()> {
        if !self.kok(&["get", "crd", "ingressroutes.traefik.io"]) {
            bail!("traefik.io CRDs are not registered. Start the cluster first");
        }
        let (name, port) = target.unwrap_or(("frontend", "3000"));
        let node = format!("{}-control-plane", self.name());
        let gateway = exec::output(
            "docker",
            &[
                "inspect",
                &node,
                "--format",
                "{{range .NetworkSettings.Networks}}{{.Gateway}}{{end}}",
            ],
        )?
        .trim()
        .to_string();
        if gateway.is_empty() {
            bail!("could not read the docker-bridge gateway for {node}");
        }
        if target.is_none() {
            self.kq(&["apply", "-f", "infra/local/edge-auth.yaml"])?;
        }
        self.apply(None, &endpoint_slice(name, &self.ns, port, &gateway))?;
        ok(&format!(
            "edge glue applied: {name} → host {gateway}:{port}"
        ));
        Ok(())
    }

    fn stage_postgres(&self) -> Result<()> {
        step("applying the postgres dependency stand-in, which is Kratos's store");
        self.kq(&[
            "apply",
            "-f",
            "infra/local/deps.yaml",
            "-l",
            "local.platform/component=postgres",
        ])?;
        self.k(&[
            "-n",
            &self.ns,
            "rollout",
            "status",
            "deploy/postgres",
            "--timeout=180s",
        ])
    }

    fn stage_certs(&self) -> Result<()> {
        step("installing cert-manager and the self-signed wildcard issuer");
        const CHART: &str = "infra/helm/platform/cert-manager";
        chart_deps(CHART)?;
        let common = [
            "upgrade",
            "--install",
            "cert-manager",
            CHART,
            "-n",
            &self.ns,
            "--create-namespace",
            "--timeout",
            "5m",
            "--wait",
            "-f",
            "infra/gitops/platform/local/values.yaml",
        ];
        let mut first = common.to_vec();
        first.extend(["--set", "issuers.enabled=false"]);
        self.h(&first)?;
        self.k(&[
            "-n",
            &self.ns,
            "rollout",
            "status",
            "deploy/cert-manager-webhook",
            "--timeout=180s",
        ])?;
        for attempt in 1..=3 {
            if self.h(&common).is_ok() {
                break;
            }
            if attempt == 3 {
                bail!("cert-manager did not install after 3 attempts");
            }
            detail("waiting for the cert-manager webhook, then retrying");
            let _ = self.k(&[
                "-n",
                &self.ns,
                "rollout",
                "status",
                "deploy/cert-manager-webhook",
                "--timeout=180s",
            ]);
        }
        self.k(&[
            "-n",
            &self.ns,
            "wait",
            "--for=condition=Ready",
            "certificate/wildcard",
            "--timeout=120s",
        ])
    }

    fn stage_priority(&self) -> Result<()> {
        step("applying the priority classes services are scheduled by");
        let rendered = self.hout(&[
            "template",
            "resource-governance",
            "infra/helm/platform/resource-governance",
            "-f",
            "infra/gitops/platform/local/values.yaml",
        ])?;
        let work = TempDir::new("priority")?;
        let file = work.0.join("rendered.yaml");
        fs::write(&file, rendered)?;
        let only = exec::output(
            "yq",
            &[
                r#"select(.kind == "PriorityClass")"#,
                &file.to_string_lossy(),
            ],
        )?;
        self.apply(None, &only)
    }

    fn stage_middlewares(&self) -> Result<()> {
        step("applying the edge middlewares");
        self.kq(&[
            "apply",
            "-n",
            &self.ns,
            "-f",
            "infra/gateway/middlewares.yaml",
        ])
    }

    fn stage_secrets(&self) -> Result<()> {
        secrets::rotate_local(&[])?;
        step("materialising kratos-secrets from the committed local SOPS bundle");
        let plain = exec::output_env(
            "sops",
            &[
                "-d",
                "infra/gitops/platform/local/secrets/platform.enc.yaml",
            ],
            &[("SOPS_AGE_KEY_FILE", "infra/gitops/platform/local/age.key")],
        )?;
        let work = TempDir::new("kratos")?;
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&work.0, fs::Permissions::from_mode(0o700))?;
        let file = work.0.join("plain.yaml");
        fs::write(&file, plain)?;
        let raw = exec::output(
            "yq",
            &[
                "-o=json",
                r#".spec.secretTemplates[] | select(.name == "kratos-secrets") | .stringData"#,
                &file.to_string_lossy(),
            ],
        )?;
        if raw.trim().is_empty() {
            bail!("kratos-secrets not found in the local SOPS bundle");
        }
        let data: serde_json::Value = serde_json::from_str(&raw).context("a bad kratos secret")?;
        let field = |key: &str| data[key].as_str().unwrap_or("null").to_string();
        let dsn = format!(
            "postgres://dev:dev@postgres.{}.svc.cluster.local:5432/kratos?sslmode=disable",
            self.ns
        );
        // The literals go through a generated manifest on standard input, so no secret reaches the process list.
        let manifest = serde_json::json!({
            "apiVersion": "v1", "kind": "Secret", "metadata": { "name": "kratos-secrets", "namespace": self.ns },
            "type": "Opaque",
            "stringData": { "secretsDefault": field("secretsDefault"), "secretsCookie": field("secretsCookie"), "secretsCipher": field("secretsCipher"), "smtpConnectionURI": field("smtpConnectionURI"), "dsn": dsn }
        });
        self.apply(None, &manifest.to_string())
    }

    fn stage_ory(&self) -> Result<()> {
        step("installing kratos and oathkeeper");
        const CHART: &str = "infra/helm/platform/ory";
        chart_deps(CHART)?;
        self.h(&[
            "upgrade", "--install", "ory", CHART, "-n", &self.ns, "--create-namespace", "--timeout", "8m", "--wait",
            "-f", "infra/auth/kratos/values.yaml", "-f", "infra/auth/oathkeeper/values.yaml", "-f", "infra/gitops/platform/local/values.yaml",
            "--set", "firstOperator.enabled=false",
            "--set-file", r"kratos.kratos.identitySchemas.user\.v1\.json=infra/auth/kratos/identity-schemas/user.v1.json",
            "--set-file", "oathkeeper.oathkeeper.accessRules=infra/auth/oathkeeper/access-rules.json",
        ])
    }

    fn stage_sopskey(&self) -> Result<()> {
        step("planting sops-age-key, the local throwaway key");
        let manifest = self.kout(&[
            "-n",
            &self.ns,
            "create",
            "secret",
            "generic",
            "sops-age-key",
            "--from-file=keys.txt=infra/gitops/platform/local/age.key",
            "--dry-run=client",
            "-o",
            "yaml",
        ])?;
        self.apply(None, &manifest)
    }

    fn stage_kyverno(&self) -> Result<()> {
        if !self.kok(&[
            "-n",
            "kyverno",
            "get",
            "deploy",
            "kyverno-admission-controller",
        ]) {
            return Ok(());
        }
        step("waiting for the Kyverno admission webhook to serve");
        self.k(&[
            "-n",
            "kyverno",
            "rollout",
            "status",
            "deploy/kyverno-admission-controller",
            "--timeout=300s",
        ])?;
        self.wait_for("a ready kyverno endpoint", 120, || {
            self.kout(&[
                "-n",
                "kyverno",
                "get",
                "endpointslice",
                "-l",
                "kubernetes.io/service-name=kyverno-svc",
                "-o",
                "jsonpath={.items[*].endpoints[?(@.conditions.ready==true)].addresses[0]}",
            ])
            .is_ok_and(|o| !o.trim().is_empty())
        })
    }

    fn stage_argocd(&self) -> Result<()> {
        step("installing ArgoCD");
        const CHART: &str = "infra/helm/platform/argocd";
        chart_deps(CHART)?;
        let mut args = vec![
            "upgrade",
            "--install",
            "argocd",
            CHART,
            "-n",
            "argocd",
            "--create-namespace",
            "--timeout",
            "8m",
        ];
        if Path::new("infra/local/proxy.local.yaml").is_file() {
            args.extend(["-f", "infra/local/proxy.local.yaml"]);
        }
        self.h(&args)?;
        for deployment in [
            "argocd-server",
            "argocd-repo-server",
            "argocd-applicationset-controller",
        ] {
            self.k(&[
                "-n",
                "argocd",
                "rollout",
                "status",
                &format!("deploy/{deployment}"),
                "--timeout=300s",
            ])?;
        }
        super::cluster_argo::stage_repo_creds(self)
    }

    fn stage_populate(&self) {
        if variable("CI").is_some() {
            detail(
                "populate-zot: skipped in CI. Run `just cluster-populate-zot` to fill the zot.ops catalogue",
            );
            return;
        }
        if deploy::populate_zot(self).is_err() {
            warn(
                "populate-zot did not complete. The in-cluster zot console can be empty. This is not fatal",
            );
        }
    }

    fn epilogue(&self) {
        let (domain, login) = (&self.domain, login_hint());
        if self.tier == "base" {
            println!(
                "\n✓ cluster-up base: real edge, real identity, Postgres.\n\n  Nothing else is running. Add what you need:\n    cd services/<svc> && just server    native, pulls its own deps in\n    just cluster-add <svc>           in-cluster, from the working tree\n    just mock-start                     /api from the committed OpenAPI\n\n  Open:       https://{domain}:8443/\n  Log in:     {login}\n  Teardown:   just cluster-stop to keep the cache, or cluster-down to delete\n\n  Data does not persist here, and no workflow runs without dep-temporal. Check\n  persistence, Temporal behaviour, and authorization decisions on the full tier."
            );
        } else {
            println!(
                "\n✓ cluster-up full: ArgoCD deploys from master.\n  Product:      https://{domain}:8443/api/<resource>/\n  Log in:       https://{domain}:8443/auth/login as {login}\n  Ops tier:     https://<name>.ops.{domain}:8443/ for argocd, grafana, hubble,\n                temporal, seaweedfs, lowdefy, headlamp, and pgweb, per ADR-0306\n  Frontend:     run it natively on :3000. The edge glue routes /auth and the\n                landing page to the host.\n  Diagnose:     just cluster-status\n  Break-glass:  kubectl port-forward with your kubeconfig, see docs/guide/break-glass.md\n  Teardown:     just cluster-stop to keep the cache, or cluster-down to delete"
            );
        }
    }

    pub fn up(&self) -> Result<()> {
        step(&format!(
            "bringing up the {} tier as cluster '{}'",
            self.tier,
            self.name()
        ));
        for stage in PRELUDE {
            self.run_stage(stage)?;
        }
        let stages: &[&str] = if self.tier == "full" {
            &STAGES_FULL
        } else {
            &STAGES_BASE
        };
        for stage in stages {
            self.run_stage(stage)?;
        }
        self.epilogue();
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let name = self.name();
        if !self.exists(&name) {
            ok(&format!("cluster '{name}' does not exist. Nothing to stop"));
            self.other_hint("stop");
            return Ok(());
        }
        let nodes = exec::output("kind", &["get", "nodes", "--name", &name])?;
        let nodes: Vec<&str> = nodes.lines().collect();
        step(&format!(
            "stopping '{name}': {} node(s), image cache kept",
            nodes.len()
        ));
        let mut args = vec!["stop"];
        args.extend(&nodes);
        exec::output("docker", &args)?;
        ok(&format!("stopped. Resume with '{}'", self.up_hint()));
        Ok(())
    }

    pub fn down(&self) -> Result<()> {
        let name = self.name();
        if variable("CI").is_some() && Self::registry_job() == Self::ci_job() {
            exec::output("docker", &["rm", "-f", REGISTRY])?;
        }
        if !self.exists(&name) {
            ok(&format!(
                "cluster '{name}' does not exist. Nothing to delete"
            ));
            self.other_hint("down");
            return Ok(());
        }
        if !Self::ci_owned(&name) {
            ok(&format!(
                "cluster '{name}' belongs to another job on this runner. Left alone"
            ));
            return Ok(());
        }
        step(&format!("deleting kind cluster '{name}'"));
        exec::status("kind", &["delete", "cluster", "--name", &name], &[])?;
        ok(&format!(
            "deleted '{name}'. The registry container stays, because it is host-level"
        ));
        Ok(())
    }

    fn heal(&self) -> Result<()> {
        let name = self.name();
        if !exec::succeeds("docker", &["inspect", &format!("{name}-control-plane")]) {
            self.other_hint("heal");
            bail!("cluster '{name}' does not exist. Nothing to heal");
        }
        step(&format!("restarting the nodes of '{name}'"));
        let nodes = exec::output("kind", &["get", "nodes", "--name", &name])?;
        let mut args = vec!["restart"];
        args.extend(nodes.lines());
        exec::output("docker", &args)?;
        self.wait_for("the API server", 180, || {
            self.kok(&["get", "--raw", "/healthz"])
        })?;
        self.k(&[
            "wait",
            "--for=condition=Ready",
            "node",
            "--all",
            "--timeout=300s",
        ])?;
        self.kq(&["-n", "kube-system", "rollout", "restart", "ds/cilium"])?;
        self.k(&[
            "-n",
            "kube-system",
            "rollout",
            "status",
            "ds/cilium",
            "--timeout=180s",
        ])?;
        self.kq(&["-n", "kube-system", "rollout", "restart", "deploy/coredns"])?;
        if self
            .k(&[
                "-n",
                "kube-system",
                "rollout",
                "status",
                "deploy/coredns",
                "--timeout=180s",
            ])
            .is_err()
        {
            bail!(
                "CoreDNS is still not Ready. The Cilium datapath has a fault that a\n  restart does not clear. Recreate the cluster: just cluster-down {} && {}",
                self.tier,
                self.up_hint()
            );
        }
        self.run_stage("glue")?;
        ok("heal complete. Pods can reach the API again");
        Ok(())
    }

    fn status(&self) -> Result<()> {
        let name = self.name();
        step(&format!(
            "tier {}, cluster '{name}', context {}",
            self.tier,
            self.ctx()
        ));
        if !self.exists(&name) {
            warn(&format!("not created. Run '{}'", self.up_hint()));
            self.other_hint("status");
            return Ok(());
        }
        let state = exec::output("docker", &["inspect", "-f", "{{.State.Status}}", REGISTRY])
            .map_or_else(|_| "absent".to_string(), |o| o.trim().to_string());
        detail(&format!("registry {REGISTRY}:5000: {state}"));
        self.k(&["get", "nodes"])?;
        // The namespaces chart creates `argocd` on every tier, so only the CRD shows that Argo CD runs.
        if self.kok(&["get", "crd", "applications.argoproj.io"]) {
            self.k(&["-n", "argocd", "get", "applications.argoproj.io", "-o", "custom-columns=APP:.metadata.name,SYNC:.status.sync.status,HEALTH:.status.health.status"])?;
        } else {
            self.h(&["-n", &self.ns, "list", "--short"])?;
        }
        let pods = self
            .kout(&[
                "get",
                "pods",
                "-A",
                "--field-selector=status.phase!=Running,status.phase!=Succeeded",
            ])
            .unwrap_or_default();
        if pods.trim().is_empty() {
            ok("every pod is Running or Succeeded");
        } else {
            print!("{pods}");
        }
        Ok(())
    }

    fn classify(name: &str) -> Result<&'static str> {
        let mut kinds = Vec::new();
        if Path::new(&format!("services/{name}")).is_dir() && !name.starts_with('_') {
            kinds.push("service");
        }
        if Path::new(&format!("apps/{name}")).is_dir()
            && Path::new(&format!("infra/gitops/services/local/values/{name}.yaml")).is_file()
        {
            kinds.push("app");
        }
        if Path::new(&format!("infra/helm/platform/{name}")).is_dir() {
            kinds.push("chart");
        }
        match kinds.as_slice() {
            [one] => Ok(one),
            [] => {
                detail(&format!(
                    "services:        {}",
                    child_dirs("services")
                        .iter()
                        .filter(|n| !n.starts_with('_'))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                detail(&format!(
                    "platform charts: {}",
                    child_dirs("infra/helm/platform").join(" ")
                ));
                bail!("'{name}' is not a service, an app, or a platform chart");
            }
            many => bail!(
                "'{name}' is a {} at the same time. Rename one, because the namespaces must stay disjoint",
                many.join(" ")
            ),
        }
    }

    fn remove(&self, name: &str) -> Result<()> {
        self.require()?;
        let kind = Self::classify(name)?;
        let selector = format!("local.platform/glue={name}");
        if kind == "service" {
            let listed = self
                .kout(&[
                    "-n",
                    &self.ns,
                    "get",
                    "service,ingressroute,middleware,endpointslice",
                    "-l",
                    &selector,
                    "-o",
                    "name",
                ])
                .unwrap_or_default();
            if !listed.trim().is_empty() {
                step(&format!("removing the native edge glue for {name}"));
                self.kq(&[
                    "-n",
                    &self.ns,
                    "delete",
                    "service,ingressroute,middleware,endpointslice",
                    "-l",
                    &selector,
                    "--ignore-not-found",
                ])?;
            }
            let pidfile = deploy::run_dir().join(format!("portforward-{name}.pid"));
            if pidfile.is_file() {
                step(&format!("stopping the {name} port-forward"));
                let pid = fs::read_to_string(&pidfile)?;
                let _ = exec::succeeds("kill", &[pid.trim()]);
                fs::remove_file(&pidfile)?;
            }
        }
        let app = if kind == "chart" {
            Some(format!("local-platform-{name}"))
        } else {
            self.argo_service_app(name)
        };
        let owned = app
            .as_ref()
            .filter(|a| self.kok(&["-n", "argocd", "get", "application.argoproj.io", a]));
        if owned.is_some() {
            step(&format!("handing {name} back to ArgoCD"));
            self.kq(&[
                "-n",
                &self.ns,
                "delete",
                "secret",
                "-l",
                &format!("owner=helm,name={name}"),
                "--ignore-not-found",
            ])?;
        } else if self.hok(&["-n", &self.ns, "status", name]) {
            step(&format!("uninstalling {name}"));
            self.h(&["-n", &self.ns, "uninstall", name])?;
        } else {
            detail(&format!(
                "no helm release '{name}' in {}. Nothing to uninstall",
                self.ns
            ));
        }
        if let Some(app) = owned {
            step(&format!("restoring ArgoCD auto-sync on {app}"));
            self.kq(&[
                "-n",
                "argocd",
                "patch",
                "application.argoproj.io",
                app,
                "--type",
                "merge",
                "-p",
                r#"{"spec":{"syncPolicy":{"automated":{"prune":true,"selfHeal":true}}}}"#,
            ])?;
            detail(&format!("Argo syncs {name} again from committed master"));
        }
        ok(&format!("{name} removed"));
        Ok(())
    }
}

fn with_context<'a>(ctx: &'a str, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["--context", ctx];
    all.extend(args);
    all
}

fn login_hint() -> String {
    let root = std::env::current_dir().unwrap_or_default();
    let code = format!(
        "const {{ADMIN}} = await import('{}/test/e2e/fixtures/identities.ts'); console.log(ADMIN.email + ' with password ' + ADMIN.password)",
        root.display()
    );
    exec::output("bun", &["--silent", "-e", &code]).map_or_else(
        |_| "the admin in test/e2e/fixtures/identities.ts".to_string(),
        |o| o.trim().to_string(),
    )
}

fn child_dirs(root: &str) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

pub fn require_tools(tools: &[&str]) -> Result<()> {
    for tool in tools {
        let found = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()));
        if !found {
            bail!(
                "{tool} not found on PATH. Run this through just, inside the dev shell of flake.nix, which puts the pinned toolchain there"
            );
        }
    }
    Ok(())
}

pub fn chart_deps(chart: &str) -> Result<()> {
    if exec::succeeds("helm", &["dependency", "build", chart]) {
        return Ok(());
    }
    exec::output("helm", &["dependency", "update", chart]).map(|_| ())
}

/// Runs a tool with this text on standard input, and drops its standard output.
pub fn pipe_in(program: &str, args: &[&str], input: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    child
        .stdin
        .take()
        .context("no standard input")?
        .write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn warm_list(text: &str, tier: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let (first, image) = (words.next()?, words.next()?);
            (first == "base" || tier == "full").then(|| image.to_string())
        })
        .collect()
}

/// A reference split the way a registry client splits it.
#[derive(Debug, PartialEq, Eq)]
struct ImageRef {
    host: String,
    path: String,
    reference: String,
    tag: String,
}

impl ImageRef {
    fn parse(image: &str) -> Self {
        let (mut host, mut path) = ("docker.io".to_string(), image.to_string());
        if let Some((first, rest)) = image.split_once('/')
            && (first.contains('.') || first.contains(':') || first == "localhost")
        {
            host = first.to_string();
            path = rest.to_string();
        }
        let mut reference = String::new();
        if let Some((name, digest)) = path.clone().split_once('@') {
            reference = digest.to_string();
            path = name.to_string();
        }
        let mut tag = String::new();
        let last = path.rsplit('/').next().unwrap_or(&path).to_string();
        if let Some((_, value)) = last.split_once(':') {
            tag = value.to_string();
            path.truncate(path.len() - value.len() - 1);
        }
        if host == "docker.io" && !path.contains('/') {
            path = format!("library/{path}");
        }
        if reference.is_empty() {
            reference.clone_from(&tag);
        }
        Self {
            host,
            path,
            reference,
            tag,
        }
    }

    /// The tags API reads local storage only, so it answers without a call to the upstream.
    fn cached(&self) -> bool {
        if self.tag.is_empty() {
            return false;
        }
        let url = format!("http://127.0.0.1:5000/v2/{}/tags/list", self.path);
        let Some(body) = exec::lossy("curl", &["-sf", "--noproxy", "*", "--max-time", "10", &url])
        else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(&body).is_ok_and(|v| {
            v["tags"]
                .as_array()
                .is_some_and(|t| t.iter().any(|x| x == self.tag.as_str()))
        })
    }

    fn fetch(&self) -> String {
        let url = format!(
            "http://127.0.0.1:5000/v2/{}/manifests/{}?ns={}",
            self.path, self.reference, self.host
        );
        let mut status = "000".to_string();
        for attempt in 1..=3 {
            status = exec::lossy("curl", &["-s", "-o", "/dev/null", "-w", "%{http_code}", "--noproxy", "*", "--max-time", "900", "-H", "Accept: application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.docker.distribution.manifest.v2+json", &url])
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "000".to_string());
            if status == "200" {
                break;
            }
            if attempt < 3 {
                sleep(attempt * 5);
            }
        }
        status
    }
}

fn report_missed(paths: &[String]) {
    detail(&format!("{REGISTRY} log for the images that missed:"));
    let logs = Command::new("docker").args(["logs", REGISTRY]).output();
    let text = logs
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default();
    let own: Vec<&str> = text
        .lines()
        .filter(|l| paths.iter().any(|p| l.contains(p.as_str())))
        .collect();
    let is_error = |l: &&&str| {
        let lower = l.to_lowercase();
        [
            "\"level\":\"error\"",
            "\"level\":\"warn\"",
            "denied",
            "unauthorized",
            "toomanyrequests",
            "rate limit",
            "ratelimit",
            "rate-limit",
            "error",
        ]
        .iter()
        .any(|w| lower.contains(w))
    };
    let errors: Vec<&&str> = own.iter().filter(is_error).collect();
    if !errors.is_empty() {
        for line in &errors[errors.len().saturating_sub(20)..] {
            eprintln!("      {line}");
        }
    } else if !own.is_empty() {
        detail("  sync attempted, no error logged. This is a truncated read, so run it again");
        for line in &own[own.len().saturating_sub(10)..] {
            eprintln!("      {line}");
        }
    } else {
        detail("  no sync attempted. The fault is the reference, not the upstream");
    }
}

fn patch_corefile(corefile: &str, domain: &str) -> String {
    let escaped = domain.replace('.', r"\.");
    let stanza = format!(
        "rewrite stop {{\n    name exact {domain} traefik.kube-system.svc.cluster.local\n    answer auto\n}}\nrewrite stop {{\n    name regex (.*)\\.{escaped} traefik.kube-system.svc.cluster.local\n    answer auto\n}}"
    );
    let mut lines = corefile.lines();
    let first = lines.next().unwrap_or_default();
    let rest: Vec<&str> = lines.collect();
    format!("{first}\n{stanza}\n{}\n", rest.join("\n"))
}

fn endpoint_slice(name: &str, namespace: &str, port: &str, gateway: &str) -> String {
    format!(
        "apiVersion: discovery.k8s.io/v1\nkind: EndpointSlice\nmetadata:\n  name: {name}-dev\n  namespace: {namespace}\n  labels:\n    kubernetes.io/service-name: {name}-dev\naddressType: IPv4\nports:\n  - name: http\n    port: {port}\n    protocol: TCP\nendpoints:\n  - addresses: [\"{gateway}\"]\n    conditions: {{ ready: true }}\n"
    )
}

/// `tabkit-ops cluster <verb> [args]`.
pub fn run(args: &[String]) -> Result<()> {
    let verb = args.first().map_or("up", String::as_str);
    let mut rest = args.get(1..).unwrap_or_default();
    let mut tier = variable("TIER");
    if matches!(verb, "up" | "stop" | "down" | "heal" | "status")
        && let Some(first) = rest.first()
    {
        valid_tier(first)?;
        tier = Some(first.clone());
        rest = &rest[1..];
    }
    let cluster = Cluster::with_tier(tier.as_deref().unwrap_or("base"))?;
    require_tools(&["docker", "kind", "kubectl", "helm"])?;
    match verb {
        "up" => cluster.up(),
        "stop" => cluster.stop(),
        "down" => cluster.down(),
        "heal" => cluster.heal(),
        "status" => cluster.status(),
        "add" => {
            let name = rest
                .first()
                .context("usage: just cluster-add <service|app|chart>")?;
            match Cluster::classify(name)? {
                "chart" => deploy::platform_deploy(&cluster, name),
                _ => deploy::service_deploy(&cluster, name, &mut Vec::new()),
            }
        }
        "remove" => cluster.remove(
            rest.first()
                .context("usage: just cluster-remove <service|app|chart>")?,
        ),
        "glue" => {
            cluster.require()?;
            match rest {
                [name, port, ..] => cluster.stage_glue(Some((name, port))),
                _ => cluster.stage_glue(None),
            }
        }
        other => {
            bail!("unknown verb '{other}'. Use up, stop, down, heal, status, add, remove, or glue")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_tier_has_its_own_name() {
        assert_eq!(tier_name("platform", "full"), "platform-full");
        assert_eq!(tier_name("platform", "base"), "platform");
    }

    #[test]
    fn a_running_tier_wins_and_an_unclear_answer_is_none() {
        assert_eq!(pick_tier(&["base", "full"], &["full"]), Some("full"));
        assert_eq!(pick_tier(&["base"], &[]), Some("base"));
        assert_eq!(pick_tier(&["base", "full"], &[]), None);
        assert_eq!(pick_tier(&["base", "full"], &["base", "full"]), None);
        assert_eq!(pick_tier(&[], &[]), None);
    }

    #[test]
    fn splits_each_shape_of_reference() {
        assert_eq!(
            ImageRef::parse("postgres:19-alpine"),
            ImageRef {
                host: "docker.io".into(),
                path: "library/postgres".into(),
                reference: "19-alpine".into(),
                tag: "19-alpine".into()
            }
        );
        assert_eq!(ImageRef::parse("alpine/k8s:1.37.2").path, "alpine/k8s");
        assert_eq!(ImageRef::parse("alpine/k8s:1.37.2").host, "docker.io");
        let pinned = ImageRef::parse("quay.io/cilium/cilium:v1.20@sha256:abc");
        assert_eq!(
            (
                pinned.host.as_str(),
                pinned.path.as_str(),
                pinned.reference.as_str(),
                pinned.tag.as_str()
            ),
            ("quay.io", "cilium/cilium", "sha256:abc", "v1.20")
        );
        assert_eq!(ImageRef::parse("registry:5000/a/b:1").host, "registry:5000");
    }

    #[test]
    fn the_base_tier_warms_only_its_images() {
        let text = "# header\n\nbase a:1\nfull b:1\n";
        assert_eq!(warm_list(text, "base"), vec!["a:1"]);
        assert_eq!(warm_list(text, "full"), vec!["a:1", "b:1"]);
    }

    #[test]
    fn the_coredns_rewrite_opens_the_default_block() {
        let patched = patch_corefile(".:53 {\n    errors\n}\n", "dev.localtest.me");
        assert!(patched.starts_with(".:53 {\nrewrite stop {\n    name exact dev.localtest.me "));
        assert!(patched.contains(r"name regex (.*)\.dev\.localtest\.me "));
        assert!(patched.ends_with("    errors\n}\n"));
    }

    #[test]
    fn the_stages_name_the_base_charts() {
        assert!(PRELUDE.contains(&"edge") && STAGES_BASE.contains(&"ory"));
        assert_eq!(BASE_CHARTS.len(), 6);
    }
}
