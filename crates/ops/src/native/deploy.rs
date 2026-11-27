//! One-time deploys from the working tree, the native service glue, and the GitOps bootstrap.

use std::collections::BTreeSet;
use std::fs;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::cluster::{Cluster, REGISTRY, chart_deps};
use super::files::TempDir;
use super::project::Background;
use super::{detail, exec, ok, step, values, warn};

/// The folder that holds the pidfiles and logs of the detached port-forwards.
pub fn run_dir() -> PathBuf {
    let base = ["XDG_RUNTIME_DIR", "TMPDIR"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "/tmp".to_string());
    Path::new(&base).join("platform-local")
}

/// The registered local port of a service, from the port registry of the project.
pub fn service_port(service: &str) -> Result<String> {
    const FILE: &str = "scripts/lib/ports.sh";
    let text = fs::read_to_string(FILE).with_context(|| format!("cannot read {FILE}"))?;
    parse_port(&text, service)
        .with_context(|| format!("no local port registered for {service}. Add it to {FILE}"))
}

fn parse_port(text: &str, service: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, port) = line.trim().split_once(':')?;
        (name == service && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
            .then(|| port.to_string())
    })
}

/// The `<prefix><name>` recipes that a service justfile calls, without comment lines.
fn declared(justfile: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in justfile
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
    {
        let mut rest = line;
        while let Some(at) = rest.find(prefix) {
            let before = rest[..at].chars().next_back();
            let tail = &rest[at + prefix.len()..];
            let length = tail
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(tail.len());
            let boundary = !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if boundary && tail.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
                out.insert(tail[..length].to_string());
            }
            rest = &tail[length..];
        }
    }
    out
}

/// Whether a block after the `worker:` line turns a worker on.
fn worker_enabled(values_text: &str) -> bool {
    let mut seen = false;
    for line in values_text.lines() {
        seen = seen || line.starts_with("worker:");
        if seen
            && line
                .trim_start()
                .strip_prefix("enabled:")
                .is_some_and(|v| v.trim_start().starts_with("true"))
        {
            return true;
        }
    }
    false
}

fn pause_app(cluster: &Cluster, app: &str) -> Result<()> {
    step(&format!("pausing ArgoCD auto-sync on {app}"));
    cluster.k(&[
        "-n",
        "argocd",
        "patch",
        "application.argoproj.io",
        app,
        "--type",
        "merge",
        "-p",
        r#"{"spec":{"syncPolicy":{"automated":null}}}"#,
    ])
}

fn now() -> Result<String> {
    Ok(exec::output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])?
        .trim()
        .to_string())
}

fn revision() -> String {
    let mut rev = exec::output("git", &["rev-parse", "--short=12", "HEAD"])
        .map_or_else(|_| "unknown".to_string(), |o| o.trim().to_string());
    if !exec::succeeds("git", &["diff", "--quiet"]) {
        rev.push_str("-dirty");
    }
    rev
}

fn build_and_load(
    cluster: &Cluster,
    reference: &str,
    dockerfile: &str,
    build_args: &[String],
) -> Result<()> {
    step(&format!("building {reference}"));
    let contexts = super::buildx::contexts(dockerfile)?;
    let mut args = vec!["build", "-t", reference];
    args.extend(contexts.iter().map(String::as_str));
    for arg in build_args {
        args.extend(["--build-arg", arg.as_str()]);
    }
    args.extend(["-f", dockerfile, "."]);
    exec::status("docker", &args, &[])?;
    exec::output(
        "kind",
        &["load", "docker-image", reference, "--name", &cluster.name()],
    )
    .map(|_| ())
}

/// Deploys one service or app from the working tree. `chain` is the visited set, because a call graph can hold a cycle.
pub fn service_deploy(cluster: &Cluster, service: &str, chain: &mut Vec<String>) -> Result<()> {
    let values_file = format!("infra/gitops/services/local/values/{service}.yaml");
    if !Path::new(&values_file).is_file() {
        bail!("missing local values: {values_file}");
    }
    let (kind, dir, image) = if Path::new(&format!("services/{service}")).is_dir() {
        (
            "service",
            format!("services/{service}"),
            format!("{service}-server"),
        )
    } else if Path::new(&format!("apps/{service}")).is_dir() {
        ("app", format!("apps/{service}"), service.to_string())
    } else {
        bail!("no such deployable: there is no services/{service} and no apps/{service}");
    };
    let justfile = fs::read_to_string(format!("{dir}/justfile")).unwrap_or_default();
    let mut deps = declared(&justfile, "dep-");
    if kind == "service" {
        deps.insert("db-secrets".to_string());
    }
    if cluster.tier == "full" {
        step("full tier: dependencies come from the platform charts, not stand-ins");
        deps.clear();
    }
    for dep in &deps {
        exec::status("bash", &["scripts/dep-apply.sh", dep], &[])?;
    }
    chain.push(service.to_string());
    for name in declared(&justfile, "svc-") {
        if chain.contains(&name) {
            step(&format!(
                "{name} already in this deploy chain. Skipping, to stop a cycle"
            ));
        } else if cluster.kok(&[
            "-n",
            &cluster.ns,
            "rollout",
            "status",
            &format!("deploy/{name}-server"),
            "--timeout=0",
        ]) {
            detail(&format!("{name} already deployed, skipping"));
        } else {
            step(&format!("{service} calls {name}. Deploying it first"));
            service_deploy(cluster, &name, chain)?;
        }
    }
    let tag = format!("local-{}", exec::output("date", &["+%s"])?.trim());
    let repo = format!("{REGISTRY}:5000/{image}");
    let worker_repo = format!("{REGISTRY}:5000/{service}-worker");
    let mut set = vec![
        format!("image.repository={repo}"),
        format!("image.tag={tag}"),
    ];
    let rev = revision();
    let text = fs::read_to_string(&values_file)?;
    let server = values::get(&text, ".server.enabled")? != "false";
    let dockerfile = format!("{dir}/Dockerfile");
    let reference = format!("{repo}:{tag}");
    let identity = |cmd: &str| -> Result<Vec<String>> {
        Ok(vec![
            format!("SERVICE={service}"),
            format!("APP_CMD={cmd}"),
            format!("GIT_SHA={rev}"),
            "BUILD_VERSION=local".to_string(),
            format!("BUILD_TIME={}", now()?),
        ])
    };
    if !server {
        detail("no server: this service runs a worker only");
    } else if kind == "service" {
        build_and_load(cluster, &reference, &dockerfile, &identity("server")?)?;
    } else {
        let origin = values::get(&text, ".env.EDGE_PUBLIC_ORIGIN")?;
        build_and_load(
            cluster,
            &reference,
            &dockerfile,
            &[
                format!("SERVICE_VERSION={rev}"),
                format!("EDGE_PUBLIC_ORIGIN={origin}"),
            ],
        )?;
    }
    if worker_enabled(&text) {
        build_and_load(
            cluster,
            &format!("{worker_repo}:{tag}"),
            &dockerfile,
            &identity("worker")?,
        )?;
        set.push(format!("worker.image.repository={worker_repo}"));
        set.push(format!("worker.image.tag={tag}"));
    }
    if let Some(app) = cluster.argo_service_app(service) {
        pause_app(cluster, &app)?;
    }
    step(&format!(
        "helm upgrade {service} with working-tree image {tag}"
    ));
    let mut args = vec![
        "upgrade",
        "--install",
        service,
        "infra/helm/service",
        "-n",
        &cluster.ns,
        "-f",
        "infra/gitops/services/local/shared.yaml",
        "-f",
        &values_file,
        "--take-ownership",
        "--force-conflicts",
        "--set",
        "image.pullPolicy=IfNotPresent",
    ];
    for pair in &set {
        args.extend(["--set", pair.as_str()]);
    }
    args.extend(["--timeout", "5m"]);
    cluster.h(&args)?;
    if server {
        let deployment = format!("deploy/{service}-server");
        cluster.k(&["-n", &cluster.ns, "rollout", "restart", &deployment])?;
        cluster.k(&[
            "-n",
            &cluster.ns,
            "rollout",
            "status",
            &deployment,
            "--timeout=180s",
        ])?;
    }
    ok(&format!(
        "{service} deployed from the working tree, tag {tag}"
    ));
    Ok(())
}

pub fn platform_deploy(cluster: &Cluster, chart: &str) -> Result<()> {
    let dir = format!("infra/helm/platform/{chart}");
    if !Path::new(&dir).is_dir() {
        bail!("no such platform chart: {dir}");
    }
    if chart == "admin" {
        let image = "registry.localhost:5000/admin:local";
        step(&format!(
            "regenerating admin pages and rebuilding the admin image {image}"
        ));
        super::generate::gen_admin()?;
        let mut args = vec!["build", "-t", image, "-f", "apps/admin/Dockerfile"];
        let contexts = super::buildx::contexts("apps/admin/Dockerfile")?;
        args.extend(contexts.iter().map(String::as_str));
        args.push(".");
        exec::status("docker", &args, &[])?;
        exec::status("docker", &["push", image], &[])?;
        exec::output(
            "kind",
            &["load", "docker-image", image, "--name", &cluster.name()],
        )?;
    }
    let app = format!("local-platform-{chart}");
    if cluster.kok(&["-n", "argocd", "get", "application.argoproj.io", &app]) {
        pause_app(cluster, &app)?;
    }
    step(&format!("helm upgrade {chart} from the working tree"));
    let ctx = cluster.ctx();
    exec::output(
        "helm",
        &["--kube-context", &ctx, "dependency", "update", &dir],
    )?;
    let mut args = vec![
        "upgrade",
        "--install",
        chart,
        &dir,
        "-n",
        &cluster.ns,
        "--take-ownership",
        "--force-conflicts",
        "-f",
        "infra/auth/kratos/values.yaml",
        "-f",
        "infra/auth/oathkeeper/values.yaml",
    ];
    match chart {
        "openfga" => args.extend(["--set-file", "seed.model=infra/auth/openfga/model.json"]),
        "ory" => args.extend([
            "--set-file", r"kratos.kratos.identitySchemas.user\.v1\.json=infra/auth/kratos/identity-schemas/user.v1.json",
            "--set-file", "oathkeeper.oathkeeper.accessRules=infra/auth/oathkeeper/access-rules.json",
        ]),
        _ => {}
    }
    args.extend([
        "-f",
        "infra/gitops/platform/local/values.yaml",
        "--timeout",
        "8m",
    ]);
    cluster.h(&args)?;
    if chart == "admin" {
        step("restarting admin to run the rebuilt image");
        cluster.k(&["-n", &cluster.ns, "rollout", "restart", "deploy/admin"])?;
        cluster.k(&[
            "-n",
            &cluster.ns,
            "rollout",
            "status",
            "deploy/admin",
            "--timeout=180s",
        ])?;
    }
    ok(&format!("{chart} overlaid from the working tree."));
    detail("Re-enable GitOps when done:");
    detail(&format!(
        "  kubectl -n argocd patch application.argoproj.io {app} --type merge \\"
    ));
    detail(
        "    -p '{\"spec\":{\"syncPolicy\":{\"automated\":{\"prune\":true,\"selfHeal\":true}}}}'",
    );
    Ok(())
}

fn port_open(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|p| {
        TcpStream::connect_timeout(&([127, 0, 0, 1], p).into(), Duration::from_millis(300)).is_ok()
    })
}

pub fn svc_apply(args: &[String]) -> Result<()> {
    let service = args
        .first()
        .context("usage: tabkit-ops svc-apply <service>")?;
    if !Path::new(&format!("services/{service}")).is_dir() {
        bail!("no such service: services/{service}");
    }
    let cluster = Cluster::detect()?;
    let port = service_port(service)?;
    let dir = run_dir();
    let pidfile = dir.join(format!("portforward-{service}.pid"));
    cluster.require()?;
    if port_open(&port) {
        detail(&format!(
            "svc-{service} already reachable on :{port}, skipping"
        ));
        return Ok(());
    }
    if pidfile.is_file() {
        let pid = fs::read_to_string(&pidfile)?;
        let _ = exec::succeeds("kill", &[pid.trim()]);
        fs::remove_file(&pidfile)?;
    }
    if cluster.kok(&[
        "-n",
        &cluster.ns,
        "rollout",
        "status",
        &format!("deploy/{service}-server"),
        "--timeout=0",
    ]) {
        detail(&format!("svc-{service} already deployed, forwarding only"));
    } else {
        step(&format!(
            "svc-{service}: deploying, because the caller needs it and you are not working on it"
        ));
        service_deploy(&cluster, service, &mut Vec::new())?;
    }
    step(&format!(
        "svc-{service}: forwarding :{port} → {service}-server:80"
    ));
    fs::create_dir_all(&dir)?;
    let log_path = dir.join(format!("portforward-{service}.log"));
    let log = fs::File::create(&log_path)?;
    // The forward outlives this command, because the next recipe needs it. `cluster-remove` stops it through the pidfile.
    let child = Command::new("nohup")
        .args([
            "kubectl",
            "--context",
            &cluster.ctx(),
            "-n",
            &cluster.ns,
            "port-forward",
            &format!("svc/{service}-server"),
            &format!("{port}:80"),
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("cannot run kubectl")?;
    fs::write(&pidfile, format!("{}\n", child.id()))?;
    for _ in 0..50 {
        if port_open(&port) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !port_open(&port) {
        bail!(
            "svc-{service}: port-forward did not start. See {}",
            log_path.display()
        );
    }
    ok(&format!("svc-{service} reachable on :{port}"));
    Ok(())
}

fn match_rule(resources: &[&str]) -> String {
    resources
        .iter()
        .map(|r| format!("PathPrefix(`/api/{r}`)"))
        .collect::<Vec<_>>()
        .join(" || ")
}

fn glue_service(service: &str, ns: &str, port: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: {service}-dev\n  namespace: {ns}\n  labels: {{ local.platform/glue: \"{service}\" }}\nspec:\n  ports:\n    - name: http\n      port: {port}\n      targetPort: {port}\n"
    )
}

fn glue_route(service: &str, ns: &str, port: &str, domain: &str, rule: &str) -> String {
    format!(
        "apiVersion: traefik.io/v1alpha1\nkind: Middleware\nmetadata:\n  name: {service}-dev-stripprefix\n  namespace: {ns}\n  labels: {{ local.platform/glue: \"{service}\" }}\nspec:\n  stripPrefix:\n    prefixes:\n      - /api\n---\napiVersion: traefik.io/v1alpha1\nkind: IngressRoute\nmetadata:\n  name: {service}-dev\n  namespace: {ns}\n  labels: {{ local.platform/glue: \"{service}\" }}\nspec:\n  entryPoints: [websecure]\n  routes:\n    - kind: Rule\n      match: Host(`{domain}`) && ({rule})\n      middlewares:\n        - name: strip-identity-headers\n        - name: oathkeeper-forward-auth\n        - name: security-headers\n        - name: {service}-dev-stripprefix\n      services:\n        - name: {service}-dev\n          port: {port}\n  tls:\n    secretName: wildcard-tls\n"
    )
}

pub fn service_dev(args: &[String]) -> Result<()> {
    let service = args.first().context("usage: just service-dev <svc>")?;
    let cluster = Cluster::detect()?;
    let port = service_port(service)?;
    let dir = format!("services/{service}");
    let values_file = format!("infra/gitops/services/local/values/{service}.yaml");
    if !Path::new(&dir).is_dir() {
        bail!("no such service: {dir}");
    }
    if !Path::new(&values_file).is_file() {
        bail!("missing local values: {values_file}");
    }
    if cluster.kok(&[
        "-n",
        &cluster.ns,
        "get",
        &format!("deploy/{service}-server"),
    ]) {
        bail!("{service} is deployed in-cluster. Run 'just cluster-remove {service}' first");
    }
    step(&format!("stamping the edge glue for a native {service}"));
    cluster.apply(None, &glue_service(service, &cluster.ns, &port))?;
    let listed = exec::output(
        "yq",
        &[
            "-r",
            r#".ingress.resources // [] | join(" ")"#,
            &values_file,
        ],
    )?;
    let resources: Vec<&str> = listed.split_whitespace().collect();
    if resources.is_empty() {
        bail!(
            "{service} declares no ingress.resources in {values_file}, so there is nothing to route"
        );
    }
    cluster.apply(
        None,
        &glue_route(
            service,
            &cluster.ns,
            &port,
            &cluster.domain,
            &match_rule(&resources),
        ),
    )?;
    cluster.stage_glue(Some((service, &port)))?;
    println!(
        "\n✓ {service} glued to the host on :{port}.\n\n  Now run it. Its own tasks pull in the dependency components:\n    cd services/{service} && just server\n\n  Reached at:  https://{}:8443/api/{}\n  Through:     the real Oathkeeper chain, so identity headers are real here\n  Remove:      just cluster-remove {service}",
        cluster.domain, resources[0]
    );
    Ok(())
}

pub fn populate_zot(cluster: &Cluster) -> Result<()> {
    if cluster.tier != "full" {
        detail(&format!(
            "populate-zot: no in-cluster zot on the {} tier, skipping",
            cluster.tier
        ));
        return Ok(());
    }
    let env_or = |name: &str, default: &str| {
        std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let (user, password, port) = (
        env_or("ZOT_LOCAL_PUSH_USER", "ci"),
        env_or("ZOT_LOCAL_PUSH_PASSWORD", "ci-local-push"),
        env_or("ZOT_FWD_PORT", "5100"),
    );
    let (source, target) = ("127.0.0.1:5000", format!("127.0.0.1:{port}"));
    if !cluster.kok(&[
        "-n",
        &cluster.ns,
        "rollout",
        "status",
        "deploy/zot",
        "--timeout=120s",
    ]) {
        warn("populate-zot: zot is not ready, skipping");
        return Ok(());
    }
    step("mirroring first-party images into the in-cluster zot");
    let forward = Command::new("kubectl")
        .args([
            "--context",
            &cluster.ctx(),
            "-n",
            &cluster.ns,
            "port-forward",
            "svc/zot",
            &format!("{port}:5000"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot run kubectl")?;
    let _forward = Background(forward);
    // A docker config of its own, so the login touches nothing in the home folder of the engineer.
    let config = TempDir::new("docker")?;
    let config_text = config.0.to_string_lossy().into_owned();
    let envs = [("DOCKER_CONFIG", config_text.as_str())];
    let probe = format!("http://{target}/v2/");
    for _ in 0..30 {
        if exec::succeeds("curl", &["-sf", "-o", "/dev/null", &probe]) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let mut login = Command::new("docker")
        .args(["login", &target, "-u", &user, "--password-stdin"])
        .env("DOCKER_CONFIG", &config.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot run docker")?;
    {
        use std::io::Write as _;
        login
            .stdin
            .take()
            .context("no standard input")?
            .write_all(password.as_bytes())?;
    }
    if !login.wait()?.success() {
        bail!("populate-zot: the in-cluster zot refused the local push identity {user}");
    }
    let filter = format!("reference={source}/*:local");
    let listed = exec::output(
        "docker",
        &[
            "images",
            "--filter",
            &filter,
            "--format",
            "{{.Repository}}:{{.Tag}}",
        ],
    )?;
    let images: BTreeSet<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    let (mut copied, mut failed) = (0, 0);
    for image in images {
        let repo = image
            .strip_prefix(&format!("{source}/"))
            .unwrap_or(image)
            .trim_end_matches(":local");
        let dest = format!("{target}/{repo}:local");
        exec::output("docker", &["tag", image, &dest])?;
        match exec::output_env("timeout", &["300", "docker", "push", &dest], &envs) {
            Ok(_) => {
                detail(&format!("mirrored {repo}:local"));
                copied += 1;
            }
            Err(e) => {
                warn(&format!(
                    "could not mirror {repo}:local. The console will not show it: {e}"
                ));
                failed += 1;
            }
        }
        let _ = exec::succeeds("docker", &["rmi", &dest]);
    }
    if copied == 0 && failed == 0 {
        warn("populate-zot: no first-party image on this host. Run `just cluster-up full` first");
    }
    ok(&format!(
        "in-cluster zot populated: {copied} first-party image(s) mirrored, {failed} failed"
    ));
    Ok(())
}

pub fn argo(args: &[String]) -> Result<()> {
    const STS: &str = "statefulset/argocd-application-controller";
    let [verb] = args else {
        bail!("usage: tabkit-ops argo <pause|resume>");
    };
    if verb != "pause" && verb != "resume" {
        bail!("'{verb}' is not a verb. Use \"pause\" or \"resume\"");
    }
    let cluster = Cluster::detect()?;
    cluster.require()?;
    if cluster.tier != "full" {
        bail!(
            "Argo CD runs on the full tier only, and the {} tier is what is up.\n  Nothing reconciles the inner loop, so there is no window to open or close.",
            cluster.tier
        );
    }
    if !cluster.kok(&["-n", "argocd", "get", STS]) {
        bail!(
            "no application-controller in {}. The full tier is up but its\n  GitOps bootstrap is not. Run '{}' again.",
            cluster.ctx(),
            cluster.up_hint()
        );
    }
    if verb == "pause" {
        step(&format!(
            "pausing Argo CD on {}. The cluster stops tracking master",
            cluster.ctx()
        ));
        cluster.kq(&["-n", "argocd", "scale", STS, "--replicas=0"])?;
        ok("paused. Resume with 'just argo-resume' when the patch is committed");
        return Ok(());
    }
    step(&format!("resuming Argo CD on {}", cluster.ctx()));
    cluster.kq(&["-n", "argocd", "scale", STS, "--replicas=1"])?;
    if !cluster.kok(&["-n", "argocd", "rollout", "status", STS, "--timeout=180s"]) {
        bail!(
            "the application-controller did not come back. Run 'kubectl -n argocd describe {STS}'"
        );
    }
    let listed = cluster
        .kout(&[
            "-n",
            "argocd",
            "get",
            "application.argoproj.io",
            "--no-headers",
            "-o",
            "custom-columns=NAME:.metadata.name,AUTO:.spec.syncPolicy.automated",
        ])
        .unwrap_or_default();
    let manual: Vec<&str> = listed
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let name = w.next()?;
            (w.next() == Some("<none>")).then_some(name)
        })
        .collect();
    if !manual.is_empty() {
        warn(&format!(
            "{} application(s) still have auto-sync off. cluster-add pauses the app it deploys:",
            manual.len()
        ));
        for name in manual {
            eprintln!("    · {name}");
        }
        detail("turn auto-sync on again with: argocd app set <name> --sync-policy automated");
    }
    ok("resumed. The cluster tracks master again");
    Ok(())
}

fn install_chart(release: &str, chart: &str, namespace: &str, values: Option<&str>) -> Result<()> {
    if !Path::new(chart).join("charts").is_dir() {
        chart_deps(chart)?;
    }
    let mut args = vec![
        "upgrade",
        "--install",
        release,
        chart,
        "-n",
        namespace,
        "--create-namespace",
        "--timeout",
        "10m",
    ];
    if let Some(path) = values {
        args.extend(["-f", path]);
    }
    exec::status("helm", &args, &[])
}

fn kubectl_apply(manifest: &str) -> Result<()> {
    super::cluster::pipe_in("kubectl", &["apply", "-f", "-"], manifest)
}

/// The forge origin of a repository URL, so a credential survives a move to a new owner.
fn forge_origin(url: &str) -> String {
    url.strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
        .map_or_else(|| url.to_string(), |(host, _)| format!("https://{host}/"))
}

pub fn argocd_bootstrap(args: &[String]) -> Result<()> {
    let env = args
        .first()
        .context("usage: tabkit-ops argocd-bootstrap <env>")?;
    let root_app = format!("infra/gitops/{env}-bootstrap/root-application.yaml");
    let repo_secret = format!("infra/gitops/platform/{env}/secrets/argocd-repo.enc.yaml");
    if !Path::new(&root_app).is_file() {
        bail!("no {root_app}: '{env}' is not an environment");
    }
    step(&format!(
        "bootstrapping {env} in {}",
        exec::output("kubectl", &["config", "current-context"])?.trim()
    ));
    step("installing Argo CD");
    let values = std::env::var("ARGOCD_VALUES_FILE")
        .ok()
        .filter(|value| !value.is_empty());
    install_chart(
        "argocd",
        "infra/helm/platform/argocd",
        "argocd",
        values.as_deref(),
    )?;
    for name in [
        "argocd-server",
        "argocd-repo-server",
        "argocd-applicationset-controller",
    ] {
        exec::status(
            "kubectl",
            &[
                "-n",
                "argocd",
                "rollout",
                "status",
                &format!("deploy/{name}"),
                "--timeout=300s",
            ],
            &[],
        )?;
    }
    step("installing Traefik, whose CRDs the gateway's Middlewares resolve against");
    install_chart(
        "traefik",
        "infra/helm/platform/traefik",
        "kube-system",
        None,
    )?;
    exec::status(
        "kubectl",
        &[
            "-n",
            "kube-system",
            "rollout",
            "status",
            "ds/traefik",
            "--timeout=300s",
        ],
        &[],
    )?;
    step("the cluster's age key");
    kubectl_apply(&exec::output(
        "kubectl",
        &[
            "create",
            "namespace",
            "platform",
            "--dry-run=client",
            "-o",
            "yaml",
        ],
    )?)?;
    if let Some(key) = std::env::var("CLUSTER_AGE_KEY")
        .ok()
        .filter(|v| !v.is_empty())
    {
        let manifest = exec::output(
            "kubectl",
            &[
                "-n",
                "platform",
                "create",
                "secret",
                "generic",
                "sops-age-key",
                &format!("--from-file=keys.txt={key}"),
                "--dry-run=client",
                "-o",
                "yaml",
            ],
        )?;
        kubectl_apply(&manifest)?;
        detail(&format!("planted from {key}"));
    } else {
        if !exec::succeeds(
            "kubectl",
            &["-n", "platform", "get", "secret", "sops-age-key"],
        ) {
            bail!(
                "no sops-age-key in the cluster. Set CLUSTER_AGE_KEY to the {env} cluster's age private key"
            );
        }
        detail("present");
    }
    if Path::new(&repo_secret).is_file() {
        step("the forge credential");
        let plain = exec::output("sops", &["--decrypt", &repo_secret])?;
        let field = |key: &str| super::sign::string_data(&plain, key);
        let origin = forge_origin(&field("FORGE_REPO_URL")?);
        let manifest = serde_json::json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": { "name": "forge-creds", "namespace": "argocd", "labels": { "argocd.argoproj.io/secret-type": "repo-creds" } },
            "type": "Opaque",
            "stringData": { "type": "git", "url": origin, "username": field("FORGE_REPO_USERNAME")?, "password": field("FORGE_REPO_PASSWORD")? }
        });
        kubectl_apply(&manifest.to_string())?;
        detail(&origin);
    }
    step("applying the root application");
    exec::output("kubectl", &["apply", "-f", &root_app])?;
    ok(&format!(
        "Argo CD reconciles {env}. Watch it with 'kubectl -n argocd get applications'"
    ));
    Ok(())
}

pub fn ci_preview(args: &[String]) -> Result<()> {
    let verb = args.first().map_or("", String::as_str);
    if verb != "up" && verb != "down" {
        bail!("usage: tabkit-ops ci-preview up|down");
    }
    let pr = std::env::var("PREVIEW_PR").unwrap_or_default();
    if pr.is_empty() || !pr.bytes().all(|b| b.is_ascii_digit()) {
        bail!("PREVIEW_PR is not a pull request number: '{pr}'");
    }
    let wanted = format!("pr-{pr}");
    if std::env::var("CLUSTER").ok().as_deref() != Some(wanted.as_str())
        || std::env::var("TIER").ok().as_deref() != Some("full")
    {
        use std::os::unix::process::CommandExt as _;
        let error = Command::new(std::env::current_exe()?)
            .args(std::env::args().skip(1))
            .env("CLUSTER", &wanted)
            .env("TIER", "full")
            .env_remove("TABKIT_CLUSTER_ENV")
            .exec();
        bail!("cannot start again: {error}");
    }
    let cluster = Cluster::with_tier("full")?;
    if verb == "down" {
        step(&format!("destroying preview pr-{pr}"));
        cluster.down()?;
        ok(&format!("preview pr-{pr} destroyed"));
        return Ok(());
    }
    let rev = format!("refs/pull/{pr}/head");
    step(&format!("preview pr-{pr} on {rev}"));
    cluster.up()?;
    step(&format!("repointing the root application at {rev}"));
    let patch = serde_json::json!({ "spec": { "source": { "targetRevision": rev } } }).to_string();
    cluster.kq(&[
        "-n",
        "argocd",
        "patch",
        "application",
        "local-root",
        "--type",
        "merge",
        "-p",
        &patch,
    ])?;
    let work = TempDir::new("preview")?;
    let kubeconfig = work.0.join("kubeconfig");
    fs::write(
        &kubeconfig,
        cluster.kout(&["config", "view", "--minify", "--flatten"])?,
    )?;
    let path = kubeconfig.to_string_lossy().into_owned();
    exec::output(
        "kubectl",
        &[
            "--kubeconfig",
            &path,
            "config",
            "set-context",
            "--current",
            "--namespace",
            "argocd",
        ],
    )?;
    exec::status(
        "argocd",
        &[
            "--core",
            "app",
            "wait",
            "-l",
            "env=local",
            "--sync",
            "--health",
            "--timeout",
            "900",
        ],
        &[("KUBECONFIG", &path)],
    )?;
    ok(&format!(
        "preview pr-{pr} is up at https://{}:8443/",
        cluster.domain
    ));
    detail("it holds no real data and no real credential, and nothing is released from it");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_port_from_the_registry() {
        let text = "__LOCAL_PORTS=\"\nanalytics:8086\norders:8082\n\"\nservice_port() { x; }\n";
        assert_eq!(parse_port(text, "orders").as_deref(), Some("8082"));
        assert_eq!(parse_port(text, "payment"), None);
    }

    #[test]
    fn reads_the_declared_recipes_and_skips_comments() {
        let text = "# dep-comment\nserver: env\n    just --justfile ../../justfile dep-postgres dep-openfga svc-catalog\nworker:\n  just dep-temporal svc-payment\n";
        assert_eq!(
            declared(text, "dep-").into_iter().collect::<Vec<_>>(),
            vec!["openfga", "postgres", "temporal"]
        );
        assert_eq!(
            declared(text, "svc-").into_iter().collect::<Vec<_>>(),
            vec!["catalog", "payment"]
        );
    }

    #[test]
    fn a_worker_needs_a_block_after_its_key() {
        assert!(worker_enabled("image: x\nworker:\n  enabled: true\n"));
        assert!(!worker_enabled(
            "server:\n  enabled: true\nworker:\n  enabled: false\n"
        ));
        assert!(!worker_enabled("server:\n  enabled: true\n"));
    }

    #[test]
    fn the_rule_matches_each_resource() {
        assert_eq!(
            match_rule(&["orders", "carts"]),
            "PathPrefix(`/api/orders`) || PathPrefix(`/api/carts`)"
        );
    }

    #[test]
    fn the_origin_drops_the_repository_path() {
        assert_eq!(
            forge_origin("https://github.com/acme/app.git"),
            "https://github.com/"
        );
    }
}
