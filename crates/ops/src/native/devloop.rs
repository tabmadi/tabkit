//! The commands of the local development loop.

use std::fs;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::buildx::Builder;
use super::cluster::{Cluster, sleep};
use super::project::Background;
use super::{detail, exec, ok, step, warn};

fn forward(
    cluster: &Cluster,
    namespace: &str,
    target: &str,
    ports: &str,
    quiet: bool,
) -> Result<Child> {
    let mut command = Command::new("kubectl");
    command
        .args([
            "--context",
            &cluster.ctx(),
            "-n",
            namespace,
            "port-forward",
            target,
            ports,
        ])
        .stdin(Stdio::null());
    if quiet {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    command.spawn().context("cannot run kubectl")
}

pub fn dev_forward() -> Result<()> {
    let cluster = Cluster::detect()?;
    let ns = cluster.ns.clone();
    step("forwarding deps: postgres 5432, temporal 7233 and 8233. Press Ctrl-C to stop");
    let mut children = Vec::new();
    for (target, ports) in [
        ("svc/postgres", "5432:5432"),
        ("svc/temporal", "7233:7233"),
        ("svc/temporal", "8233:8233"),
        ("svc/openfga", "18080:8080"),
    ] {
        children.push(Background(forward(&cluster, &ns, target, ports, false)?));
    }
    if cluster.kok(&["-n", "otel-agent", "get", "svc", "otel-collector"]) {
        step("observability detected: grafana 3001, faro 12347");
        children.push(Background(forward(
            &cluster,
            &ns,
            "svc/grafana",
            "3001:80",
            false,
        )?));
        children.push(Background(forward(
            &cluster,
            "otel-agent",
            "svc/otel-collector",
            "12347:8027",
            false,
        )?));
    }
    for child in &mut children {
        let _ = child.0.wait();
    }
    Ok(())
}

fn decode(cluster: &Cluster, namespace: &str, secret: &str, path: &str) -> Result<Vec<u8>> {
    let encoded = cluster.kout(&[
        "-n",
        namespace,
        "get",
        "secret",
        secret,
        "-o",
        &format!("jsonpath={{.data.{path}}}"),
    ])?;
    let mut child = Command::new("base64")
        .arg("-d")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("cannot run base64")?;
    child
        .stdin
        .take()
        .context("no standard input")?
        .write_all(encoded.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("cannot decode {secret}");
    }
    Ok(out.stdout)
}

pub fn dev_frontend() -> Result<()> {
    let cluster = Cluster::detect()?;
    let cache = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.cache", std::env::var("HOME").unwrap_or_default()));
    let dir = Path::new(&cache).join("platform-local");
    let file = dir.join("local-ca.crt");
    let mut command = Command::new("bun");
    if cluster.kok(&["-n", &cluster.ns, "get", "secret", "local-ca-tls"]) {
        fs::create_dir_all(&dir)?;
        fs::write(
            &file,
            decode(&cluster, &cluster.ns, "local-ca-tls", r"ca\.crt")?,
        )?;
        command.env("NODE_EXTRA_CA_CERTS", &file);
        detail(&format!("trusting the local CA from {}", file.display()));
    } else {
        warn(
            "local-ca-tls not found. Check that cluster-up is running. TLS calls to the edge will fail",
        );
    }
    let error = command
        .args(["run", "next", "dev", "-H", "0.0.0.0"])
        .current_dir("apps/frontend")
        .exec();
    bail!("cannot run bun: {error}");
}

pub fn mock(args: &[String]) -> Result<()> {
    const SPEC: &str = "apps/frontend/public/devportal/openapi/internal.json";
    const MANIFEST: &str = "infra/local/mock.yaml";
    let cluster = Cluster::detect()?;
    let ns = cluster.ns.clone();
    match args.first().map(String::as_str) {
        Some("start") => {
            if !Path::new(SPEC).is_file() {
                bail!("missing {SPEC}. Run `just gen` first");
            }
            step("stamping the committed projection into the mock's spec ConfigMap");
            let rendered = cluster.kout(&[
                "-n",
                &ns,
                "create",
                "configmap",
                "mock-spec",
                &format!("--from-file=internal.json={SPEC}"),
                "--dry-run=client",
                "-o",
                "yaml",
            ])?;
            super::cluster::pipe_in(
                "kubectl",
                &[
                    "--context",
                    &cluster.ctx(),
                    "-n",
                    &ns,
                    "apply",
                    "--server-side",
                    "--force-conflicts",
                    "-f",
                    "-",
                ],
                &rendered,
            )?;
            detail(&format!("{} bytes from {SPEC}", fs::metadata(SPEC)?.len()));
            step("applying the mock");
            cluster.kq(&["-n", &ns, "apply", "-f", MANIFEST])?;
            let hash: String = exec::output("sha256sum", &[SPEC])?
                .chars()
                .take(16)
                .collect();
            let patch = json!({ "spec": { "template": { "metadata": { "annotations": { "local.platform/spec-hash": hash } } } } }).to_string();
            cluster.kq(&[
                "-n",
                &ns,
                "patch",
                "deployment",
                "mock",
                "--type",
                "merge",
                "-p",
                &patch,
            ])?;
            cluster.k(&[
                "-n",
                &ns,
                "rollout",
                "status",
                "deployment/mock",
                "--timeout=120s",
            ])?;
            ok("mock serving /api at https://dev.localtest.me:8443/api/products");
            detail("it answers from the spec's examples. It has no session, no 401, and no state");
            Ok(())
        }
        Some("stop") => {
            step("removing the mock");
            cluster.kq(&["-n", &ns, "delete", "-f", MANIFEST, "--ignore-not-found"])?;
            cluster.kq(&[
                "-n",
                &ns,
                "delete",
                "configmap",
                "mock-spec",
                "--ignore-not-found",
            ])?;
            ok("mock removed. /api goes to whatever is deployed");
            Ok(())
        }
        Some("logs") => cluster.k(&["-n", &ns, "logs", "-f", "deployment/mock"]),
        _ => bail!("usage: just mock-start, mock-stop, or mock-logs"),
    }
}

/// A JSON call to a local port-forward. The body goes on standard input.
fn local_json(method: &str, url: &str, body: Option<&Value>) -> Result<String> {
    let mut args = vec![
        "-fsS",
        "-X",
        method,
        "-H",
        "Accept: application/json",
        "-H",
        "Content-Type: application/json",
        url,
    ];
    if body.is_some() {
        args.extend(["--data-binary", "@-"]);
    }
    let mut child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run curl")?;
    if let Some(body) = body {
        child
            .stdin
            .take()
            .context("no standard input")?
            .write_all(body.to_string().as_bytes())?;
    } else {
        drop(child.stdin.take());
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "{method} {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read_password(email: &str) -> Result<String> {
    if let Some(password) = std::env::var("KRATOS_PASSWORD")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Ok(password);
    }
    eprint!("password for {email}: ");
    let _ = Command::new("stty")
        .arg("-echo")
        .stdin(Stdio::inherit())
        .status();
    let mut line = String::new();
    let result = std::io::stdin().read_line(&mut line);
    let _ = Command::new("stty")
        .arg("echo")
        .stdin(Stdio::inherit())
        .status();
    eprintln!();
    result.context("cannot read the password")?;
    Ok(line.trim_end_matches('\n').to_string())
}

pub fn auth_token(args: &[String]) -> Result<()> {
    let email = args.first().context("usage: just auth-token <email>")?;
    let cluster = Cluster::detect()?;
    let password = read_password(email)?;
    let _forward = Background(forward(
        &cluster,
        "platform",
        "svc/ory-kratos-public",
        "4433:80",
        true,
    )?);
    sleep(3);
    let flow: Value = serde_json::from_str(&local_json(
        "GET",
        "http://localhost:4433/self-service/login/api",
        None,
    )?)
    .context("a bad login flow")?;
    let action = flow["ui"]["action"]
        .as_str()
        .context("the login flow has no action")?;
    let body = json!({ "method": "password", "identifier": email, "password": password });
    let answer: Value = serde_json::from_str(&local_json("POST", action, Some(&body))?)
        .context("a bad login answer")?;
    let Some(token) = answer["session_token"].as_str().filter(|t| !t.is_empty()) else {
        warn("login failed:");
        for message in answer["ui"]["messages"].as_array().into_iter().flatten() {
            eprintln!("{}", message["text"].as_str().unwrap_or_default());
        }
        bail!("no session token for {email}");
    };
    println!("{token}");
    Ok(())
}

fn is_identity(value: &str) -> bool {
    value.len() == 36
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}

pub fn privacy(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: tabkit-ops privacy <erase|export> <identity-id>";
    let [action, identity] = args else {
        bail!("{USAGE}");
    };
    if !is_identity(identity) {
        bail!("not a Kratos identity id: {identity}");
    }
    let context = std::env::var("KUBE_CONTEXT").ok().filter(|v| !v.is_empty());
    let mut prefix: Vec<&str> = Vec::new();
    if let Some(context) = &context {
        prefix.extend(["--context", context]);
    }
    prefix.extend([
        "-n",
        "platform",
        "exec",
        "deploy/temporal-admintools",
        "--",
        "temporal",
        "workflow",
    ]);
    let input = json!({ "IdentityID": identity }).to_string();
    match action.as_str() {
        "erase" => {
            let id = format!("erase-subject-{identity}");
            let mut command = prefix.clone();
            command.extend([
                "start",
                "--type",
                "EraseSubject",
                "--task-queue",
                "platform-queue",
                "--workflow-id",
                &id,
                "--id-conflict-policy",
                "UseExisting",
                "--input",
                &input,
            ]);
            exec::output("kubectl", &command)?;
            ok(&format!(
                "erasure started as {id}. Follow it in the Temporal UI"
            ));
            Ok(())
        }
        "export" => {
            let id = format!(
                "export-subject-{identity}-{}",
                exec::output("date", &["+%s"])?.trim()
            );
            let mut command = prefix.clone();
            command.extend([
                "execute",
                "--type",
                "ExportSubject",
                "--task-queue",
                "platform-queue",
                "--workflow-id",
                &id,
                "--input",
                &input,
                "--output",
                "json",
            ]);
            println!("{}", export_result(&exec::output("kubectl", &command)?));
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

/// The result of an export. Temporal can return it as a JSON string, which is decoded once.
fn export_result(output: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return "null".to_string();
    };
    match &value["result"] {
        Value::String(text) => serde_json::from_str::<Value>(text).map_or_else(
            |_| text.clone(),
            |inner| match inner {
                Value::String(s) => s,
                other => serde_json::to_string_pretty(&other).unwrap_or_default(),
            },
        ),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn set_operator(id: &str, enabled: bool) -> Result<()> {
    let url = format!("http://localhost:4434/admin/identities/{id}");
    let identity: Value =
        serde_json::from_str(&local_json("GET", &url, None)?).context("a bad identity")?;
    let mut metadata = identity["metadata_public"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    metadata.insert("operator".to_string(), json!(enabled));
    let patch = json!([{ "op": "add", "path": "/metadata_public", "value": metadata }]);
    local_json("PATCH", &url, Some(&patch)).map(|_| ())
}

/// A write of an existing tuple or a delete of an absent one is a no-op, and OpenFGA reports both as errors.
fn membership_settled(output: &str) -> bool {
    output.contains("already existed") || output.contains("did not exist")
}

pub fn ops_grant(args: &[String]) -> Result<()> {
    const API: &str = "http://localhost:18080";
    let Some(email) = args.first().filter(|a| !a.is_empty()) else {
        eprintln!("usage: just ops-grant <email> [--revoke]");
        std::process::exit(2);
    };
    let revoke = args.get(1).is_some_and(|a| a == "--revoke");
    let ns = std::env::var("NS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "platform".to_string());
    let context = std::env::var("KUBE_CONTEXT").ok().filter(|v| !v.is_empty());
    let kube = |rest: &[&str]| -> Vec<String> {
        let mut all: Vec<String> = context
            .iter()
            .flat_map(|c| ["--context".to_string(), c.clone()])
            .collect();
        all.extend(["-n".to_string(), ns.clone()]);
        all.extend(rest.iter().map(|s| (*s).to_string()));
        all
    };
    let _ = exec::succeeds(
        "pkill",
        &["-f", "kubectl.*port-forward svc/(ory-kratos-admin|openfga)"],
    );
    let spawn = |target: &str, ports: &str| -> Result<Background> {
        Ok(Background(
            Command::new("kubectl")
                .args(kube(&["port-forward", target, ports]))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("cannot run kubectl")?,
        ))
    };
    let _kratos = spawn("svc/ory-kratos-admin", "4434:80")?;
    let secret_args = kube(&[
        "get",
        "secret",
        "openfga-creds",
        "-o",
        "jsonpath={.data.preshared_key}",
    ]);
    let refs: Vec<&str> = secret_args.iter().map(String::as_str).collect();
    let encoded = exec::output("kubectl", &refs)?;
    let mut decoder = Command::new("base64")
        .arg("-d")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("cannot run base64")?;
    decoder
        .stdin
        .take()
        .context("no standard input")?
        .write_all(encoded.as_bytes())?;
    let token = String::from_utf8_lossy(&decoder.wait_with_output()?.stdout).into_owned();
    let _openfga = spawn("svc/openfga", "18080:8080")?;
    sleep(4);
    let listed: Value = serde_json::from_str(&local_json(
        "GET",
        &format!("http://localhost:4434/admin/identities?credentials_identifier={email}"),
        None,
    )?)
    .context("a bad identity list")?;
    let Some(id) = listed[0]["id"].as_str().filter(|v| !v.is_empty()) else {
        eprintln!("no Kratos identity for {email}. They must register first");
        std::process::exit(1);
    };
    set_operator(id, !revoke)?;
    let stores: Value = serde_json::from_str(&exec::output(
        "fga",
        &["store", "list", "--api-url", API, "--api-token", &token],
    )?)
    .context("a bad store list")?;
    let Some(store) = stores["stores"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["name"] == "platform"))
        .and_then(|s| s["id"].as_str())
    else {
        eprintln!("no OpenFGA store 'platform'. Check that the seed Job has run");
        std::process::exit(1);
    };
    let action = if revoke { "delete" } else { "write" };
    let user = format!("user:{id}");
    let out = Command::new("fga")
        .args([
            "tuple",
            action,
            "--store-id",
            store,
            "--api-url",
            API,
            "--api-token",
            &token,
            &user,
            "member",
            "group:operator",
        ])
        .output()
        .context("cannot run fga")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() && !membership_settled(&text) {
        bail!("{}", text.trim());
    }
    ok(&format!(
        "{} operator flag and group:operator for {email}, user:{id}",
        if revoke { "revoked" } else { "granted" }
    ));
    if !revoke {
        detail(
            "→ they must have AAL2, a second factor, enrolled. Log in again if already signed in.",
        );
    }
    Ok(())
}

const LH_NET: &str = "lighthouse";
const LH_IMAGE: &str = "lighthouse/frontend:local";
const TRAEFIK: &str = "docker.io/traefik:v3.7.10";
const PRISM: &str = "docker.io/stoplight/prism:5.15.10";

fn lighthouse_down() {
    let _ = exec::succeeds(
        "docker",
        &[
            "rm",
            "-f",
            "lighthouse-edge",
            "lighthouse-frontend",
            "lighthouse-mock",
            "lighthouse-kratos",
        ],
    );
    let _ = exec::succeeds("docker", &["network", "rm", LH_NET]);
}

fn lighthouse_up(port: &str) -> Result<()> {
    const SPEC: &str = "apps/frontend/public/devportal/openapi/internal.json";
    if !Path::new(SPEC).is_file() {
        bail!("missing {SPEC}. Run `just gen` first");
    }
    lighthouse_down();
    step("building the frontend's production image");
    let user = exec::output("id", &["-un"])?;
    let builder = Builder::ensure(&format!("ci-keep-{}", user.trim()), false)?;
    let version = format!(
        "SERVICE_VERSION={}",
        std::env::var("GIT_SHA")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "dev".to_string())
    );
    let status = builder
        .command()
        .args([
            "--progress",
            "plain",
            "--load",
            "--tag",
            LH_IMAGE,
            "--file",
            "apps/frontend/Dockerfile",
            "--build-arg",
            &version,
            ".",
        ])
        .status()?;
    if !status.success() {
        bail!("Docker Buildx failed with {status}");
    }
    step(&format!(
        "starting the stack on https://dev.localtest.me:{port}"
    ));
    let dir = std::env::current_dir()?.join("test/e2e/lighthouse");
    exec::output("docker", &["network", "create", LH_NET])?;
    for service in ["mock", "kratos"] {
        exec::output(
            "docker",
            &[
                "create",
                "--name",
                &format!("lighthouse-{service}"),
                "--network",
                LH_NET,
                "--network-alias",
                service,
                PRISM,
                "mock",
                "-h",
                "0.0.0.0",
                "-p",
                "4010",
                "--multiprocess",
                "false",
                "/spec.json",
            ],
        )?;
    }
    exec::output("docker", &["cp", SPEC, "lighthouse-mock:/spec.json"])?;
    exec::output(
        "docker",
        &[
            "cp",
            &dir.join("kratos-stub.json").to_string_lossy(),
            "lighthouse-kratos:/spec.json",
        ],
    )?;
    exec::output("docker", &["start", "lighthouse-mock", "lighthouse-kratos"])?;
    let public = format!("EDGE_PUBLIC_ORIGIN=https://dev.localtest.me:{port}");
    exec::output(
        "docker",
        &[
            "run",
            "-d",
            "--name",
            "lighthouse-frontend",
            "--network",
            LH_NET,
            "--network-alias",
            "frontend",
            "-e",
            &public,
            "-e",
            "EDGE_INTERNAL_ORIGIN=http://edge:8080",
            "-e",
            "KRATOS_PUBLIC_URL=http://kratos:4010",
            LH_IMAGE,
        ],
    )?;
    let mapping = format!("127.0.0.1:{port}:8443");
    exec::output(
        "docker",
        &[
            "create",
            "--name",
            "lighthouse-edge",
            "--network",
            LH_NET,
            "--network-alias",
            "edge",
            "-p",
            &mapping,
            TRAEFIK,
            "--entrypoints.web.address=:8080",
            "--entrypoints.websecure.address=:8443",
            "--entrypoints.websecure.http.tls=true",
            "--providers.file.filename=/edge.yaml",
        ],
    )?;
    exec::output(
        "docker",
        &[
            "cp",
            &dir.join("edge.yaml").to_string_lossy(),
            "lighthouse-edge:/edge.yaml",
        ],
    )?;
    exec::output("docker", &["start", "lighthouse-edge"])?;
    let (home, login) = (
        format!("https://127.0.0.1:{port}/"),
        format!("https://127.0.0.1:{port}/auth/self-service/login/flows?id=lighthouse-login"),
    );
    for _ in 0..60 {
        if exec::succeeds("curl", &["-ksf", "-o", "/dev/null", &home])
            && exec::succeeds("curl", &["-ksf", "-o", "/dev/null", &login])
        {
            ok(&format!("serving https://dev.localtest.me:{port}"));
            return Ok(());
        }
        sleep(1);
    }
    let _ = exec::status(
        "docker",
        &["logs", "--tail", "20", "lighthouse-frontend"],
        &[],
    );
    bail!("the stack did not answer on :{port}");
}

pub fn lighthouse_stack(args: &[String]) -> Result<()> {
    let port = std::env::var("LIGHTHOUSE_PORT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "8444".to_string());
    match args.first().map(String::as_str) {
        Some("up") => lighthouse_up(&port),
        Some("down") => {
            lighthouse_down();
            ok("the Lighthouse stack is removed");
            Ok(())
        }
        _ => bail!("usage: just lighthouse-up or lighthouse-down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_an_identity_id() {
        assert!(is_identity("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!is_identity("not-an-id"));
        assert!(!is_identity("123E4567-E89B-12D3-A456-426614174000"));
    }

    #[test]
    fn decodes_an_export_result_once() {
        assert_eq!(
            export_result(r#"{"result":"{\"a\":1}"}"#),
            "{\n  \"a\": 1\n}"
        );
        assert_eq!(export_result(r#"{"result":"plain"}"#), "plain");
        assert_eq!(export_result("not json"), "null");
    }

    #[test]
    fn a_repeated_grant_is_settled() {
        assert!(membership_settled("tuple already existed"));
        assert!(membership_settled("tuple did not exist"));
        assert!(!membership_settled("connection refused"));
    }
}
