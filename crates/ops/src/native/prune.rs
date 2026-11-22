//! Deletes first-party image tags that nothing can deploy, per the retention rule of the registry ADR of a project.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tabkit_core::yaml;

use super::{detail, exec, ok, required, warn};

const ACCEPT: &str = "Accept: application/vnd.oci.image.index.v1+json,application/vnd.oci.image.manifest.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.docker.distribution.manifest.v2+json";

#[derive(Default, Debug)]
struct Pins {
    tags: BTreeSet<String>,
    digests: BTreeSet<String>,
}

fn walk(node: &saphyr::MarkedYaml<'_>, out: &mut BTreeMap<String, Pins>) {
    let field = |name: &str| {
        yaml::pairs(node)
            .find(|(k, _)| yaml::string(k) == Some(name))
            .map(|(_, value)| yaml::text(value).unwrap_or_default())
    };
    if let (Some(repository), Some(tag)) = (field("repository"), field("tag")) {
        let pins = out.entry(repository).or_default();
        pins.tags.insert(tag);
        if let Some(digest) = field("digest").filter(|d| !d.is_empty()) {
            pins.digests.insert(digest);
        }
    }
    for child in yaml::children(node) {
        walk(child, out);
    }
}

fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            yaml_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "yaml") {
            out.push(path);
        }
    }
    Ok(())
}

fn pins(root: &Path) -> Result<BTreeMap<String, Pins>> {
    let mut files = Vec::new();
    yaml_files(root, &mut files)?;
    let mut out = BTreeMap::new();
    for file in files {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        for doc in yaml::load(&text).with_context(|| file.display().to_string())? {
            walk(&doc, &mut out);
        }
    }
    Ok(out)
}

fn is_commit(tag: &str) -> bool {
    tag.len() == 40
        && tag
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The digest that a cosign signature or attestation tag names.
fn cosign_digest(tag: &str) -> Option<String> {
    let rest = tag.strip_prefix("sha256-")?;
    let (hex, suffix) = (rest.get(..64)?, rest.get(64..)?);
    (suffix.starts_with('.')
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| format!("sha256:{hex}"))
}

/// The tags to delete and the number kept. Any tag of another shape stays. A failed digest read stops the plan.
fn plan(
    tags: &[String],
    pins: &Pins,
    recent: &BTreeSet<String>,
    digest_of: &mut dyn FnMut(&str) -> Result<String>,
) -> Result<(Vec<String>, usize)> {
    let (mut drop, mut kept) = (Vec::new(), 0);
    let mut held = BTreeSet::new();
    for tag in tags.iter().filter(|tag| is_commit(tag)) {
        let digest = digest_of(tag)?;
        if pins.tags.contains(tag) || recent.contains(tag) || pins.digests.contains(&digest) {
            held.insert(digest);
            kept += 1;
        } else {
            drop.push(tag.clone());
        }
    }
    for tag in tags {
        let Some(digest) = cosign_digest(tag) else {
            continue;
        };
        if held.contains(&digest) {
            kept += 1;
        } else {
            drop.push(tag.clone());
        }
    }
    Ok((drop, kept))
}

struct Registry {
    host: String,
    auth: String,
}

impl Registry {
    fn url(&self, path: &str) -> String {
        format!("https://{}/v2/{path}", self.host)
    }

    fn tags(&self, name: &str) -> Result<Vec<String>> {
        let (success, body) = exec::curl(
            &self.auth,
            &["-sS", "-f", &self.url(&format!("{name}/tags/list"))],
        )?;
        if !success {
            bail!("cannot list {name}'s tags on {}", self.host);
        }
        let list: serde_json::Value =
            serde_json::from_str(&body).with_context(|| format!("{name}: a bad tag list"))?;
        Ok(list["tags"]
            .as_array()
            .map(|tags| {
                tags.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn digest(&self, name: &str, tag: &str) -> Result<String> {
        let url = self.url(&format!("{name}/manifests/{tag}"));
        let (success, head) = exec::curl(&self.auth, &["-sS", "-I", "-f", "-H", ACCEPT, &url])?;
        let digest = head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("docker-content-digest")
                .then(|| value.trim().to_string())
        });
        match digest {
            Some(digest) if success && !digest.is_empty() => Ok(digest),
            _ => bail!("cannot read {name}:{tag}'s digest on {}", self.host),
        }
    }

    fn delete(&self, name: &str, tag: &str) -> Result<()> {
        let url = self.url(&format!("{name}/manifests/{tag}"));
        let (_, code) = exec::curl(
            &self.auth,
            &[
                "-sS",
                "-X",
                "DELETE",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                &url,
            ],
        )?;
        if !code.starts_with('2') {
            bail!("{} refused to delete {name}:{tag}, HTTP {code}", self.host);
        }
        Ok(())
    }
}

pub fn run() -> Result<()> {
    let prefix = required(
        "IMAGE_PREFIX",
        "IMAGE_PREFIX is unset. It is the registry and repository that the pipeline pushes to",
    )?;
    let keep = std::env::var("PRUNE_KEEP")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "10".to_string());
    keep.parse::<u32>().context("PRUNE_KEEP is not a number")?;
    let user = required("REGISTRY_USERNAME", "REGISTRY_USERNAME is unset")?;
    let password = required("REGISTRY_PASSWORD", "REGISTRY_PASSWORD is unset")?;
    let dry = std::env::var("PRUNE_DRY_RUN").is_ok_and(|v| !v.is_empty());

    let all = pins(Path::new("infra/gitops"))?;
    if all.is_empty() {
        bail!("no pinned image found under infra/gitops");
    }
    let recent: BTreeSet<String> =
        exec::output("git", &["rev-list", &format!("--max-count={keep}"), "HEAD"])?
            .lines()
            .map(str::to_string)
            .collect();
    let host = prefix.split('/').next().unwrap_or_default().to_string();
    if host == "ghcr.io" || host == "docker.io" || host.ends_with(".docker.io") {
        warn(&format!(
            "{host} does not delete tags through the registry API. Its own retention applies"
        ));
        return Ok(());
    }
    let registry = Registry {
        host: host.clone(),
        auth: format!(
            "user = {}\n",
            exec::curl_quote(&format!("{user}:{password}"))
        ),
    };
    let (mut deleted, mut kept) = (0, 0);
    let wanted = format!("{prefix}/");
    for (repository, repo_pins) in all.iter().filter(|(r, _)| r.starts_with(&wanted)) {
        let name = repository
            .split_once('/')
            .map_or(repository.as_str(), |(_, n)| n);
        let tags = registry.tags(name)?;
        let (drop, held) = plan(&tags, repo_pins, &recent, &mut |tag| {
            registry.digest(name, tag)
        })?;
        kept += held;
        for tag in drop {
            if dry {
                detail(&format!("would delete {name}:{tag}"));
            } else {
                registry.delete(name, &tag)?;
            }
            deleted += 1;
        }
    }
    let would = if dry { "would be " } else { "" };
    ok(&format!(
        "registry pruned: {deleted} tag(s) {would}deleted, {kept} kept"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(c: char) -> String {
        c.to_string().repeat(40)
    }

    fn digest(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn sig(c: char) -> String {
        format!("sha256-{}.sig", c.to_string().repeat(64))
    }

    fn run_plan(tags: &[String], pins: &Pins, recent: &[String]) -> (Vec<String>, usize) {
        let recent = recent.iter().cloned().collect();
        plan(tags, pins, &recent, &mut |tag| {
            Ok(digest(tag.chars().next().unwrap()))
        })
        .unwrap()
    }

    #[test]
    fn keeps_a_pinned_tag_a_pinned_digest_and_a_recent_commit() {
        let mut pins = Pins::default();
        pins.tags.insert(commit('a'));
        pins.digests.insert(digest('b'));
        let tags = [commit('a'), commit('b'), commit('c'), commit('d')];
        let (drop, kept) = run_plan(&tags, &pins, &[commit('c')]);
        assert_eq!(drop, vec![commit('d')]);
        assert_eq!(kept, 3);
    }

    #[test]
    fn a_signature_goes_with_its_image() {
        let mut pins = Pins::default();
        pins.tags.insert(commit('a'));
        let tags = [commit('a'), commit('d'), sig('a'), sig('d')];
        let (drop, kept) = run_plan(&tags, &pins, &[]);
        assert_eq!(drop, vec![commit('d'), sig('d')]);
        assert_eq!(kept, 2);
    }

    #[test]
    fn a_tag_of_another_shape_stays() {
        let tags = [
            "latest".to_string(),
            "v2026.11.0".to_string(),
            "sha256-short.sig".to_string(),
        ];
        let (drop, kept) = run_plan(&tags, &Pins::default(), &[]);
        assert_eq!(drop, Vec::<String>::new());
        assert_eq!(kept, 0);
    }

    #[test]
    fn a_failed_digest_read_deletes_nothing() {
        let tags = [commit('a')];
        let result = plan(&tags, &Pins::default(), &BTreeSet::new(), &mut |_| {
            bail!("no answer")
        });
        assert!(result.is_err());
    }

    #[test]
    fn reads_every_pin_of_a_values_tree() {
        let text = "a:\n  image:\n    repository: r/x\n    tag: t1\n    digest: sha256:1\nb:\n  - repository: r/x\n    tag: 7\n";
        let mut out = BTreeMap::new();
        for doc in yaml::load(text).unwrap() {
            walk(&doc, &mut out);
        }
        let pins = &out["r/x"];
        assert!(pins.tags.contains("t1") && pins.tags.contains("7"));
        assert!(pins.digests.contains("sha256:1"));
    }
}
