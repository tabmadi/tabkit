//! `comment/workaround`: a workaround names the pin it matches, or a recent check date, and how to retest it.

use std::process::Command;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use super::Ctx;
use tabkit_core::finding::Finding;

const RULE: &str = "comment/workaround";
/// How many lines after the name of a component its version may sit, as in a Chart.yaml dependency.
const PIN_WINDOW: usize = 4;
/// How many lines after the marker the `Retest:` may sit, within one three-line comment.
const RETEST_WINDOW: usize = 2;
const MAX_AGE_MONTHS: u32 = 12;

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:#|//|--|\*)\s*Workaround for\b(.*)$").expect("a built-in pattern compiles")
});
static PINNED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ (\S+) (\S+) in `?([^\s`:]+)`?:").expect("a built-in pattern compiles")
});
static CHECKED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ (\S+), checked (\d{4})-(\d{2}):").expect("a built-in pattern compiles")
});

pub fn check(ctx: &Ctx<'_>) -> Vec<Finding> {
    if !ctx.cfg.enabled(RULE) {
        return Vec::new();
    }
    let today = commit_date(ctx);
    let read = |path: &str| {
        ctx.repo
            .files
            .binary_search_by(|f| f.as_str().cmp(path))
            .ok()
            .and_then(|_| ctx.repo.read(path))
    };
    let mut out = Vec::new();
    for path in ctx
        .files()
        .filter(|f| !f.starts_with("docs/") && !f.ends_with(".md"))
    {
        if let Some(text) = ctx.repo.read(path) {
            for (line, message) in check_text(&text, &today, &read) {
                out.push(Finding::new(path, line, RULE, message));
            }
        }
    }
    out
}

/// Each failing marker of a text, as a line and a message.
fn check_text(
    text: &str,
    today: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(found) = MARKER.captures(line) else {
            continue;
        };
        let end = (i + RETEST_WINDOW + 1).min(lines.len());
        if !lines[i..end].iter().any(|l| l.contains("Retest:")) {
            out.push((
                i + 1,
                "no `Retest:` in the comment of the marker".to_owned(),
            ));
        }
        if let Some(problem) = marker_problem(&found[1], today, read) {
            out.push((i + 1, problem));
        }
    }
    out
}

fn marker_problem(
    rest: &str,
    today: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(p) = PINNED.captures(rest) {
        return pin_problem(&p[1], &p[2], &p[3], read);
    }
    if let Some(c) = CHECKED.captures(rest) {
        return age_problem(&c[1], &c[2], &c[3], today);
    }
    Some(
        "the marker is neither `Workaround for <name> <version> in <path>:` nor `Workaround for <name>, checked <YYYY-MM>:`"
            .to_owned(),
    )
}

fn pin_problem(
    name: &str,
    version: &str,
    pin_file: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let Some(content) = read(pin_file) else {
        return Some(format!(
            "{name} names {pin_file} as its pin, and git does not track that file"
        ));
    };
    let lines: Vec<&str> = content.split('\n').collect();
    let lower = name.to_lowercase();
    let mut named = false;
    for (i, line) in lines.iter().enumerate() {
        if !line.to_lowercase().contains(&lower) {
            continue;
        }
        named = true;
        let end = (i + PIN_WINDOW + 1).min(lines.len());
        if lines[i..end].iter().any(|l| l.contains(version)) {
            return None;
        }
    }
    Some(if named {
        format!("{name} is pinned to another version than {version} in {pin_file}")
    } else {
        format!("{name} does not appear in {pin_file}")
    })
}

fn age_problem(name: &str, year: &str, month: &str, today: &str) -> Option<String> {
    let parsed = year.parse::<u32>().ok().zip(month.parse::<u32>().ok());
    let Some((y, m)) = parsed.filter(|(_, m)| (1..=12).contains(m)) else {
        return Some(format!(
            "{name} has a check date that is not a month: {year}-{month}"
        ));
    };
    let expires = format!("{:04}-{m:02}-01", y + MAX_AGE_MONTHS / 12);
    (expires.as_str() < today).then(|| {
        format!(
            "{name} was checked in {year}-{month}, more than {MAX_AGE_MONTHS} months before this commit"
        )
    })
}

/// The committer date of HEAD, so one commit gets one verdict on every day. Without a commit, it is today.
fn commit_date(ctx: &Ctx<'_>) -> String {
    Command::new("git")
        .arg("-C")
        .arg(&ctx.repo.root)
        .args(["log", "-1", "--format=%cs", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(today)
}

fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil(i64::try_from(secs / 86_400).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The calendar date of a day count since 1970-01-01, in the proleptic Gregorian calendar.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &str) -> Option<String> {
        match path {
            "pins.toml" => Some("[tools]\ncosign = \"2.4.1\"\n".to_owned()),
            "Chart.yaml" => {
                Some("dependencies:\n  - name: traefik\n    version: \"37.1.0\"\n".to_owned())
            }
            _ => None,
        }
    }

    #[test]
    fn markers() {
        let cases = [
            ("# A plain comment.\nkey: value\n", 0),
            (
                "# Workaround for cosign 2.4.1 in pins.toml: it drops the tag. Retest: sign once.\n",
                0,
            ),
            (
                "# Workaround for cosign 2.4.1 in `pins.toml`: it drops the tag.\n# Retest: once.\n",
                0,
            ),
            (
                "// Workaround for traefik 37.1.0 in Chart.yaml: probes time out. Retest: once.\n",
                0,
            ),
            (
                "# Workaround for cosign 2.3.0 in pins.toml: it drops the tag. Retest: sign once.\n",
                1,
            ),
            (
                "# Workaround for zot 2.1.0 in pins.toml: it stalls. Retest: pull once.\n",
                1,
            ),
            (
                "# Workaround for zot 2.1.0 in infra/zot.yaml: it stalls. Retest: pull once.\n",
                1,
            ),
            (
                "# Workaround for cosign 2.4.1 in pins.toml: it drops the tag.\nkey: value\n",
                1,
            ),
            (
                "# Workaround for mirror.gcr.io, checked 2026-09: it stalls. Retest: pull through it.\n",
                0,
            ),
            (
                "# Workaround for mirror.gcr.io, checked 2025-09: it stalls. Retest: pull through it.\n",
                1,
            ),
            ("# Workaround for zot: it stalls. Retest: pull once.\n", 1),
            ("# This is not a workaround for zot.\n", 0),
        ];
        for (text, want) in cases {
            assert_eq!(check_text(text, "2026-10-29", &read).len(), want, "{text}");
        }
    }

    #[test]
    fn the_calendar_is_right() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_513), (2026, 3, 1));
    }
}
