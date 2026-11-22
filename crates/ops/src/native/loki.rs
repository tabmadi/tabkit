//! One structured line to Loki from outside the cluster.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use super::{detail, exec, warn};

/// Pushes one line. A failed or unset push warns, because the record never decides a build.
pub fn push(stream: &Value, line: &Value, work: &Path) -> Result<()> {
    let Some(url) = std::env::var("LOKI_PUSH_URL")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        warn(
            "LOKI_PUSH_URL unset: supply-chain record not filed. This build is still signed and attested",
        );
        return Ok(());
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let payload = json!({ "streams": [{ "stream": stream, "values": [[nanos.to_string(), line.to_string()]] }] });
    let body = work.join("record.json");
    fs::write(&body, payload.to_string())?;
    let mut config = String::from("header = \"Content-Type: application/json\"\n");
    for (variable, header) in [
        ("LOKI_PUSH_TENANT", "X-Scope-OrgID"),
        ("LOKI_PUSH_AUTH", "Authorization"),
    ] {
        if let Some(value) = std::env::var(variable).ok().filter(|v| !v.is_empty()) {
            config.push_str("header = ");
            config.push_str(&exec::curl_quote(&format!("{header}: {value}")));
            config.push('\n');
        }
    }
    let target = format!("{}/loki/api/v1/push", url.trim_end_matches('/'));
    let data = format!("@{}", body.display());
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
