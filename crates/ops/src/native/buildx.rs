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

/// The ARG defaults of a Dockerfile, so a FROM line resolves to the image that a build pulls.
fn arg_defaults(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("ARG "))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
        .collect()
}

fn substitute(value: &str, args: &[(String, String)]) -> String {
    args.iter().fold(value.to_owned(), |out, (k, v)| {
        out.replace(&format!("${{{k}}}"), v)
            .replace(&format!("${k}"), v)
    })
}

/// The images that a Dockerfile pulls: each FROM that is not a stage, and each `COPY --from` of an image.
fn pulled_images(text: &str) -> Vec<String> {
    let args = arg_defaults(text);
    let mut stages: Vec<String> = Vec::new();
    let mut images: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let image = match words.as_slice() {
            ["FROM", rest @ ..] => {
                let rest: Vec<&&str> = rest.iter().filter(|w| !w.starts_with("--")).collect();
                if let [_, as_word, name] = rest.as_slice()
                    && as_word.eq_ignore_ascii_case("as")
                {
                    stages.push((**name).to_owned());
                }
                rest.first().map(|w| substitute(w, &args))
            }
            ["COPY", rest @ ..] => rest
                .iter()
                .find_map(|w| w.strip_prefix("--from="))
                .map(|w| substitute(w, &args)),
            _ => None,
        };
        if let Some(image) = image
            && image != "scratch"
            && !stages.contains(&image)
            && !images.contains(&image)
        {
            images.push(image);
        }
    }
    images
}

/// The Nix outputs that a Dockerfile mounts: each `from=nix-<attribute>` of a RUN mount.
fn nix_mounts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for attribute in text
        .split("from=nix-")
        .skip(1)
        .filter_map(|rest| rest.split([',', ' ']).next())
    {
        if !out.iter().any(|a| a == attribute) {
            out.push(attribute.to_owned());
        }
    }
    out
}

/// The build contexts of ADR-1300 for one Dockerfile. Each Nix mount gets its store path, so a build step fetches
/// no package. With a registry mirror, each pulled image comes from the mirror.
pub(super) fn contexts(dockerfile: &str) -> Result<Vec<String>> {
    let text =
        fs::read_to_string(dockerfile).with_context(|| format!("cannot read {dockerfile}"))?;
    let mut args = Vec::new();
    for attribute in nix_mounts(&text) {
        let path = exec::output(
            "nix",
            &[
                "build",
                "--no-link",
                "--print-out-paths",
                &format!(".#{attribute}"),
            ],
        )?;
        args.push("--build-context".to_owned());
        args.push(format!("nix-{attribute}={}", path.trim()));
    }
    if let Some(mirror) = super::egress::Egress::load().mirror {
        for image in pulled_images(&text) {
            args.push("--build-context".to_owned());
            args.push(format!(
                "{image}=docker-image://{mirror}/{}",
                super::egress::full_name(&image)
            ));
        }
    }
    Ok(args)
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
    fn a_dockerfile_lists_its_pulled_images_and_nix_mounts() {
        let text = "ARG GO_VERSION=1.27.3\nFROM golang:${GO_VERSION}-alpine AS build\n\
            RUN --mount=type=bind,from=nix-go-modules,target=/goproxy go build\n\
            FROM build AS test\nFROM gcr.io/distroless/static:nonroot\n\
            COPY --from=build /out/app /app\nCOPY --from=amacneil/dbmate:2.38.2 /dbmate /dbmate\n";
        assert_eq!(
            pulled_images(text),
            [
                "golang:1.27.3-alpine",
                "gcr.io/distroless/static:nonroot",
                "amacneil/dbmate:2.38.2"
            ]
        );
        assert_eq!(nix_mounts(text), ["go-modules"]);
    }

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
