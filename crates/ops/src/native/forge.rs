//! Calls to the forge API, and the git identity of a commit that CI makes.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use anyhow::{Context, Result, bail};

use super::files::TempDir;
use super::{exec, required, warn};

/// The REST API of the forge that runs the job. The token stays out of the process list.
pub struct Api {
    base: String,
    config: String,
    work: TempDir,
}

impl Api {
    pub fn from_env() -> Result<Self> {
        let token = required("GITHUB_TOKEN", "the job token is unset")?;
        let api = required("GITHUB_API_URL", "GITHUB_API_URL is unset")?;
        let repository = required("GITHUB_REPOSITORY", "GITHUB_REPOSITORY is unset")?;
        let config = format!(
            "header = {}\nheader = \"Content-Type: application/json\"\n",
            exec::curl_quote(&format!("Authorization: token {token}"))
        );
        Ok(Self {
            base: format!("{api}/repos/{repository}"),
            config,
            work: TempDir::new("forge")?,
        })
    }

    pub fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<String> {
        let url = format!("{}{path}", self.base);
        let mut args = vec!["-fsS", "-X", method, url.as_str()];
        let file = self.work.0.join("body.json");
        let data = format!("@{}", file.display());
        if let Some(body) = body {
            fs::write(&file, body.to_string())?;
            args.extend(["--data-binary", data.as_str()]);
        }
        let (success, out) = exec::curl(&self.config, &args)?;
        if !success {
            bail!("the forge refused {method} {path}");
        }
        Ok(out)
    }
}

/// The identity of a CI commit. Its temporary GnuPG home is removed on every exit path.
pub struct CiGit {
    home: Option<TempDir>,
}

impl CiGit {
    pub fn configure() -> Result<Self> {
        let server = std::env::var("GITHUB_SERVER_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "https://github.com".to_string());
        let host = server
            .split_once("://")
            .map_or(server.as_str(), |(_, h)| h)
            .to_string();
        if host == "github.com" {
            exec::output("git", &["config", "user.name", "github-actions[bot]"])?;
            exec::output(
                "git",
                &[
                    "config",
                    "user.email",
                    "41898283+github-actions[bot]@users.noreply.github.com",
                ],
            )?;
            return Ok(Self { home: None });
        }
        exec::output("git", &["config", "user.name", "ci"])?;
        exec::output(
            "git",
            &["config", "user.email", &format!("ci@noreply.{host}")],
        )?;
        Self::signing()
    }

    fn signing() -> Result<Self> {
        const KEY: &str = "infra/ci/commit-signing-key.enc.yaml";
        if std::env::var("SOPS_AGE_KEY").map_or(true, |v| v.is_empty())
            || !std::path::Path::new(KEY).is_file()
        {
            warn("no commit-signing key. The promotion commit is unsigned");
            return Ok(Self { home: None });
        }
        let home = TempDir::new("gnupg")?;
        fs::set_permissions(&home.0, fs::Permissions::from_mode(0o700))?;
        let plain = exec::output("sops", &["-d", KEY])?;
        let private = super::sign::string_data(&plain, "private.asc")?;
        let fingerprint = super::sign::string_data(&plain, "fingerprint")?;
        let file = home.0.join("private.asc");
        fs::write(&file, private)?;
        let gnupg = home.0.to_string_lossy().into_owned();
        exec::status(
            "gpg",
            &["--batch", "--quiet", "--import", &file.to_string_lossy()],
            &[("GNUPGHOME", &gnupg)],
        )?;
        fs::remove_file(&file)?;
        exec::output("git", &["config", "user.signingkey", &fingerprint])?;
        exec::output("git", &["config", "commit.gpgsign", "true"])?;
        Ok(Self { home: Some(home) })
    }

    fn envs(&self) -> Vec<(&'static str, String)> {
        self.home
            .iter()
            .map(|h| ("GNUPGHOME", h.0.to_string_lossy().into_owned()))
            .collect()
    }

    /// Runs git with the signing key in reach. A rebase signs again, so the key stays until this value drops.
    pub fn git(&self, args: &[&str]) -> Result<()> {
        let envs = self.envs();
        let pairs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        exec::status("git", args, &pairs)
    }
}

impl Drop for CiGit {
    fn drop(&mut self) {
        if let Some(home) = &self.home {
            let _ = std::process::Command::new("gpgconf")
                .args(["--kill", "gpg-agent"])
                .env("GNUPGHOME", &home.0)
                .status();
        }
    }
}

/// Whether the tracked files under a path differ from the index.
pub fn values_changed() -> Result<bool> {
    Ok(!exec::succeeds(
        "git",
        &["diff", "--quiet", "--", "infra/gitops"],
    ))
}

/// Reads a JSON value from a tool's output.
pub fn json(text: &str) -> Result<serde_json::Value> {
    serde_json::from_str(text).context("the output is not JSON")
}
