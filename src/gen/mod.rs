//! The generators. Each one reads the repository and returns whole files, so `--check` compares bytes.

mod admin;
mod build_path;
mod data_classes;
mod emit;
mod rules_index;
mod shared_components;
mod zod;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::repo::Repo;

/// One file that a generator owns.
pub struct Output {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// A generator with a stable ID of the form `gen/<name>`. It returns no output when its input does not exist.
pub struct Generator {
    pub id: &'static str,
    pub what: &'static str,
    pub run: fn(&Repo, &Config) -> Result<Vec<Output>>,
    /// The directory whose files this generator owns. A file there that it no longer writes is stale.
    pub owned: fn(&Config) -> Result<Option<String>>,
}

/// For a generator that owns no directory.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every generator has this signature"
)]
fn owns_nothing(_: &Config) -> Result<Option<String>> {
    Ok(None)
}

/// Every generator. `tabrules gen --list` prints this table.
pub const GENERATORS: &[Generator] = &[
    Generator {
        id: "gen/rules-index",
        what: "The index of every ADR rule, and the security baseline",
        run: rules_index::run,
        owned: owns_nothing,
    },
    Generator {
        id: "gen/data-classes",
        what: "The registry of classified columns and their retention",
        run: data_classes::run,
        owned: owns_nothing,
    },
    Generator {
        id: "gen/shared-components",
        what: "The shared OpenAPI components that each spec reaches",
        run: shared_components::run,
        owned: owns_nothing,
    },
    Generator {
        id: "gen/zod",
        what: "A zod schema for each JSON request body",
        run: zod::run,
        owned: owns_nothing,
    },
    Generator {
        id: "gen/build-path",
        what: "The table of the recipes that CI, checks, and hooks reach",
        run: build_path::run,
        owned: owns_nothing,
    },
    Generator {
        id: "gen/admin",
        what: "Lowdefy admin pages from the OpenAPI specs",
        run: admin::run,
        owned: admin::owned,
    },
];

/// The generators to run: the named ones, or every enabled one when no name is given.
pub fn select<'a>(
    all: &'a [Generator],
    names: &[String],
    cfg: &Config,
) -> Result<Vec<&'a Generator>> {
    if names.is_empty() {
        return Ok(all.iter().filter(|g| cfg.enabled(g.id)).collect());
    }
    names
        .iter()
        .map(|name| {
            let id = format!("gen/{}", name.trim_start_matches("gen/"));
            match all.iter().find(|g| g.id == id) {
                Some(g) => Ok(g),
                None => bail!("no generator is named `{name}`"),
            }
        })
        .collect()
}

/// Runs the generators. With `check`, it writes nothing. It returns each file that differs, with its generator.
pub fn run(
    repo: &Repo,
    cfg: &Config,
    selected: &[&Generator],
    check: bool,
) -> Result<Vec<(String, &'static str)>> {
    let mut changed = Vec::new();
    for generator in selected {
        let outputs = (generator.run)(repo, cfg).with_context(|| generator.id)?;
        if let Some(dir) = (generator.owned)(cfg)? {
            let prefix = format!("{dir}/");
            for stale in stale_files(&repo.root.join(&dir), &prefix)? {
                if outputs.iter().any(|o| o.path == stale) {
                    continue;
                }
                if !check {
                    let path = repo.root.join(&stale);
                    std::fs::remove_file(&path)
                        .with_context(|| format!("remove {}", path.display()))?;
                }
                changed.push((stale, generator.id));
            }
        }
        for output in outputs {
            let path = repo.root.join(&output.path);
            if std::fs::read(&path).is_ok_and(|bytes| bytes == output.bytes) {
                continue;
            }
            if !check {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create {}", parent.display()))?;
                }
                std::fs::write(&path, &output.bytes)
                    .with_context(|| format!("write {}", path.display()))?;
            }
            changed.push((output.path, generator.id));
        }
    }
    changed.sort_unstable();
    Ok(changed)
}

/// Every file under a directory, as a path that starts with `prefix`. A missing directory holds none.
fn stale_files(dir: &std::path::Path, prefix: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(out);
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", dir.display()))?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            out.extend(stale_files(&entry.path(), &format!("{name}/"))?);
        } else {
            out.push(name);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(clippy::unnecessary_wraps, reason = "a generator returns a Result")]
    fn sample(_: &Repo, _: &Config) -> Result<Vec<Output>> {
        Ok(vec![Output {
            path: "out/a.txt".to_owned(),
            bytes: b"a\n".to_vec(),
        }])
    }

    const SAMPLE: &[Generator] = &[Generator {
        id: "gen/sample",
        what: "A sample",
        run: sample,
        owned: owns_nothing,
    }];

    #[test]
    fn check_writes_nothing_and_write_settles() {
        let dir = std::env::temp_dir().join(format!("tabrules-gen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seed.txt"), "x").unwrap();
        let repo = Repo::open(&dir).unwrap();
        let cfg = Config::default();
        let selected = select(SAMPLE, &[], &cfg).unwrap();

        assert_eq!(run(&repo, &cfg, &selected, true).unwrap().len(), 1);
        assert!(!dir.join("out/a.txt").exists());
        assert_eq!(run(&repo, &cfg, &selected, false).unwrap().len(), 1);
        assert_eq!(std::fs::read(dir.join("out/a.txt")).unwrap(), b"a\n");
        assert_eq!(run(&repo, &cfg, &selected, true).unwrap(), Vec::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn select_refuses_an_unknown_name_and_skips_a_disabled_one() {
        let cfg = Config {
            disable: vec!["gen".to_owned()],
            ..Config::default()
        };
        assert!(matches!(select(SAMPLE, &[], &cfg).unwrap().as_slice(), []));
        assert_eq!(
            select(SAMPLE, &["sample".to_owned()], &cfg).unwrap().len(),
            1
        );
        assert!(select(SAMPLE, &["other".to_owned()], &cfg).is_err());
    }
}
