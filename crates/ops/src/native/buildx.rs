//! The Docker container builder and the proxy settings for its build steps.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::exec;

const KEYS: [(&str, &str, &str); 3] = [
    ("HTTP_PROXY", "http_proxy", "httpProxy"),
    ("HTTPS_PROXY", "https_proxy", "httpsProxy"),
    ("NO_PROXY", "no_proxy", "noProxy"),
];

pub(super) fn docker_config() -> PathBuf {
    std::env::var_os("DOCKER_CONFIG")
        .filter(|dir| !dir.is_empty())
        .map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".docker"),
            PathBuf::from,
        )
        .join("config.json")
}

pub(super) fn read_config() -> Result<Value> {
    match fs::read(docker_config()) {
        Ok(body) => serde_json::from_slice(&body).context("invalid Docker CLI config"),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(e).context("cannot read the Docker CLI config"),
    }
}

fn driver_opt(key: &str, value: &str) -> String {
    format!(r#""env.{key}={}""#, value.replace('"', "\"\""))
}

fn has_credentials(url: &str) -> bool {
    let authority = url.split_once("://").map_or(url, |(_, rest)| rest);
    authority
        .split('/')
        .next()
        .is_some_and(|host| host.contains('@'))
}

pub(super) struct Builder {
    name: String,
    proxy: Vec<(&'static str, String)>,
}

impl Builder {
    pub(super) fn ensure(base: &str, pull_request: bool) -> Result<Self> {
        let config = read_config()?;
        let egress = super::egress::Egress::load().env();
        let mut proxy = Vec::new();
        for (upper, lower, key) in KEYS {
            let value = config["proxies"]["default"][key]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    egress
                        .iter()
                        .find(|(name, _)| name == upper)
                        .map(|(_, value)| value.clone())
                });
            if let Some(value) = value {
                proxy.extend([(upper, value.clone()), (lower, value)]);
            }
        }
        if pull_request
            && (proxy.iter().any(|(key, value)| {
                matches!(*key, "HTTP_PROXY" | "HTTPS_PROXY") && has_credentials(value)
            }) || config["proxies"].as_object().is_some_and(|entries| {
                entries.values().any(|entry| {
                    ["httpProxy", "httpsProxy", "allProxy"]
                        .iter()
                        .any(|key| entry[key].as_str().is_some_and(has_credentials))
                })
            }))
        {
            bail!("use a proxy URL without credentials for pull request builds");
        }
        let name = if proxy.is_empty() {
            base.to_owned()
        } else {
            let mut hash = DefaultHasher::new();
            proxy.hash(&mut hash);
            format!("{base}-{:016x}", hash.finish())
        };
        let args = ["buildx", "inspect", &name];
        if !exec::succeeds("docker", &args) {
            let mut command = Command::new("docker");
            command.args([
                "buildx",
                "create",
                "--name",
                &name,
                "--driver",
                "docker-container",
            ]);
            for (key, value) in &proxy {
                command.args(["--driver-opt", &driver_opt(key, value)]);
            }
            let output = command
                .stdin(Stdio::null())
                .output()
                .context("cannot run Docker Buildx")?;
            if !output.status.success() && !exec::succeeds("docker", &args) {
                if proxy.is_empty() {
                    bail!(
                        "cannot create Buildx builder: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                bail!("cannot create the Buildx builder with the configured proxy");
            }
        }
        Ok(Self { name, proxy })
    }

    pub(super) fn command(&self) -> Command {
        let mut command = Command::new("docker");
        command.args(["buildx", "build", "--builder", &self.name]);
        for (key, value) in &self.proxy {
            command.args(["--build-arg", key]).env(key, value);
        }
        command.stdin(Stdio::null());
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bypass_list_is_one_driver_option() {
        assert_eq!(
            driver_opt("NO_PROXY", "local,10.0.0.0/8"),
            r#""env.NO_PROXY=local,10.0.0.0/8""#
        );
        assert_eq!(
            driver_opt("HTTP_PROXY", r#"x"y"#),
            r#""env.HTTP_PROXY=x""y""#
        );
    }

    #[test]
    fn only_proxy_authority_can_hold_credentials() {
        assert!(has_credentials("http://user:password@proxy:3128/"));
        assert!(!has_credentials("http://proxy:3128/path@elsewhere"));
    }
}
