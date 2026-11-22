//! Verifies the SBOM attestation of every pinned image.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::files::TempDir;
use super::{detail, exec, loki, ok, step, values, warn};

const PLACEHOLDER: &str = "0000000000000000000000000000000000000000";

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0_u32, 0);
    for byte in text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
    {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// The package count and the subject digest of the first attestation that cosign printed.
fn attestation(output: &str) -> Option<(usize, String)> {
    let envelope: Value = serde_json::from_str(output.lines().next()?).ok()?;
    let payload: Value = serde_json::from_slice(&base64(envelope["payload"].as_str()?)?).ok()?;
    let packages = payload["predicate"]["packages"]
        .as_array()
        .map_or(0, Vec::len);
    let digest = payload["subject"][0]["digest"]["sha256"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Some((packages, digest))
}

fn value_files(env_root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for env in std::fs::read_dir(env_root)? {
        let env = env?.path();
        if env.file_name().is_some_and(|n| n == "local") {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(env.join("values")) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(())
}

pub fn inventory() -> Result<()> {
    const PUBLIC: &str = "infra/auth/cosign/cosign.pub";
    if Path::new(PUBLIC).metadata().map_or(true, |m| m.len() == 0) {
        if Path::new("scripts/project-init.sh").is_file() {
            ok(
                "no signing key, and none belongs here. The template publishes no environment images",
            );
            return Ok(());
        }
        bail!("no public key at {PUBLIC}. Run 'just secrets-cosign' once, at bootstrap");
    }
    let work = TempDir::new("inventory")?;
    let mut files = Vec::new();
    value_files(Path::new("infra/gitops/services"), &mut files)?;
    if files.is_empty() {
        bail!("no committed service values found under infra/gitops/services");
    }
    let (mut total, mut verified) = (0, 0);
    for path in &files {
        let env = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(path)?;
        let (repo, tag, digest) = (
            values::get(&text, ".image.repository")?,
            values::get(&text, ".image.tag")?,
            values::get(&text, ".image.digest")?,
        );
        if repo.is_empty() {
            continue;
        }
        let reference = if !digest.is_empty() {
            format!("{repo}@{digest}")
        } else if !tag.is_empty() {
            format!("{repo}:{tag}")
        } else {
            continue;
        };
        let mut service = values::get(&text, ".name")?;
        if service.is_empty() {
            service = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
        if digest.is_empty() && tag == PLACEHOLDER {
            detail(&format!("{env}/{service}: not yet promoted"));
            continue;
        }
        total += 1;
        step(&format!("{env}/{service}: {reference}"));
        let out = Command::new("cosign")
            .args([
                "verify-attestation",
                "--key",
                PUBLIC,
                "--type",
                "spdxjson",
                "--insecure-ignore-tlog",
                &reference,
            ])
            .output()
            .context("cannot run cosign")?;
        let at = exec::output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])?
            .trim()
            .to_string();
        let parsed = out
            .status
            .success()
            .then(|| attestation(&String::from_utf8_lossy(&out.stdout)))
            .flatten();
        let line = if let Some((packages, resolved)) = parsed {
            verified += 1;
            detail(&format!("verified, {packages} packages"));
            json!({ "event": "inventory", "ref": reference, "digest": format!("sha256:{resolved}"), "sbom_verified": true, "packages": packages, "at": at })
        } else {
            warn("attestation did not verify");
            let errors = String::from_utf8_lossy(&out.stderr);
            let reason = errors.lines().take(3).collect::<Vec<_>>().join(" ");
            detail(&reason);
            json!({ "event": "inventory", "ref": reference, "sbom_verified": false, "reason": reason, "at": at })
        };
        let stream = json!({ "service": service, "env": env, "job": "supply-chain-inventory" });
        loki::push(&stream, &line, &work.0)?;
    }
    if verified != total {
        bail!(
            "inventory: {} of {total} pinned images have no verifiable SBOM",
            total - verified
        );
    }
    ok(&format!(
        "inventory: all {total} pinned images carry a verifiable SBOM"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGk").unwrap(), b"hi");
        assert_eq!(base64("a*b"), None);
    }

    #[test]
    fn reads_the_first_attestation() {
        let payload = "eyJwcmVkaWNhdGUiOnsicGFja2FnZXMiOlt7fSx7fV19LCJzdWJqZWN0IjpbeyJkaWdlc3QiOnsic2hhMjU2IjoiYWJjIn19XX0=";
        let output = format!("{{\"payload\":\"{payload}\"}}\n{{\"payload\":\"x\"}}\n");
        assert_eq!(attestation(&output).unwrap(), (2, "abc".to_string()));
        assert_eq!(attestation("not json"), None);
    }
}
