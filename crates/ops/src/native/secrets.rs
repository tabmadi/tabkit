//! The keys of a project: age, cosign, commit signing, and their SOPS files.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use regex::Regex;

use super::files::TempDir;
use super::{detail, exec, ok, step, warn};

const SOPS: &str = ".sops.yaml";
const PLACEHOLDER: &str = "age1example";
const TEMPLATE_RECIPIENT: &str = "age1aapmzzzcs8qm2n9exkel8vtknqsmy9t3k9g99wpypfez8av4uaps0f9gpp";

fn key_file() -> Result<PathBuf> {
    let dir = match std::env::var("SOPS_AGE_KEY_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            PathBuf::from(std::env::var("HOME").context("HOME is unset")?).join(".config/sops/age")
        }
    };
    Ok(dir.join("keys.txt"))
}

fn private_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("cannot set the mode of {}", path.display()))
}

fn recipient(key: &Path) -> Result<String> {
    Ok(exec::output("age-keygen", &["-y", &key.to_string_lossy()])?
        .trim()
        .to_string())
}

pub fn age() -> Result<()> {
    let file = key_file()?;
    let dir = file.parent().context("a key file has a folder")?;
    fs::create_dir_all(dir)?;
    private_mode(dir, 0o700)?;
    if file.is_file() {
        step(&format!(
            "age key already exists at {}, leaving it",
            file.display()
        ));
    } else {
        step(&format!("generating age key at {}", file.display()));
        exec::output("age-keygen", &["-o", &file.to_string_lossy()])?;
        private_mode(&file, 0o600)?;
    }
    println!();
    println!(
        "Your age public key. Add it to .sops.yaml, then run 'sops updatekeys' on encrypted files:"
    );
    println!("  {}", recipient(&file)?);
    Ok(())
}

/// The text of `.sops.yaml` with the recipient of one anchor replaced. The other recipients stay.
fn set_anchor(text: &str, anchor: &str, value: &str) -> Result<String> {
    let pattern =
        Regex::new(&format!(r"(&{anchor}\s+)age1[a-z0-9]+")).context("the anchor pattern")?;
    Ok(pattern
        .replace_all(text, |c: &regex::Captures<'_>| format!("{}{value}", &c[1]))
        .into_owned())
}

pub fn bootstrap() -> Result<()> {
    if !Path::new(".env").exists() && Path::new(".env.example").exists() {
        fs::copy(".env.example", ".env")?;
        detail(".env seeded from .env.example");
    }
    if Path::new("scripts/project-init.sh").is_file() {
        ok("not initialised: the project keys are not changed. Run 'just init' first");
        return Ok(());
    }
    step("ensuring this engineer's SOPS age key");
    exec::output("tabkit-ops", &["secrets-age"])?;
    let engineer = recipient(&key_file()?)?;
    let text = fs::read_to_string(SOPS)?;
    if text.contains(PLACEHOLDER) {
        let short: String = engineer.chars().take(20).collect();
        step(&format!(
            "adopting the engineer recipient that starts with {short}"
        ));
        let new = set_anchor(&text, "eng_placeholder", &engineer)?;
        fs::write(SOPS, &new)?;
        if !new.contains(&engineer) {
            bail!(
                ".sops.yaml does not name the engineer recipient. The &eng_placeholder anchor was not found"
            );
        }
    }
    step("minting this project's local-tier age key");
    rotate_local(&[])?;
    step("re-encrypting the committed secrets to the current recipients");
    update_keys()?;
    ok("bootstrap complete. Run 'just cluster-up'");
    let public = Path::new("infra/auth/cosign/cosign.pub");
    if public.metadata().map_or(true, |m| m.len() == 0) {
        println!(
            "  Signing key: add the CI and ops-recovery age recipients to .sops.yaml, then run 'just secrets-cosign'."
        );
    }
    Ok(())
}

pub fn update_keys() -> Result<()> {
    let list = exec::output("git", &["ls-files", "-z", "--", "*.enc.yaml", "*.enc.yml"])?;
    let files: Vec<&str> = list.split('\0').filter(|f| !f.is_empty()).collect();
    if files.is_empty() {
        ok("no SOPS-managed files in this repository");
        return Ok(());
    }
    step(&format!(
        "re-keying {} encrypted file(s) to .sops.yaml",
        files.len()
    ));
    let mut failed = Vec::new();
    for file in &files {
        let out = std::process::Command::new("sops")
            .args(["updatekeys", "--yes", file])
            .output()
            .context("cannot run sops")?;
        if out.status.success() {
            detail(file);
        } else {
            failed.push(*file);
            for line in String::from_utf8_lossy(&out.stdout)
                .lines()
                .chain(String::from_utf8_lossy(&out.stderr).lines())
            {
                println!("    {line}");
            }
        }
    }
    if !failed.is_empty() {
        warn("left unchanged. A file can be re-keyed only by someone who can already decrypt it:");
        for file in &failed {
            println!("  {file}");
        }
        bail!(
            "sops updatekeys failed on {} of {} file(s)",
            failed.len(),
            files.len()
        );
    }
    ok("every SOPS file matches .sops.yaml. Commit the diff to grant or revoke access");
    Ok(())
}

pub fn rotate_local(args: &[String]) -> Result<()> {
    const KEY: &str = "infra/gitops/platform/local/age.key";
    const SECRETS: &str = "infra/gitops/platform/local/secrets";
    let force = args.first().is_some_and(|a| a == "--force");
    let key = Path::new(KEY);
    if !key.is_file() {
        bail!("{KEY} does not exist");
    }
    let current = recipient(key)?;
    if !force && current != TEMPLATE_RECIPIENT {
        let short: String = current.chars().take(20).collect();
        ok(&format!("the local age key is this project's own, {short}"));
        return Ok(());
    }
    step("minting a local age key for this project");
    let old = fs::read_to_string(key)?;
    let header: Vec<&str> = old.lines().filter(|l| l.starts_with('#')).collect();
    let work = TempDir::new("age")?;
    private_mode(&work.0, 0o700)?;
    let fresh = work.0.join("new");
    exec::output("age-keygen", &["-o", &fresh.to_string_lossy()])?;
    private_mode(&fresh, 0o600)?;
    let new_recipient = recipient(&fresh)?;
    let new_key = fs::read_to_string(&fresh)?;
    let body: Vec<&str> = new_key.lines().filter(|l| !l.starts_with('#')).collect();
    fs::write(key, format!("{}\n{}\n", header.join("\n"), body.join("\n")))?;
    private_mode(key, 0o600)?;

    step("pointing .sops.yaml's cluster_local recipient at it");
    let text = set_anchor(&fs::read_to_string(SOPS)?, "cluster_local", &new_recipient)?;
    fs::write(SOPS, &text)?;
    if !text.contains(&new_recipient) {
        bail!(
            ".sops.yaml still does not name the new recipient. The cluster_local anchor was not found"
        );
    }
    step("re-encrypting the local secrets to the new recipient");
    let mut files: Vec<PathBuf> = fs::read_dir(SECRETS)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|entry| entry.path()))
                .collect()
        })
        .unwrap_or_default();
    files.retain(|p| p.to_string_lossy().ends_with(".enc.yaml"));
    files.sort();
    if files.is_empty() {
        bail!("no encrypted files under {SECRETS}/. Expected at least one");
    }
    // sops reads every identity in the file: the old one opens the secret, and the new one is its next recipient.
    let both = work.0.join("both");
    fs::write(&both, format!("{old}{new_key}"))?;
    private_mode(&both, 0o600)?;
    let both_text = both.to_string_lossy().into_owned();
    for file in &files {
        exec::output_env(
            "sops",
            &["updatekeys", "--yes", &file.to_string_lossy()],
            &[("SOPS_AGE_KEY_FILE", &both_text)],
        )?;
        println!("  {}", file.display());
    }
    let short: String = new_recipient.chars().take(20).collect();
    ok(&format!("local age key rotated to {short}"));
    println!("  The shared template key no longer opens this project.");
    Ok(())
}

/// A 32-character passphrase from the system random source.
fn passphrase() -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(32);
    let mut source = fs::File::open("/dev/urandom")?;
    let mut byte = [0_u8; 1];
    while out.len() < 32 {
        source.read_exact(&mut byte)?;
        // Rejection keeps each character equally likely.
        if usize::from(byte[0]) < 248 {
            out.push(char::from(ALPHABET[usize::from(byte[0]) % ALPHABET.len()]));
        }
    }
    Ok(out)
}

pub fn cosign() -> Result<()> {
    const DIR: &str = "infra/auth/cosign";
    let (public, private) = (
        format!("{DIR}/cosign.pub"),
        format!("{DIR}/signing-key.enc.yaml"),
    );
    if Path::new(&public).metadata().is_ok_and(|m| m.len() > 0) || Path::new(&private).is_file() {
        bail!(
            "{DIR} already holds a key pair. A second one is a rotation. Follow docs/guide/secrets-runbook.md, which keeps both public keys through the window."
        );
    }
    if fs::read_to_string(SOPS)?.contains(PLACEHOLDER) {
        bail!(
            ".sops.yaml still has placeholder recipients. Add your real engineer and CI age keys first, with just secrets-age and then .sops.yaml. Nobody can decrypt a signing key encrypted to a placeholder."
        );
    }
    fs::create_dir_all(DIR)?;
    let work = TempDir::new("cosign")?;
    private_mode(&work.0, 0o700)?;
    let password = passphrase()?;
    step("generating the platform image-signing key pair");
    let made = std::process::Command::new("cosign")
        .arg("generate-key-pair")
        .env("COSIGN_PASSWORD", &password)
        .current_dir(&work.0)
        .stdout(std::process::Stdio::null())
        .status()
        .context("cannot run cosign")?;
    if !made.success() {
        bail!("cosign could not generate a key pair");
    }
    fs::copy(work.0.join("cosign.pub"), &public)?;
    ok(&format!(
        "public key → {public}. It is committed in clear text, and every environment's Kyverno policy names it"
    ));
    step(&format!("encrypting the private half into {private}"));
    let key = fs::read_to_string(work.0.join("cosign.key"))?;
    let plain = exec::output_env(
        "yq",
        &[
            "-n",
            r#"{"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "cosign-signing-key"}, "stringData": {"cosign.key": strenv(COSIGN_KEY), "COSIGN_PASSWORD": strenv(COSIGN_PASSWORD)}}"#,
        ],
        &[("COSIGN_KEY", &key), ("COSIGN_PASSWORD", &password)],
    )?;
    encrypt(&plain, &private, &work.0)?;
    ok(&format!("private key and passphrase → {private}"));
    println!();
    println!("Next:");
    println!("  1. Commit both halves. The public one is plaintext by design.");
    println!(
        "  2. The Kyverno image policies in infra/helm/platform/kyverno name {public} as its attestor."
    );
    println!("  3. Give CI an age recipient in .sops.yaml and its private half as a forge");
    println!("     secret, so ci-sign can decrypt this file. Nothing else ever decrypts it.");
    Ok(())
}

fn encrypt(plain: &str, target: &str, work: &Path) -> Result<()> {
    let file = work.join("plain.yaml");
    fs::write(&file, plain)?;
    private_mode(&file, 0o600)?;
    let cipher = exec::output(
        "sops",
        &[
            "--encrypt",
            "--filename-override",
            target,
            &file.to_string_lossy(),
        ],
    )?;
    fs::write(target, cipher).with_context(|| format!("cannot write {target}"))
}

fn generate_gpg(target: &str, name: &str, email: &str, work: &Path) -> Result<()> {
    let home = work.join(format!("home-{name}"));
    fs::create_dir(&home)?;
    private_mode(&home, 0o700)?;
    let home_text = home.to_string_lossy().into_owned();
    let envs = [("GNUPGHOME", home_text.as_str())];
    let identity = format!("{name} <{email}>");
    exec::output_env(
        "gpg",
        &[
            "--batch",
            "--quiet",
            "--passphrase",
            "",
            "--quick-generate-key",
            &identity,
            "ed25519",
            "sign",
            "never",
        ],
        &envs,
    )?;
    let listing = exec::output_env(
        "gpg",
        &["--batch", "--with-colons", "--list-secret-keys", email],
        &envs,
    )?;
    let fingerprint = listing
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .filter(|f| !f.is_empty())
        .with_context(|| format!("gpg made no key for {email}"))?
        .to_string();
    let private = exec::output_env(
        "gpg",
        &["--batch", "--armor", "--export-secret-keys", &fingerprint],
        &envs,
    )?;
    let plain = exec::output_env(
        "yq",
        &[
            "-n",
            r#"{"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "commit-signing-key"}, "stringData": {"private.asc": strenv(PRIVATE), "fingerprint": strenv(FP), "email": strenv(EMAIL), "name": strenv(NAME)}}"#,
        ],
        &[
            ("PRIVATE", &private),
            ("FP", &fingerprint),
            ("EMAIL", email),
            ("NAME", name),
        ],
    )?;
    encrypt(&plain, target, work)?;
    let _ = std::process::Command::new("gpgconf")
        .args(["--kill", "gpg-agent"])
        .env("GNUPGHOME", &home)
        .status();
    ok(&format!("{name} {fingerprint} → {target}"));
    Ok(())
}

pub fn gpg() -> Result<()> {
    const FORGE_KEY: &str = "infra/forge-host/signing-key.enc.yaml";
    const CI_KEY: &str = "infra/ci/commit-signing-key.enc.yaml";
    // The forge facts are Nix values, per ADR-0116 of the platform. `nix eval` reads one at a time.
    let fact = |name: &str| {
        exec::output(
            "nix",
            &[
                "eval",
                "--raw",
                "--file",
                "infra/forge-host/settings.nix",
                name,
            ],
        )
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
    };
    let (org, domain) = (fact("org"), fact("domain"));
    if org.is_empty() || domain.is_empty() {
        bail!("infra/forge-host/settings.nix names no org or domain");
    }
    if Path::new(FORGE_KEY).exists() || Path::new(CI_KEY).exists() {
        bail!(
            "a commit-signing key already exists. A second one is a rotation. Follow docs/guide/secrets-runbook.md."
        );
    }
    if fs::read_to_string(SOPS)?.contains(PLACEHOLDER) {
        bail!(
            ".sops.yaml still has placeholder recipients. Add your real engineer and CI age keys first, with just secrets-age."
        );
    }
    let work = TempDir::new("gpg")?;
    private_mode(&work.0, 0o700)?;
    step("generating the forge's signing key");
    generate_gpg(
        FORGE_KEY,
        &format!("{org}-forge"),
        &format!("forge@noreply.{domain}"),
        &work.0,
    )?;
    step("generating the signing key of the ci user");
    generate_gpg(CI_KEY, "ci", &format!("ci@noreply.{domain}"), &work.0)?;
    println!();
    println!("Next:");
    println!("  1. Commit both files. Only their values are ciphertext.");
    println!("  2. just forge-signing imports the forge key and sets the signing keys of app.ini.");
    println!(
        "  3. just forge-ci-user uploads the public key of ci. Only then turn on require_signed_commits."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_only_the_named_anchor() {
        let text = "keys:\n  - &eng_placeholder age1example\n  - &cluster_local age1other\n";
        let new = set_anchor(text, "cluster_local", "age1new").unwrap();
        assert!(new.contains("&eng_placeholder age1example"));
        assert!(new.contains("&cluster_local age1new"));
    }

    #[test]
    fn a_passphrase_has_32_safe_characters() {
        let value = passphrase().unwrap();
        assert_eq!(value.len(), 32);
        assert!(value.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(value, passphrase().unwrap());
    }
}
