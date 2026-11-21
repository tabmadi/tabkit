//! tabrules: opinionated checks and generators that read a repository's files.

mod affected;
mod comments;
mod config;
mod constructs;
mod finding;
mod r#gen;
mod markdown;
mod repo;
mod rules;
mod simple;
mod yaml;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a repository and print every finding.
    Check {
        /// The repository root.
        #[arg(default_value = ".")]
        root: PathBuf,
        /// A config file other than `<root>/.config/tabrules.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Print every rule ID and what it enforces.
    Rules,
    /// Print the JSON manifest of what the diff against a base ref touches.
    Affected {
        /// The git ref to diff against, through the merge base.
        #[arg(long, default_value = "origin/master")]
        base: String,
        /// Report a global change, whatever the diff holds.
        #[arg(long)]
        all: bool,
        /// Ignore the paths that only build and check the repository.
        #[arg(long)]
        runtime: bool,
        /// The repository root.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// A config file other than `<root>/.config/tabrules.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Write the generated files, or compare them with `--check`.
    Gen {
        /// The generators to run. Without a name, every enabled generator runs.
        names: Vec<String>,
        /// Write nothing, and fail when a generated file differs.
        #[arg(long)]
        check: bool,
        /// Print every generator and what it writes.
        #[arg(long)]
        list: bool,
        /// The repository root.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// A config file other than `<root>/.config/tabrules.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Rules => {
            for (id, what) in rules::RULES {
                println!("{id:<28} {what}");
            }
            ExitCode::SUCCESS
        }
        Command::Affected {
            base,
            all,
            runtime,
            root,
            config,
        } => exit(print_affected(
            &root,
            config.as_deref(),
            &base,
            all,
            runtime,
        )),
        Command::Gen { list: true, .. } => {
            for g in r#gen::GENERATORS {
                println!("{:<28} {}", g.id, g.what);
            }
            ExitCode::SUCCESS
        }
        Command::Gen {
            names,
            check,
            root,
            config,
            ..
        } => exit(generate(&root, config.as_deref(), &names, check)),
        Command::Check { root, config } => exit(self::check(&root, config.as_deref())),
    }
}

fn exit(result: anyhow::Result<bool>) -> ExitCode {
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("✗ {e:#}");
            ExitCode::from(2)
        }
    }
}

fn print_affected(
    root: &Path,
    config: Option<&Path>,
    base: &str,
    all: bool,
    runtime: bool,
) -> anyhow::Result<bool> {
    let cfg = config::Config::load(root, config)?;
    rules::validate(&cfg)?;
    print!("{}", affected::run(root, &cfg, base, all, runtime)?);
    Ok(true)
}

/// Runs the generators, and returns false when `--check` finds a file that differs.
fn generate(
    root: &Path,
    config: Option<&Path>,
    names: &[String],
    check: bool,
) -> anyhow::Result<bool> {
    let cfg = config::Config::load(root, config)?;
    rules::validate(&cfg)?;
    let repo = repo::Repo::open(root)?;
    let selected = r#gen::select(r#gen::GENERATORS, names, &cfg)?;
    let changed = r#gen::run(&repo, &cfg, &selected, check)?;
    if check && !changed.is_empty() {
        for (path, id) in &changed {
            println!("{path}: {id}: the file differs from its generated form");
        }
        eprintln!(
            "✗ {} generated files differ. Run `tabrules gen`",
            changed.len()
        );
        return Ok(false);
    }
    for (path, id) in &changed {
        println!("→ {path}: {id}");
    }
    println!("✓ {} generators ran", selected.len());
    Ok(true)
}

/// Prints the findings, and returns whether the repository passed.
fn check(root: &Path, config: Option<&Path>) -> anyhow::Result<bool> {
    let cfg = config::Config::load(root, config)?;
    rules::validate(&cfg)?;
    let repo = repo::Repo::open(root)?;
    let found = rules::run(&repo, &cfg)?;
    if found.is_empty() {
        println!("✓ {} files follow the rules", repo.files.len());
        return Ok(true);
    }
    for f in &found {
        println!("{f}");
    }
    let files = found
        .iter()
        .map(|f| f.path.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    eprintln!(
        "✗ {} findings in {files} files. `tabrules rules` explains each rule",
        found.len()
    );
    Ok(false)
}
