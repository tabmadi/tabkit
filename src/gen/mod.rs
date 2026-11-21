//! The generators. Each one reads the repository and returns whole files, so `--check` compares bytes.

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
}

/// Every generator. `tabrules gen --list` prints this table.
pub const GENERATORS: &[Generator] = &[];

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
        for output in (generator.run)(repo, cfg).with_context(|| generator.id)? {
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
