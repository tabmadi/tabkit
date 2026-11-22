//! Small commands of the inner loop.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};

use super::files::under;
use super::{detail, exec, ok, step};

pub fn ci_build_context(args: &[String]) -> Result<()> {
    let Some(app) = args.first().filter(|a| !a.is_empty()) else {
        bail!("usage: just ci-build-context <app>");
    };
    println!(
        "context={}",
        if app == "admin" { "apps/admin" } else { "." }
    );
    Ok(())
}

pub fn ci_release_info() -> Result<()> {
    let reference = std::env::var("GITHUB_REF").unwrap_or_default();
    let Some(version) = reference.strip_prefix("refs/tags/") else {
        let shown = if reference.is_empty() {
            "<unset>"
        } else {
            &reference
        };
        bail!("GITHUB_REF is not a tag: {shown}");
    };
    let sha = exec::output("git", &["rev-parse", &format!("{reference}^{{}}")])?;
    println!("version={version}");
    println!("sha={}", sha.trim());
    Ok(())
}

/// Seeds `.env` in the folder of the caller. The first run stops, because `just` read the environment before it.
pub fn env_ensure() -> Result<()> {
    if Path::new(".env").exists() {
        return Ok(());
    }
    if !Path::new(".env.example").exists() {
        bail!(
            "no .env or .env.example in {}",
            std::env::current_dir()?.display()
        );
    }
    fs::copy(".env.example", ".env")?;
    step("seeded .env from .env.example");
    detail("review it, then run again. just reads .env when it loads the justfile,");
    detail("so this run has no environment yet.");
    std::process::exit(1);
}

const NO_PROXY: &str = "localhost,127.0.0.1,.localtest.me";

pub fn e2e(args: &[String]) -> Result<()> {
    let Some((script, rest)) = args.split_first() else {
        bail!("usage: just e2e, e2e-smoke, e2e-a11y, e2e-visual, or lighthouse");
    };
    std::env::set_current_dir("test/e2e").context("cannot enter test/e2e")?;
    if !Path::new("node_modules").is_dir() {
        exec::status(
            "npm",
            &["ci"],
            &[("NO_PROXY", NO_PROXY), ("no_proxy", NO_PROXY)],
        )?;
    }
    let error = Command::new("npm")
        .args(["run", script, "--"])
        .args(rest)
        .env("NO_PROXY", NO_PROXY)
        .env("no_proxy", NO_PROXY)
        .exec();
    bail!("cannot run npm: {error}");
}

/// A child process that stops when this value is dropped.
pub struct Background(pub Child);

impl Drop for Background {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn db_migrate() -> Result<()> {
    let forward = Command::new("kubectl")
        .args([
            "-n",
            "platform",
            "port-forward",
            "svc/postgres-rw",
            "5432:5432",
        ])
        .stdout(Stdio::null())
        .spawn()
        .context("cannot run kubectl")?;
    let _forward = Background(forward);
    std::thread::sleep(std::time::Duration::from_secs(2));
    for dir in under("services", "migrations") {
        let service = super::files::service_of(&dir);
        if service == "_template" {
            continue;
        }
        step(&format!("migrating {service}"));
        let url = format!("postgres://dev:dev@localhost:5432/{service}?sslmode=disable");
        let migrations = dir.to_string_lossy();
        exec::status(
            "dbmate",
            &["up"],
            &[
                ("DATABASE_URL", &url),
                ("DBMATE_MIGRATIONS_DIR", &migrations),
                ("DBMATE_NO_DUMP_SCHEMA", "true"),
            ],
        )?;
    }
    ok("migrations applied");
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A Go file without its `//go:build _template` line and the blank line after it.
fn strip_build_tag(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split_inclusive('\n');
    while let Some(line) = lines.next() {
        if line.trim_end_matches('\n') == "//go:build _template" {
            lines.next();
        } else {
            out.push_str(line);
        }
    }
    out
}

fn rewrite(dir: &Path, name: &str) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            rewrite(&path, name)?;
            continue;
        }
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let is_docker = path.file_name().is_some_and(|n| n == "Dockerfile");
        if !(is_docker || matches!(extension, "go" | "yaml" | "md" | "toml")) {
            continue;
        }
        let mut text = fs::read_to_string(&path)?;
        if extension == "go" {
            text = strip_build_tag(&text);
        }
        fs::write(&path, text.replace("_template", name))?;
    }
    Ok(())
}

pub fn new_service(args: &[String]) -> Result<()> {
    let Some(name) = args.first().filter(|a| !a.is_empty()) else {
        bail!("usage: tabkit-ops new-service <service-name>");
    };
    if name.contains('/') || name.starts_with('.') {
        bail!("{name} is not a service name");
    }
    let dest = format!("services/{name}");
    if Path::new(&dest).exists() {
        bail!("{dest} already exists");
    }
    copy_dir(Path::new("services/_template"), Path::new(&dest))?;
    rewrite(Path::new(&dest), name)?;
    let stamp = exec::output("date", &["-u", "+%Y%m%d%H%M%S"])?;
    let migrations = format!("{dest}/migrations");
    for entry in fs::read_dir(&migrations)? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with("_init.sql"))
        {
            fs::rename(&path, format!("{migrations}/{}_init.sql", stamp.trim()))?;
        }
    }
    ok(&format!("created {dest}. Next:"));
    for line in [
        "  1. Register a local port in scripts/lib/ports.sh, and set the same PORT in".to_string(),
        format!("     {dest}/justfile. It ships 80XX and fails lint until you do"),
        format!("  2. Edit {dest}/openapi.yaml to define your routes"),
        "  3. just gen".to_string(),
        format!(
            "  4. Write {dest}/requirements.md under a new prefix, per ADR-0602. Then put each"
        ),
        "     ID in the name of the test that proves it. TPL fails lint until you do".to_string(),
        "  5. Implement handlers/ and wire them in cmd/server/main.go".to_string(),
        "  6. Keep only the dep-* recipes you read, and add svc-* for every service".to_string(),
        "     you call over HTTP. An undeclared callee fails at runtime, not at startup"
            .to_string(),
        format!("  7. Add infra/gitops/services/<env>/values/{name}.yaml for every env."),
        "     The ApplicationSet generates one Argo app for each values file, so with a"
            .to_string(),
        "     missing file the service is absent from that environment, with no error".to_string(),
    ] {
        detail(&line);
    }
    println!();
    detail("Then: just lint-service-contract   # checks all of the above, per ADR-0205");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_tag_and_the_blank_line_after_it() {
        let text = "//go:build _template\n\npackage main\n";
        assert_eq!(strip_build_tag(text), "package main\n");
        assert_eq!(strip_build_tag("package main\n"), "package main\n");
    }

    #[test]
    fn a_context_is_the_repo_except_for_the_admin_app() {
        assert!(ci_build_context(&["admin".to_string()]).is_ok());
        assert!(ci_build_context(&[]).is_err());
    }
}
