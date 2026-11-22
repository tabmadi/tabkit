//! Pins the values files of an environment to the digest of a commit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{detail, exec, ok, required, step, values};

const ADMIN_VALUES: &str = ".lowdefy.image";

/// The registry calls of a promotion.
pub trait Images {
    fn digest(&self, reference: &str) -> Result<String>;
    fn label(&self, target: &str, source: &str) -> Result<()>;
}

/// The registry through `docker buildx imagetools`.
pub struct Docker;

impl Images for Docker {
    fn digest(&self, reference: &str) -> Result<String> {
        let out = exec::output(
            "docker",
            &[
                "buildx",
                "imagetools",
                "inspect",
                reference,
                "--format",
                "{{json .Manifest}}",
            ],
        )?;
        let manifest: serde_json::Value =
            serde_json::from_str(&out).with_context(|| format!("no manifest for {reference}"))?;
        let digest = manifest["digest"].as_str().unwrap_or_default();
        if !is_digest(digest) {
            bail!("no manifest digest for {reference}, got: {digest:.80}");
        }
        Ok(digest.to_string())
    }

    fn label(&self, target: &str, source: &str) -> Result<()> {
        exec::status(
            "docker",
            &["buildx", "imagetools", "create", "-t", target, source],
            &[],
        )
    }
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

fn bump(images: &dyn Images, path: &Path, block: &str, sha: &str) -> Result<()> {
    let repo = values::get(&read(path)?, &format!("{block}.repository"))?;
    if repo.is_empty() {
        return Ok(());
    }
    let digest = images.digest(&format!("{repo}:{sha}"))?;
    values::set(path, &format!("{block}.tag"), sha)?;
    if !values::set(path, &format!("{block}.digest"), &digest)? {
        bail!(
            "{}: {block} has no digest key. Kyverno admits first-party images by digest only, per ADR-0113",
            path.display()
        );
    }
    let file = path.file_name().unwrap_or_default().to_string_lossy();
    detail(&format!("{file} {block} → {digest}"));
    Ok(())
}

fn names() -> Result<Vec<String>> {
    let mut out = Vec::new();
    for variable in ["SERVICES", "APPS"] {
        let raw = std::env::var(variable).unwrap_or_else(|_| "[]".to_string());
        let raw = if raw.is_empty() {
            "[]".to_string()
        } else {
            raw
        };
        let list: Vec<String> = serde_json::from_str(&raw)
            .with_context(|| format!("{variable} is not a JSON list of names"))?;
        out.extend(list);
    }
    Ok(out)
}

/// Pins dev and staging for each component that a push published.
pub fn dev_staging(images: &dyn Images) -> Result<()> {
    let sha = required("SHA", "SHA is unset, so there is nothing to promote to")?;
    let names = names()?;
    if names.is_empty() {
        ok("nothing published");
        return Ok(());
    }
    pin_dev_staging(images, Path::new("."), &names, &sha)?;
    ok(&format!("dev and staging pinned to {sha}"));
    Ok(())
}

fn pin_dev_staging(images: &dyn Images, root: &Path, names: &[String], sha: &str) -> Result<()> {
    for name in names {
        for env in ["dev", "staging"] {
            if name == "admin" {
                let path = root.join(format!("infra/gitops/platform/{env}/values.yaml"));
                bump(images, &path, ADMIN_VALUES, sha)?;
                continue;
            }
            let path = root.join(format!("infra/gitops/services/{env}/values/{name}.yaml"));
            if !path.is_file() {
                continue;
            }
            bump(images, &path, ".image", sha)?;
            bump(images, &path, ".worker.image", sha)?;
        }
    }
    Ok(())
}

fn pin_prod(images: &dyn Images, path: &Path, block: &str, sha: &str, version: &str) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let repo = values::get(&read(path)?, &format!("{block}.repository"))?;
    if repo.is_empty() {
        return Ok(());
    }
    let source = format!("{repo}:{sha}");
    let digest = images.digest(&source)?;
    for (key, value) in [("digest", digest.as_str()), ("tag", sha)] {
        if !values::set(path, &format!("{block}.{key}"), value)? {
            bail!("{}: {block} has no {key} key", path.display());
        }
    }
    images.label(&format!("{repo}:{version}"), &source)?;
    detail(&format!("{repo} → {digest}, labelled {version}"));
    Ok(())
}

/// Pins every prod values file to a release commit by digest, and labels the images with the version.
pub fn prod(images: &dyn Images) -> Result<()> {
    let sha = required("SHA", "SHA is unset")?;
    let version = required("VER", "VER is unset")?;
    step(&format!("pinning prod to {sha}"));
    pin_all_prod(images, Path::new("."), &sha, &version)?;
    ok(&format!("prod pinned to {sha}, images labelled {version}"));
    Ok(())
}

fn pin_all_prod(images: &dyn Images, root: &Path, sha: &str, version: &str) -> Result<()> {
    let dir = root.join("infra/gitops/services/prod/values");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "yaml"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    for path in files {
        pin_prod(images, &path, ".image", sha, version)?;
        pin_prod(images, &path, ".worker.image", sha, version)?;
    }
    let admin = root.join("infra/gitops/platform/prod/values.yaml");
    pin_prod(images, &admin, ADMIN_VALUES, sha, version)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;

    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SERVICE: &str = "image:\n  repository: r/orders-server\n  tag: \"old\"\n  digest: \"\"\nworker:\n  image:\n    repository: r/orders-worker\n    tag: \"old\"\n    digest: \"\"\n";

    #[derive(Default)]
    struct Fake {
        labels: RefCell<Vec<String>>,
    }

    impl Images for Fake {
        fn digest(&self, _: &str) -> Result<String> {
            Ok(DIGEST.to_string())
        }
        fn label(&self, target: &str, _: &str) -> Result<()> {
            self.labels.borrow_mut().push(target.to_string());
            Ok(())
        }
    }

    fn repo(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("tabkit-promote-{name}-{}", std::process::id()));
        for env in ["dev", "staging", "prod"] {
            let dir = root.join(format!("infra/gitops/services/{env}/values"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("orders.yaml"), SERVICE).unwrap();
        }
        root
    }

    #[test]
    fn dev_and_staging_pin_the_server_and_the_worker() {
        let root = repo("dev");
        pin_dev_staging(&Fake::default(), &root, &["orders".to_string()], "abc").unwrap();
        for env in ["dev", "staging"] {
            let text = fs::read_to_string(
                root.join(format!("infra/gitops/services/{env}/values/orders.yaml")),
            )
            .unwrap();
            assert_eq!(text.matches("tag: \"abc\"").count(), 2);
            assert_eq!(text.matches(DIGEST).count(), 2);
        }
        let prod =
            fs::read_to_string(root.join("infra/gitops/services/prod/values/orders.yaml")).unwrap();
        assert_eq!(prod, SERVICE);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_digest_key_stops_the_promotion() {
        let root = repo("nodigest");
        let path = root.join("infra/gitops/services/dev/values/orders.yaml");
        fs::write(&path, "image:\n  repository: r/x\n  tag: \"old\"\n").unwrap();
        assert!(pin_dev_staging(&Fake::default(), &root, &["orders".to_string()], "abc").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prod_pins_and_labels_each_image() {
        let root = repo("prod");
        let fake = Fake::default();
        pin_all_prod(&fake, &root, "abc", "v2026.11.0").unwrap();
        let text =
            fs::read_to_string(root.join("infra/gitops/services/prod/values/orders.yaml")).unwrap();
        assert_eq!(text.matches(DIGEST).count(), 2);
        assert_eq!(
            *fake.labels.borrow(),
            vec!["r/orders-server:v2026.11.0", "r/orders-worker:v2026.11.0"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_a_full_sha256_is_a_digest() {
        assert!(is_digest(DIGEST));
        assert!(!is_digest("sha256:abc"));
        assert!(!is_digest("Name: r/orders"));
    }
}
