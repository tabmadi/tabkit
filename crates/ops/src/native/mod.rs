//! The commands that moved from shell to Rust. Each one keeps its name, arguments, environment, and output.

pub mod allowlist;
pub mod exec;
pub mod promote;
pub mod prune;
pub mod sign;
pub mod values;

use anyhow::{Context, Result, bail};

/// The commands that this module runs in place of an embedded script.
pub const COMMANDS: &[&str] = &[
    "ci-prune-registry",
    "ci-sign",
    "gen-image-allowlist",
    "promote-dev-staging",
    "promote-prod",
];

/// Runs one native command from the root of the repository of the caller.
pub fn run(name: &str, args: &[String]) -> Result<()> {
    enter_root()?;
    match name {
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
