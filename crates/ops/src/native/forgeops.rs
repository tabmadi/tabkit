//! Reconciles a Forgejo instance with the committed files: protection, teams, the `ci` user, and sprints.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::files::TempDir;
use super::{detail, exec, ok, step, warn};

const CONFIG: &str = "infra/forge/branch-protection.yaml";
const TEAMS: &str = "infra/forge/teams.yaml";
const LABELS: &str = "infra/forge/labels.yaml";

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The API of the forge. Credentials go to curl on standard input.
struct Client {
    url: String,
    config: String,
    work: TempDir,
}

impl Client {
    fn token(url: &str, token: &str) -> Result<Self> {
        let config = format!(
            "header = {}\nheader = \"Content-Type: application/json\"\n",
            exec::curl_quote(&format!("Authorization: token {token}"))
        );
        Ok(Self {
            url: url.to_string(),
            config,
            work: TempDir::new("forgeapi")?,
        })
    }

    fn basic(url: &str, user: &str, password: &str) -> Result<Self> {
        let config = format!(
            "user = {}\nheader = \"Content-Type: application/json\"\n",
            exec::curl_quote(&format!("{user}:{password}"))
        );
        Ok(Self {
            url: url.to_string(),
            config,
            work: TempDir::new("forgeapi")?,
        })
    }

    fn with_body<T>(
        &self,
        body: Option<&str>,
        call: impl FnOnce(Option<&str>) -> Result<T>,
    ) -> Result<T> {
        let Some(body) = body else {
            return call(None);
        };
        let file = self.work.0.join("body.json");
        fs::write(&file, body)?;
        let data = format!("@{}", file.display());
        let result = call(Some(&data));
        fs::remove_file(&file)?;
        result
    }

    /// The HTTP status of a call.
    fn code(&self, method: &str, path: &str, body: Option<&str>) -> Result<String> {
        let url = format!("{}/api/v1/{path}", self.url);
        self.with_body(body, |data| {
            let mut args = vec![
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-X",
                method,
                url.as_str(),
            ];
            if let Some(data) = data {
                args.extend(["--data-binary", data]);
            }
            Ok(exec::curl(&self.config, &args)?.1)
        })
    }

    /// The body of a call that must succeed.
    fn fetch(&self, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
        let url = format!("{}/api/v1/{path}", self.url);
        self.with_body(body, |data| {
            let mut args = vec!["-fsS", "-X", method, url.as_str()];
            if let Some(data) = data {
                args.extend(["--data-binary", data]);
            }
            let (success, out) = exec::curl(&self.config, &args)?;
            if !success {
                bail!("the forge refused {method} {path}");
            }
            if out.trim().is_empty() {
                return Ok(Value::Null);
            }
            serde_json::from_str(&out).with_context(|| format!("{path}: a bad answer"))
        })
    }

    fn require(&self, method: &str, path: &str, body: Option<&str>, what: &str) -> Result<()> {
        let code = self.code(method, path, body)?;
        if !code.starts_with('2') {
            bail!("the forge refused {what}, HTTP {code}");
        }
        Ok(())
    }
}

fn yq_json(expression: &str, file: &str) -> Result<String> {
    exec::output("yq", &["-o=json", "-I=0", expression, file])
}

fn yq_text(expression: &str, file: &str) -> Result<String> {
    Ok(exec::output("yq", &["-r", expression, file])?
        .trim()
        .to_string())
}

pub fn apply() -> Result<()> {
    let (Some(url), Some(repo), Some(token)) = (
        variable("FORGE_URL"),
        variable("FORGE_REPO"),
        variable("FORGE_TOKEN"),
    ) else {
        warn("FORGE_URL, FORGE_REPO, or FORGE_TOKEN unset. Branch protection is not applied");
        detail(&format!("the committed rules are in {CONFIG}"));
        return Ok(());
    };
    let client = Client::token(&url, &token)?;
    let branch = yq_text(".branch", CONFIG)?;
    let body = yq_json(r#".protection + {"rule_name": .branch}"#, CONFIG)?;
    step(&format!("applying branch protection to {repo}@{branch}"));
    let mut code = client.code(
        "PATCH",
        &format!("repos/{repo}/branch_protections/{branch}"),
        Some(&body),
    )?;
    if code == "404" {
        code = client.code(
            "POST",
            &format!("repos/{repo}/branch_protections"),
            Some(&body),
        )?;
    }
    if !code.starts_with('2') {
        bail!("forge refused the branch protection, HTTP {code}");
    }
    detail(&format!(
        "{} required",
        yq_text(r#".protection.status_check_contexts | join(", ")"#, CONFIG)?
    ));
    step(&format!("applying the merge styles to {repo}"));
    let code = client.code(
        "PATCH",
        &format!("repos/{repo}"),
        Some(&yq_json(".repository", CONFIG)?),
    )?;
    if !code.starts_with('2') {
        bail!("forge refused the merge styles, HTTP {code}");
    }
    detail(&format!(
        "default merge style: {}",
        yq_text(".repository.default_merge_style", CONFIG)?
    ));
    if yq_text(".actions.approval_for_outside_collaborators", CONFIG)? == "true" {
        detail(
            "runs from an outside collaborator's fork wait for a maintainer's approval, which Forgejo does by itself",
        );
    }
    ok(&format!("branch protection applied to {repo}@{branch}"));
    if !Path::new(TEAMS).is_file() {
        return Ok(());
    }
    let org = repo.split('/').next().unwrap_or_default();
    step(&format!("applying team access to {org}"));
    let existing = client.fetch("GET", &format!("orgs/{org}/teams?limit=50"), None)?;
    for line in yq_json(".teams[]", TEAMS)?
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        let team: Value = serde_json::from_str(line).context("a bad team entry")?;
        let name = team["name"].as_str().unwrap_or_default();
        let Some(id) = existing
            .as_array()
            .and_then(|list| list.iter().find(|t| t["name"] == name))
            .and_then(|t| t["id"].as_u64())
        else {
            bail!("the forge has no team {name} in {org}");
        };
        let code = client.code("PATCH", &format!("teams/{id}"), Some(line))?;
        if !code.starts_with('2') {
            bail!("forge refused the team {name}, HTTP {code}");
        }
        detail(&format!(
            "{name}: code {}",
            team["units_map"]["repo.code"].as_str().unwrap_or("null")
        ));
    }
    ok(&format!("team access applied to {org}"));
    Ok(())
}

fn random_password() -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut source = fs::File::open("/dev/urandom")?;
    let (mut out, mut byte) = (String::with_capacity(48), [0_u8; 1]);
    while out.len() < 48 {
        source.read_exact(&mut byte)?;
        if usize::from(byte[0]) < 248 {
            out.push(char::from(ALPHABET[usize::from(byte[0]) % ALPHABET.len()]));
        }
    }
    Ok(out)
}

fn upload_signing_key(user: &Client) -> Result<()> {
    const KEY: &str = "infra/ci/commit-signing-key.enc.yaml";
    if !Path::new(KEY).is_file() {
        return Ok(());
    }
    step("uploading the commit-signing key of ci");
    let plain = exec::output("sops", &["-d", KEY])?;
    let private = super::sign::string_data(&plain, "private.asc")?;
    let fingerprint = super::sign::string_data(&plain, "fingerprint")?;
    let email = super::sign::string_data(&plain, "email")?;
    let home = TempDir::new("gnupg")?;
    fs::set_permissions(&home.0, fs::Permissions::from_mode(0o700))?;
    let home_text = home.0.to_string_lossy().into_owned();
    let file = home.0.join("private.asc");
    fs::write(&file, private)?;
    let envs = [("GNUPGHOME", home_text.as_str())];
    exec::output_env(
        "gpg",
        &["--batch", "--quiet", "--import", &file.to_string_lossy()],
        &envs,
    )?;
    let armored = exec::output_env(
        "gpg",
        &["--batch", "--armor", "--export", &fingerprint],
        &envs,
    )?;
    let _ = std::process::Command::new("gpgconf")
        .args(["--homedir", &home_text, "--kill", "gpg-agent"])
        .status();
    for (path, body, what) in [
        ("user/emails", json!({ "emails": [email] }), "the email"),
        (
            "user/gpg_keys",
            json!({ "armored_public_key": armored }),
            "the signing key",
        ),
    ] {
        let code = user.code("POST", path, Some(&body.to_string()))?;
        if !code.starts_with('2') && code != "422" {
            bail!("the forge refused {what} of ci, HTTP {code}");
        }
    }
    Ok(())
}

const USER: &str = "ci";

pub fn ci_user() -> Result<()> {
    let (Some(url), Some(repo), Some(token)) = (
        variable("FORGE_URL"),
        variable("FORGE_REPO"),
        variable("FORGE_TOKEN"),
    ) else {
        bail!("set FORGE_URL, FORGE_REPO, and FORGE_TOKEN, an admin token");
    };
    let host = url
        .split_once("://")
        .map_or(url.as_str(), |(_, h)| h)
        .to_string();
    let email = format!("{USER}@noreply.{host}");
    let admin = Client::token(&url, &token)?;
    let password = random_password()?;
    if admin.code("GET", &format!("users/{USER}"), None)? == "200" {
        step(&format!("resetting the password of {USER}"));
        let body = json!({ "login_name": USER, "source_id": 0, "password": password, "must_change_password": false, "email": email });
        admin.require(
            "PATCH",
            &format!("admin/users/{USER}"),
            Some(&body.to_string()),
            "the ci user",
        )?;
    } else {
        step(&format!("creating the {USER} user"));
        let body = json!({ "username": USER, "email": email, "password": password, "must_change_password": false, "visibility": "private" });
        admin.require(
            "POST",
            "admin/users",
            Some(&body.to_string()),
            "the ci user",
        )?;
    }
    let user = Client::basic(&url, USER, &password)?;
    let stamp = exec::output("date", &["-u", "+%Y%m%d%H%M%S"])?
        .trim()
        .to_string();
    step(&format!(
        "creating the promote and renovate tokens for {USER}"
    ));
    let make = |name: &str, scopes: Value| -> Result<String> {
        let body = json!({ "name": format!("{name}-{stamp}"), "scopes": scopes });
        let answer = user.fetch(
            "POST",
            &format!("users/{USER}/tokens"),
            Some(&body.to_string()),
        )?;
        answer["sha1"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .with_context(|| format!("the forge did not return the {name} token for {USER}"))
    };
    let promote = make("promote", json!(["write:repository", "write:issue"]))?;
    let renovate = make(
        "renovate",
        json!([
            "write:repository",
            "write:issue",
            "read:user",
            "read:organization"
        ]),
    )?;
    upload_signing_key(&user)?;
    step(&format!("giving {USER} write access to {repo}"));
    admin.require(
        "PUT",
        &format!("repos/{repo}/collaborators/{USER}"),
        Some(r#"{"permission": "write"}"#),
        "the collaborator",
    )?;
    for (name, secret) in [("PROMOTE_TOKEN", &promote), ("RENOVATE_TOKEN", &renovate)] {
        step(&format!("storing the {name} secret"));
        let body = json!({ "data": secret }).to_string();
        admin.require(
            "PUT",
            &format!("repos/{repo}/actions/secrets/{name}"),
            Some(&body),
            &format!("the {name} secret"),
        )?;
    }
    detail(&format!(
        "tokens of {stamp}. Older tokens of {USER} stay valid until you delete them in its settings"
    ));
    apply()?;
    ok(&format!(
        "{USER} pushes the promotion commits of {repo} and runs its Renovate"
    ));
    Ok(())
}

fn date(args: &[&str]) -> Result<String> {
    let mut all = vec!["-u"];
    all.extend(args);
    Ok(exec::output("date", &all)?.trim().to_string())
}

fn monday(day: &str) -> Result<String> {
    let weekday: i64 = date(&["-d", day, "+%u"])?
        .parse()
        .context("a bad weekday")?;
    date(&["-d", &format!("{day} -{} days", weekday - 1), "+%F"])
}

fn start_of(week: &str) -> Result<String> {
    let (year, number) = week.split_once("-W").context("a bad sprint name")?;
    let number: i64 = number.parse().context("a bad week number")?;
    let first = monday(&format!("{year}-01-04"))?;
    date(&["-d", &format!("{first} +{} days", (number - 1) * 7), "+%F"])
}

/// Whether a title is a sprint: `Sprint <year>-W<week>`.
fn is_sprint(title: &str) -> bool {
    let Some(rest) = title.strip_prefix("Sprint ") else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() == 8
        && &rest[4..6] == "-W"
        && rest[..4].bytes().all(|b| b.is_ascii_digit())
        && rest[6..].bytes().all(|b| b.is_ascii_digit())
}

/// The open sprints that sort before the current one, as `(id, title)`.
fn ended<'a>(milestones: &'a Value, title: &str) -> Vec<(u64, &'a str)> {
    milestones
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let name = m["title"].as_str()?;
            (m["state"] == "open" && is_sprint(name) && name < title)
                .then_some((m["id"].as_u64()?, name))
        })
        .collect()
}

/// The Monday of the sprint that holds today. The latest sprint in any repository decides the cadence.
fn sprint_start(client: &Client, org: &str, repos: &[String], today: &str) -> Result<String> {
    let mut latest = String::new();
    for repo in repos {
        let milestones = client.fetch(
            "GET",
            &format!("repos/{org}/{repo}/milestones?state=all&limit=50"),
            None,
        )?;
        for name in milestones
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["title"].as_str())
            .filter(|t| is_sprint(t))
        {
            let week = name.trim_start_matches("Sprint ");
            if week > latest.as_str() {
                latest = week.to_string();
            }
        }
    }
    if latest.is_empty() {
        return monday(today);
    }
    let mut start = start_of(&latest)?;
    let now: i64 = date(&["-d", today, "+%s"])?.parse()?;
    while date(&["-d", &format!("{start} + 14 days"), "+%s"])?.parse::<i64>()? <= now {
        start = date(&["-d", &format!("{start} + 14 days"), "+%F"])?;
    }
    Ok(start)
}

/// Creates the sprint in one repository, moves the open issues of each ended sprint into it, and closes the ended one.
fn roll_repo(client: &Client, base: &str, repo: &str, title: &str, due: &str) -> Result<()> {
    let milestones = client.fetch(
        "GET",
        &format!("{base}/milestones?state=all&limit=50"),
        None,
    )?;
    let existing = milestones
        .as_array()
        .and_then(|l| l.iter().find(|m| m["title"] == title))
        .and_then(|m| m["id"].as_u64());
    let id = if let Some(id) = existing {
        id
    } else {
        let body = json!({ "title": title, "due_on": due, "description": "A two-week sprint, per ADR-0115." });
        let made = client.fetch(
            "POST",
            &format!("{base}/milestones"),
            Some(&body.to_string()),
        )?;
        detail(&format!("{repo}: created {title}"));
        made["id"]
            .as_u64()
            .context("the forge returned no milestone id")?
    };
    for (old_id, old_title) in ended(&milestones, title) {
        let issues = client.fetch(
            "GET",
            &format!("{base}/issues?state=open&type=issues&milestones={old_id}&limit=50"),
            None,
        )?;
        let mut moved = 0;
        for number in issues
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i["number"].as_u64())
        {
            client.fetch(
                "PATCH",
                &format!("{base}/issues/{number}"),
                Some(&json!({ "milestone": id }).to_string()),
            )?;
            moved += 1;
        }
        client.fetch(
            "PATCH",
            &format!("{base}/milestones/{old_id}"),
            Some(r#"{"state": "closed"}"#),
        )?;
        detail(&format!(
            "{repo}: moved {moved} open issue(s) from {old_title}, and closed it"
        ));
    }
    Ok(())
}

pub fn sprint() -> Result<()> {
    let (Some(url), Some(token)) = (variable("FORGE_URL"), variable("FORGE_TOKEN")) else {
        bail!("FORGE_URL or FORGE_TOKEN unset. No sprint is started");
    };
    let org = match variable("FORGE_ORG") {
        Some(org) => org,
        None if Path::new(LABELS).is_file() => yq_text(".org", LABELS)?,
        None => bail!("FORGE_ORG unset, and {LABELS} does not exist"),
    };
    if org.is_empty() {
        bail!("FORGE_ORG unset, and {LABELS} names no org");
    }
    let client = Client::token(&url, &token)?;
    let today = date(&["+%F"])?;
    let listed = client.fetch("GET", &format!("orgs/{org}/repos?limit=50"), None)?;
    let repos: Vec<String> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["archived"] != true)
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    if repos.is_empty() {
        bail!("the organization {org} has no repository");
    }
    let start = sprint_start(&client, &org, &repos, &today)?;
    let title = format!("Sprint {}", date(&["-d", &start, "+%G-W%V"])?);
    let last_day = date(&["-d", &format!("{start} + 13 days"), "+%F"])?;
    let due = format!("{last_day}T23:59:59Z");
    step(&format!("starting {title}, {start} to {last_day}"));
    for repo in &repos {
        roll_repo(&client, &format!("repos/{org}/{repo}"), repo, &title, &due)?;
    }
    ok(&format!(
        "{title} is the current sprint in {} repository(s)",
        repos.len()
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_a_sprint_title() {
        assert!(is_sprint("Sprint 2026-W47"));
        for title in [
            "Sprint 2026-47",
            "Sprint 26-W47",
            "Release 2026-W47",
            "Sprint 2026-W4",
        ] {
            assert!(!is_sprint(title), "{title}");
        }
    }

    #[test]
    fn an_ended_sprint_is_open_and_earlier() {
        let list = json!([
            {"id": 1, "title": "Sprint 2026-W43", "state": "open"},
            {"id": 2, "title": "Sprint 2026-W45", "state": "closed"},
            {"id": 3, "title": "Sprint 2026-W47", "state": "open"},
            {"id": 4, "title": "Backlog", "state": "open"}
        ]);
        assert_eq!(
            ended(&list, "Sprint 2026-W47"),
            vec![(1, "Sprint 2026-W43")]
        );
    }

    #[test]
    fn the_first_week_starts_on_the_monday_of_january_4() {
        assert_eq!(start_of("2026-W01").unwrap(), "2025-12-29");
        assert_eq!(start_of("2026-W47").unwrap(), "2026-11-16");
    }
}
