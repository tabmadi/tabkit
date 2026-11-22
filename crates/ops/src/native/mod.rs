//! The commands that moved from shell to Rust. Each one keeps its name, arguments, environment, and output.

pub mod allowlist;
pub mod exec;
pub mod files;
pub mod generate;
pub mod lint;
pub mod project;
pub mod promote;
pub mod prune;
pub mod sign;
pub mod values;

use anyhow::{Context, Result, bail};

/// The commands that this module runs in place of an embedded script.
pub const COMMANDS: &[&str] = &[
    "ci-build-context",
    "ci-gen",
    "ci-prune-registry",
    "ci-release-info",
    "ci-sign",
    "db-migrate",
    "e2e",
    "env-ensure",
    "format-shell",
    "gen-admin",
    "gen-image-allowlist",
    "gen-openapi",
    "gen-openapi-public",
    "gen-sqlc",
    "lint-api-breaking",
    "lint-etcd-addresses",
    "lint-openapi",
    "lint-secrets",
    "lint-shell",
    "lint-sql",
    "new-service",
    "nix-hashes",
    "perf-rederive",
    "promote-dev-staging",
    "promote-prod",
    "test-alerts",
];

/// Runs one native command from the root of the repository of the caller.
pub fn run(name: &str, args: &[String]) -> Result<()> {
    // This command works on the folder of the caller, because a service `.env` lives there.
    if name == "env-ensure" {
        return project::env_ensure();
    }
    enter_root()?;
    match name {
        "ci-build-context" => project::ci_build_context(args),
        "ci-gen" => generate::ci_gen(),
        "ci-release-info" => project::ci_release_info(),
        "db-migrate" => project::db_migrate(),
        "e2e" => project::e2e(args),
        "format-shell" => lint::format_shell(),
        "gen-admin" => generate::gen_admin(),
        "gen-openapi" => generate::gen_openapi(),
        "gen-openapi-public" => generate::gen_openapi_public(),
        "gen-sqlc" => generate::gen_sqlc(),
        "lint-api-breaking" => lint::lint_api_breaking(),
        "lint-etcd-addresses" => lint::lint_etcd_addresses(),
        "lint-openapi" => lint::lint_openapi(),
        "lint-secrets" => lint::lint_secrets(),
        "lint-shell" => lint::lint_shell(),
        "lint-sql" => lint::lint_sql(),
        "new-service" => project::new_service(args),
        "nix-hashes" => generate::nix_hashes(),
        "perf-rederive" => generate::perf_rederive(),
        "test-alerts" => lint::test_alerts(),
        "ci-prune-registry" => prune::run(),
        "ci-sign" => sign::run(args),
        "gen-image-allowlist" => allowlist::run(args),
        "promote-dev-staging" => promote::dev_staging(&promote::Docker),
        "promote-prod" => promote::prod(&promote::Docker),
        _ => bail!("no native command `{name}`"),
    }
}

fn enter_root() -> Result<()> {
    let root = match std::env::var("TABKIT_ROOT") {
        Ok(root) if !root.is_empty() => root,
        _ => exec::output("git", &["rev-parse", "--show-toplevel"])
            .unwrap_or_else(|_| ".".to_string()),
    };
    std::env::set_current_dir(root.trim()).context("cannot enter the repository root")
}

pub fn step(message: &str) {
    println!("→ {message}");
}

pub fn ok(message: &str) {
    println!("✓ {message}");
}

pub fn detail(message: &str) {
    println!("  {message}");
}

pub fn warn(message: &str) {
    eprintln!("⚠ {message}");
}

/// The value of a required environment variable.
pub fn required(name: &str, message: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => bail!("{message}"),
    }
}
