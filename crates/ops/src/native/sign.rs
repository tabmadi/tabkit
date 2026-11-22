//! Signs a published image and attaches its SBOM and provenance. The key lives only in a private temp folder.

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{detail, exec, ok, step, warn};

const PRIVATE: &str = "infra/auth/cosign/signing-key.enc.yaml";

/// A private folder that is deleted on every exit path.
struct Work(PathBuf);

impl Work {
    fn new() -> Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("tabkit-sign-{}-{}", std::process::id(), nanos()?));
        fs::create_dir(&dir).context("cannot create the private folder")?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self(dir))
    }

    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn nanos() -> Result<u128> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos())
}

fn write_private(path: &str, body: &str) -> Result<()> {
    fs::write(path, body).with_context(|| format!("cannot write {path}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn now() -> Result<String> {
    Ok(exec::output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])?
        .trim()
        .to_string())
}

fn run_url() -> String {
    format!(
        "{}/{}/actions/runs/{}",
        env_or("GITHUB_SERVER_URL", "https://github.com"),
        env_or("GITHUB_REPOSITORY", "unknown"),
        env_or("GITHUB_RUN_ID", "0")
    )
}

fn provenance(started: &str) -> serde_json::Value {
    let repo = format!(
        "{}/{}",
        env_or("GITHUB_SERVER_URL", "https://github.com"),
        env_or("GITHUB_REPOSITORY", "unknown")
    );
    let run = run_url();
    json!({
        "buildDefinition": {
            "buildType": "https://github.com/actions/runner",
            "externalParameters": { "repository": repo, "ref": env_or("GITHUB_REF", "unknown") },
            "resolvedDependencies": [{ "uri": repo, "digest": { "gitCommit": env_or("GITHUB_SHA", "unknown") } }]
        },
        "runDetails": {
            "builder": { "id": run },
            "metadata": { "invocationId": run, "startedOn": started }
        }
    })
}

/// The service name of `<registry>/<path>/<name>@sha256:<digest>`.
fn service(image: &str) -> &str {
    let name = image.rsplit('/').next().unwrap_or(image);
    name.split('@').next().unwrap_or(name)
}

/// A failed record never fails the build, because the image is already signed and attested.
fn file_record(work: &Work, image: &str) -> Result<()> {
    let Some(url) = std::env::var("LOKI_PUSH_URL")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        warn(
            "LOKI_PUSH_URL unset: supply-chain record not filed. This build is still signed and attested",
        );
        return Ok(());
    };
    let (name, digest) = image.split_once('@').context("the image has a digest")?;
    let line = json!({
        "event": "signed", "image": name, "digest": digest,
        "commit": env_or("GITHUB_SHA", "unknown"), "ref": env_or("GITHUB_REF", "unknown"),
        "run": run_url(), "signed_at": now()?, "sbom": true, "provenance": true
    });
    let stream = json!({ "service": service(image), "env": env_or("SUPPLY_CHAIN_ENV", "dev"), "job": "supply-chain-build" });
    let payload = json!({ "streams": [{ "stream": stream, "values": [[nanos()?.to_string(), line.to_string()]] }] });
    let body = work.file("record.json");
    fs::write(&body, payload.to_string())?;
    let mut config = String::from("header = \"Content-Type: application/json\"\n");
    for (variable, header) in [
        ("LOKI_PUSH_TENANT", "X-Scope-OrgID"),
        ("LOKI_PUSH_AUTH", "Authorization"),
    ] {
        if let Some(value) = std::env::var(variable).ok().filter(|v| !v.is_empty()) {
            writeln!(
                config,
                "header = {}",
                exec::curl_quote(&format!("{header}: {value}"))
            )
            .expect("a string write");
        }
    }
    let target = format!("{}/loki/api/v1/push", url.trim_end_matches('/'));
    let data = format!("@{body}");
    let args = [
        "--silent",
        "--show-error",
        "--fail",
        "--max-time",
        "20",
        "-o",
        "/dev/null",
        "-X",
        "POST",
        &target,
        "--data-binary",
        &data,
    ];
    match exec::curl(&config, &args) {
        Ok((true, _)) => detail("supply-chain record filed to Loki"),
        _ => {
            warn(
                "supply-chain record could not be filed to Loki. The build itself is not affected",
            );
        }
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let Some(image) = args.first().filter(|a| !a.is_empty()) else {
        bail!("usage: just ci-sign <image>@sha256:<digest>");
    };
    if !image.contains("@sha256:") {
        bail!("refusing to sign a tag: pass an image by digest, per ADR-0113. Got: {image}");
    }
    if !Path::new(PRIVATE).is_file() {
        bail!("no signing key at {PRIVATE}. Run 'just secrets-cosign' once, at bootstrap");
    }
    let work = Work::new()?;

    step("decrypting the signing key");
    let secret = exec::output("sops", &["-d", PRIVATE])?;
    let key = string_data(&secret, "cosign.key")?;
    let password = string_data(&secret, "COSIGN_PASSWORD")?;
    let key_file = work.file("cosign.key");
    write_private(&key_file, &format!("{}\n", key.trim_end()))?;
    let envs = [("COSIGN_PASSWORD", password.as_str())];

    step(&format!("signing {image}"));
    exec::status(
        "cosign",
        &[
            "sign",
            "--yes",
            "--tlog-upload=false",
            "--key",
            &key_file,
            image,
        ],
        &envs,
    )?;

    step("generating the SPDX SBOM");
    let sbom = work.file("sbom.spdx.json");
    exec::status("syft", &[image, "-o", &format!("spdx-json={sbom}")], &[])?;

    step("attaching the SBOM and the provenance");
    exec::status(
        "cosign",
        &[
            "attest",
            "--yes",
            "--tlog-upload=false",
            "--key",
            &key_file,
            "--type",
            "spdxjson",
            "--predicate",
            &sbom,
            image,
        ],
        &envs,
    )?;
    let started = match std::env::var("BUILD_STARTED_AT") {
        Ok(value) if !value.is_empty() => value,
        _ => now()?,
    };
    let predicate = work.file("provenance.json");
    fs::write(&predicate, provenance(&started).to_string())?;
    exec::status(
        "cosign",
        &[
            "attest",
            "--yes",
            "--tlog-upload=false",
            "--key",
            &key_file,
            "--type",
            "slsaprovenance1",
            "--predicate",
            &predicate,
            image,
        ],
        &envs,
    )?;

    step("filing the supply-chain record");
    file_record(&work, image)?;
    ok(&format!("signed, SBOM and provenance attached: {image}"));
    Ok(())
}

/// One value of `stringData`, whose key can hold a dot.
fn string_data(secret: &str, key: &str) -> Result<String> {
    let docs = tabkit_core::yaml::load(secret)?;
    let value = docs.first().and_then(|doc| {
        let data = tabkit_core::yaml::pairs(doc)
            .find(|(k, _)| tabkit_core::yaml::string(k) == Some("stringData"))?
            .1;
        tabkit_core::yaml::pairs(data)
            .find(|(k, _)| tabkit_core::yaml::string(k) == Some(key))
            .and_then(|(_, v)| tabkit_core::yaml::text(v))
    });
    match value {
        Some(value) if !value.is_empty() => Ok(value),
        _ => bail!("the signing secret has no {key}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_refused_before_any_tool_runs() {
        let error = run(&["ghcr.io/a/b:1".to_string()]).unwrap_err().to_string();
        assert!(error.contains("refusing to sign a tag"));
    }

    #[test]
    fn reads_a_key_with_a_dot() {
        let secret = "stringData:\n  cosign.key: |\n    PRIVATE\n  COSIGN_PASSWORD: pw\n";
        assert_eq!(string_data(secret, "cosign.key").unwrap().trim(), "PRIVATE");
        assert_eq!(string_data(secret, "COSIGN_PASSWORD").unwrap(), "pw");
        assert!(string_data(secret, "missing").is_err());
    }

    #[test]
    fn names_the_service_of_an_image() {
        assert_eq!(service("r.io/p/orders-server@sha256:abc"), "orders-server");
    }

    #[test]
    fn the_private_folder_is_removed() {
        let path = {
            let work = Work::new().unwrap();
            assert_eq!(
                fs::metadata(&work.0).unwrap().permissions().mode() & 0o777,
                0o700
            );
            work.0.clone()
        };
        assert!(!path.exists());
    }
}
