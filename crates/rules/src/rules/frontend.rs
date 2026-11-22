//! `frontend/*`: message catalogues, direction-neutral classes, token contrast, and typed fixtures.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::Ctx;
use tabkit_core::finding::Finding;

const PARITY: &str = "frontend/message-parity";
const LOGICAL: &str = "frontend/logical-property";
const CONTRAST: &str = "frontend/contrast";
const FIXTURE: &str = "frontend/fixture";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a built-in pattern compiles")
}

pub fn check(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    let mut out = parity(ctx)?;
    out.extend(logical(ctx)?);
    out.extend(contrast(ctx)?);
    out.extend(fixtures(ctx)?);
    Ok(out)
}

/// Every catalogue holds the same key paths. The check is symmetric, so it finds drift in both directions.
fn parity(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(PARITY) {
        return Ok(Vec::new());
    }
    let Some(dir) = ctx.cfg.messages.dir.as_deref() else {
        return Ok(Vec::new());
    };
    let dir = dir.trim_end_matches('/');
    let mut catalogues: BTreeMap<&str, (&str, BTreeSet<String>)> = BTreeMap::new();
    for path in ctx.repo.files.iter().map(String::as_str) {
        let Some(name) = path
            .strip_prefix(dir)
            .and_then(|r| r.strip_prefix('/'))
            .filter(|n| !n.contains('/'))
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };
        let text = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        let tree: Value = serde_json::from_str(&text).with_context(|| format!("parse {path}"))?;
        let mut keys = BTreeSet::new();
        flatten("", &tree, &mut keys);
        catalogues.insert(name, (path, keys));
    }
    let mut locales = catalogues.iter();
    let Some((reference, (reference_path, reference_keys))) = locales.next() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (locale, (path, keys)) in locales {
        for key in reference_keys.difference(keys) {
            let message =
                format!("`{locale}.json` is missing `{key}`, which is in `{reference}.json`");
            out.push(Finding::new(path, 0, PARITY, message));
        }
        for key in keys.difference(reference_keys) {
            let message =
                format!("`{reference}.json` is missing `{key}`, which is in `{locale}.json`");
            out.push(Finding::new(reference_path, 0, PARITY, message));
        }
    }
    Ok(out)
}

fn flatten(prefix: &str, value: &Value, out: &mut BTreeSet<String>) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (key, child) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if child.is_object() {
            flatten(&path, child, out);
        } else {
            out.insert(path);
        }
    }
}

/// The physical to logical swaps. Each matches a whole utility, so `left-1/2` and `slide-in-from-right-2` stay.
static SWAPS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    [
        (r"^ml-(auto|[0-9.]+)$", "ms-$1"),
        (r"^mr-(auto|[0-9.]+)$", "me-$1"),
        (r"^pl-([0-9.]+)$", "ps-$1"),
        (r"^pr-([0-9.]+)$", "pe-$1"),
        (r"^border-l-([0-9]+)$", "border-s-$1"),
        (r"^border-r-([0-9]+)$", "border-e-$1"),
        (r"^border-l$", "border-s"),
        (r"^border-r$", "border-e"),
        (r"^rounded-l-([a-z0-9]+)$", "rounded-s-$1"),
        (r"^rounded-r-([a-z0-9]+)$", "rounded-e-$1"),
        (r"^text-left$", "text-start"),
        (r"^text-right$", "text-end"),
        (r"^left-([0-9.]+)$", "start-$1"),
        (r"^right-([0-9.]+)$", "end-$1"),
    ]
    .into_iter()
    .map(|(pattern, with)| (re(pattern), with))
    .collect()
});
static TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r#"[^\s"'`{}]+"#));
static PHYSICAL_SIDE: LazyLock<Regex> = LazyLock::new(|| re(r"side=(left|right)\]"));

fn to_logical(body: &str) -> String {
    TOKEN
        .replace_all(body, |c: &regex::Captures<'_>| logical_token(&c[0]))
        .into_owned()
}

/// Swaps the utility of one token, after its variants and its `-` or `!` modifiers.
fn logical_token(token: &str) -> String {
    let cut = variant_end(token);
    let (variants, utility) = token.split_at(cut);
    if PHYSICAL_SIDE.is_match(variants) {
        return token.to_owned();
    }
    let core = utility.trim_start_matches(['-', '!']);
    let lead = &utility[..utility.len() - core.len()];
    for (pattern, with) in SWAPS.iter() {
        if pattern.is_match(core) {
            return format!("{variants}{lead}{}", pattern.replace(core, *with));
        }
    }
    token.to_owned()
}

/// The index after the last variant colon that is outside square brackets.
fn variant_end(token: &str) -> usize {
    let (mut depth, mut end) = (0_i32, 0);
    for (i, c) in token.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ':' if depth == 0 => end = i + 1,
            _ => {}
        }
    }
    end
}

fn logical(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(LOGICAL) {
        return Ok(Vec::new());
    }
    let paths = ctx.paths(LOGICAL, "paths", &["apps/frontend/src/**/*.{ts,tsx}"])?;
    let mut out = Vec::new();
    for path in ctx.files().filter(|f| paths.matches(f)) {
        let text = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        for (i, line) in text.split('\n').enumerate() {
            let logical = to_logical(line);
            if logical != line {
                out.push(Finding::new(
                    path,
                    i + 1,
                    LOGICAL,
                    "a physical property does not mirror: use its logical form",
                ));
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Copy)]
struct Rgb(f64, f64, f64);

static DECL: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*(--[a-z0-9-_]+)\s*:\s*([^;]+);"));
static VAR: LazyLock<Regex> = LazyLock::new(|| re(r"var\(\s*(--[a-z0-9-_]+)\s*\)"));
static RGB: LazyLock<Regex> = LazyLock::new(|| re(r"rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)"));
static OKLCH: LazyLock<Regex> =
    LazyLock::new(|| re(r"oklch\(\s*([0-9.]+)%?\s+([0-9.]+)\s+([0-9.]+)"));
static HEX: LazyLock<Regex> = LazyLock::new(|| re(r"^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$"));
static DARK: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\.dark\b"));

const SURFACE_PAGE: &str = "--background";
const SURFACE_CARD: &str = "--card";
const EXTRA_PAIRS: [(&str, &str); 5] = [
    ("--foreground", SURFACE_CARD),
    ("--muted-foreground", SURFACE_PAGE),
    ("--muted-foreground", SURFACE_CARD),
    ("--destructive", SURFACE_PAGE),
    ("--destructive", SURFACE_CARD),
];
/// The only visual sign of a control or its focus, so they need 3:1 against the page.
const NON_TEXT_PAIRS: [(&str, &str); 2] = [("--ring", SURFACE_PAGE), ("--input", SURFACE_PAGE)];

fn contrast(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(CONTRAST) {
        return Ok(Vec::new());
    }
    let theme = ctx
        .cfg
        .option_str(CONTRAST, "theme")?
        .unwrap_or_else(|| "apps/frontend/src/styles/theme.css".to_owned());
    let Some(css) = ctx.repo.read(&theme) else {
        return Ok(Vec::new());
    };
    // The dark block restates only what changes, so a role it omits keeps its light value.
    let (light, dark) = DARK.find(&css).map_or_else(
        || (css.clone(), css.clone()),
        |m| (css[..m.start()].to_owned(), css.clone()),
    );
    let mut out = Vec::new();
    for (mode, text) in [("light", &light), ("dark", &dark)] {
        let tokens = parse_tokens(text);
        if tokens.is_empty() {
            let message = format!("no colour tokens in the {mode} palette");
            out.push(Finding::new(&theme, 0, CONTRAST, message));
            continue;
        }
        for problem in check_mode(mode, &tokens) {
            out.push(Finding::new(&theme, 0, CONTRAST, problem));
        }
    }
    Ok(out)
}

fn parse_tokens(css: &str) -> BTreeMap<String, String> {
    DECL.captures_iter(css)
        .map(|m| (m[1].to_owned(), m[2].trim().to_owned()))
        .collect()
}

fn check_mode(mode: &str, tokens: &BTreeMap<String, String>) -> Vec<String> {
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    pairs.insert(("--foreground".to_owned(), SURFACE_PAGE.to_owned()));
    for name in tokens.keys().filter(|n| !n.starts_with("--color-")) {
        if let Some(surface) = name.strip_suffix("-foreground")
            && surface != "-"
            && !surface.is_empty()
        {
            pairs.insert((name.clone(), surface.to_owned()));
        }
    }
    for (fg, bg) in EXTRA_PAIRS.iter().chain(&NON_TEXT_PAIRS) {
        pairs.insert(((*fg).to_owned(), (*bg).to_owned()));
    }
    let mut out = Vec::new();
    for (fg, bg) in &pairs {
        // A disabled state is exempt under SC 1.4.3.
        if fg.contains("disabled") || bg.contains("disabled") {
            continue;
        }
        let Some(fg_colour) = resolve(tokens, fg) else {
            out.push(format!("{mode}: {fg} does not resolve to a colour"));
            continue;
        };
        let Some(bg_colour) = resolve(tokens, bg) else {
            out.push(format!("{mode}: {bg} does not resolve to a colour"));
            continue;
        };
        let non_text = NON_TEXT_PAIRS.iter().any(|(f, b)| f == fg && b == bg);
        let (want, criterion) = if non_text {
            (3.0, "1.4.11 non-text")
        } else {
            (4.5, "1.4.3 normal text")
        };
        let got = ratio(fg_colour, bg_colour);
        if got < want {
            out.push(format!(
                "{mode}: {fg} on {bg} is {got:.2}:1, below {want:.1}:1, SC {criterion}"
            ));
        }
    }
    out
}

fn base_colour(name: &str) -> Option<Rgb> {
    match name {
        "--white" => Some(Rgb(1.0, 1.0, 1.0)),
        "--black" => Some(Rgb(0.0, 0.0, 0.0)),
        _ => None,
    }
}

/// Follows `var()` references to a colour. The limit stops a reference loop.
fn resolve(tokens: &BTreeMap<String, String>, name: &str) -> Option<Rgb> {
    if let Some(colour) = base_colour(name) {
        return Some(colour);
    }
    let mut value = tokens.get(name)?.clone();
    for _ in 0..16 {
        let Some(m) = VAR.captures(&value) else {
            return parse_colour(&value);
        };
        if let Some(colour) = base_colour(&m[1]) {
            return Some(colour);
        }
        let next = tokens.get(&m[1])?.trim().to_owned();
        value = next;
    }
    None
}

fn parse_colour(value: &str) -> Option<Rgb> {
    let value = value.trim();
    if let Some(m) = RGB.captures(value) {
        let channel = |i: usize| m[i].parse::<f64>().unwrap_or(0.0) / 255.0;
        return Some(Rgb(channel(1), channel(2), channel(3)));
    }
    if let Some(m) = OKLCH.captures(value) {
        let number = |i: usize| m[i].parse::<f64>().unwrap_or(0.0);
        let mut lightness = number(1);
        if value.contains('%') {
            lightness /= 100.0;
        }
        return Some(oklch_to_srgb(lightness, number(2), number(3)));
    }
    if HEX.is_match(value) {
        let digits = &value[1..];
        let full: String = if digits.len() == 3 {
            digits.chars().flat_map(|c| [c, c]).collect()
        } else {
            digits.to_owned()
        };
        let n = u32::from_str_radix(&full, 16).ok()?;
        let channel = |shift: u32| f64::from((n >> shift) & 0xff) / 255.0;
        return Some(Rgb(channel(16), channel(8), channel(0)));
    }
    None
}

/// Tailwind publishes its palette in OKLCH, and WCAG defines contrast on sRGB.
#[expect(
    clippy::suboptimal_flops,
    clippy::many_single_char_names,
    reason = "the matrix reads as published"
)]
fn oklch_to_srgb(lightness: f64, chroma: f64, hue: f64) -> Rgb {
    let rad = hue * std::f64::consts::PI / 180.0;
    let (a, b) = (chroma * rad.cos(), chroma * rad.sin());
    let lp = lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let mp = lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let sp = lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l, m, s) = (lp * lp * lp, mp * mp * mp, sp * sp * sp);
    Rgb(
        gamma(4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s),
        gamma(-1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s),
        gamma(-0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s),
    )
}

/// Encodes a linear channel and clamps a colour outside the sRGB gamut.
fn gamma(v: f64) -> f64 {
    let encoded = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    encoded.clamp(0.0, 1.0)
}

fn luminance(c: Rgb) -> f64 {
    let linear = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2)
}

fn ratio(fg: Rgb, bg: Rgb) -> f64 {
    let (a, b) = (luminance(fg), luminance(bg));
    let (high, low) = if a < b { (b, a) } else { (a, b) };
    (high + 0.05) / (low + 0.05)
}

const SDK_PREFIX: &str = "@sdks/";
const GUARD: &str = "designOnly(";
const SATISFIES: &str = " satisfies ";
const TYPE_WORDS: [&str; 20] = [
    "Array",
    "ReadonlyArray",
    "Readonly",
    "Partial",
    "Required",
    "Pick",
    "Omit",
    "Record",
    "NonNullable",
    "Exclude",
    "Extract",
    "string",
    "number",
    "boolean",
    "null",
    "undefined",
    "never",
    "unknown",
    "readonly",
    "keyof",
];

static MARKER: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^//\s*no-spec:(.*)$"));
static EXTERNAL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?m)^//\s*external:\s*\S+\s+(?:GET|PUT|POST|DELETE|PATCH)\s+/\S*\s*$"));
static MARKER_BODY: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(GET|PUT|POST|DELETE|PATCH)\s+(/\S*)\s*$"));
static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"^import\s+(?:type\s+)?(?:\{([^}]*)\}|\*\s+as\s+([A-Za-z0-9_$]+)|([A-Za-z0-9_$]+))\s+from\s+"([^"]+)""#,
    )
});
static CONST: LazyLock<Regex> = LazyLock::new(|| re(r"^export\s+const\s+([A-Za-z0-9_$]+)"));
static ALIAS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:export\s+)?type\s+([A-Za-z0-9_$]+)\s*=\s*([^;]*);?$"));
static IDENT: LazyLock<Regex> = LazyLock::new(|| re(r"[A-Za-z_$][A-Za-z0-9_$]*"));
static PATH_LINE: LazyLock<Regex> = LazyLock::new(|| re(r#"^    "(/[^"]*)": \{$"#));
static METHOD_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^        (get|put|post|delete|patch): "));
static PARAM: LazyLock<Regex> = LazyLock::new(|| re(r"\{[^}]*\}"));

fn fixtures(ctx: &Ctx<'_>) -> Result<Vec<Finding>> {
    if !ctx.cfg.enabled(FIXTURE) {
        return Ok(Vec::new());
    }
    let files = ctx.paths(FIXTURE, "paths", &["apps/frontend/src/fixtures/*.ts"])?;
    let seams = ctx.paths(FIXTURE, "seams", &["apps/frontend/src/lib/data/**/*.ts"])?;
    let sdks = ctx.paths(FIXTURE, "sdks", &["libs/ts/sdks/*/index.d.ts"])?;
    let mut endpoints = BTreeMap::new();
    for path in ctx.repo.files.iter().filter(|f| sdks.matches(f)) {
        let Some(sdk) = path.split('/').rev().nth(1).filter(|s| !s.starts_with('_')) else {
            continue;
        };
        if let Some(text) = ctx.repo.read(path) {
            for key in parse_endpoints(&text) {
                endpoints.insert(key, format!("{SDK_PREFIX}{sdk}"));
            }
        }
    }
    let mut out = Vec::new();
    let mut pending: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in ctx.repo.files.iter().filter(|f| files.matches(f)) {
        let text = ctx
            .repo
            .read(path)
            .with_context(|| format!("read {path}"))?;
        let (findings, markers) = check_file(&text, &endpoints);
        for (line, message) in findings {
            out.push(Finding::new(path, line, FIXTURE, message));
        }
        if !markers.is_empty() {
            let name = path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .trim_end_matches(".ts");
            pending.insert(name.to_owned(), markers);
        }
    }
    let seam_files: BTreeMap<&str, String> = ctx
        .repo
        .files
        .iter()
        .filter(|f| seams.matches(f) && !f.ends_with(".test.ts"))
        .filter_map(|f| Some((f.as_str(), ctx.repo.read(f)?)))
        .collect();
    for (path, message) in check_guards(&pending, &seam_files) {
        out.push(Finding::new(path, 0, FIXTURE, message));
    }
    Ok(out)
}

fn check_guards<'a>(
    pending: &BTreeMap<String, Vec<String>>,
    seams: &BTreeMap<&'a str, String>,
) -> Vec<(&'a str, String)> {
    let mut out = Vec::new();
    for (fixture, markers) in pending {
        for (path, source) in seams {
            if source.contains(&format!("\"@/fixtures/{fixture}\"")) && !source.contains(GUARD) {
                out.push((
                    *path,
                    format!(
                        "reads @/fixtures/{fixture}, which waits for {}. Call `designOnly()` before the real call",
                        markers.join(", ")
                    ),
                ));
            }
        }
    }
    out
}

/// The operations of the `paths` interface. An operation that a path lacks is written as `get?: never`.
fn parse_endpoints(dts: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut path: Option<String> = None;
    for line in dts.split('\n') {
        if let Some(m) = PATH_LINE.captures(line) {
            path = Some(m[1].to_owned());
            continue;
        }
        if line == "    };" || line.starts_with('}') {
            path = None;
            continue;
        }
        if let (Some(m), Some(p)) = (METHOD_LINE.captures(line), &path) {
            out.insert(endpoint_key(&m[1], p));
        }
    }
    out
}

fn endpoint_key(method: &str, path: &str) -> String {
    format!(
        "{} {}",
        method.to_uppercase(),
        PARAM.replace_all(path, "{}")
    )
}

/// The findings of one fixture file, and the endpoints that its no-spec markers name.
fn check_file(
    src: &str,
    endpoints: &BTreeMap<String, String>,
) -> (Vec<(usize, String)>, Vec<String>) {
    let (mut findings, mut markers) = (Vec::new(), Vec::new());
    for m in MARKER.captures_iter(src) {
        let Some(body) = MARKER_BODY.captures(&m[1]) else {
            findings.push((
                0,
                "the no-spec marker must name one endpoint, as in `// no-spec: GET /members`"
                    .to_owned(),
            ));
            continue;
        };
        let endpoint = format!("{} {}", &body[1], &body[2]);
        if let Some(sdk) = endpoints.get(&endpoint_key(&body[1], &body[2])) {
            findings.push((
                0,
                format!("{endpoint} is in {sdk} now. Use its type and remove the no-spec marker"),
            ));
        }
        markers.push(endpoint);
    }
    let local = !markers.is_empty() || EXTERNAL.is_match(src);
    findings.extend(check_exports(src, local));
    (findings, markers)
}

fn check_exports(src: &str, unspecced: bool) -> Vec<(usize, String)> {
    let masked = mask(src);
    let mut imports: BTreeMap<String, String> = BTreeMap::new();
    let mut exported = 0;
    let mut out = Vec::new();
    for (start, end) in statements(&masked) {
        let begin = end
            - masked[start..end]
                .trim_start_matches([' ', '\t', '\n'])
                .len();
        let (text, code) = (src[begin..end].trim(), masked[begin..end].trim());
        if let Some(m) = IMPORT.captures(text) {
            for name in imported_names(&m) {
                imports.insert(name, m[4].to_owned());
            }
            continue;
        }
        if let Some(alias) = ALIAS.captures(code)
            && from_sdk(&alias[2], &imports)
        {
            imports.insert(alias[1].to_owned(), SDK_PREFIX.to_owned());
        }
        if !code.starts_with("export")
            || code.starts_with("export type ")
            || code.starts_with("export interface ")
        {
            continue;
        }
        exported += 1;
        if let Some(problem) = check_export(code, text, &imports, unspecced) {
            out.push((src[..begin].matches('\n').count() + 1, problem));
        }
    }
    if exported == 0 {
        out.push((0, "exports no fixture".to_owned()));
    }
    out
}

fn check_export(
    code: &str,
    text: &str,
    imports: &BTreeMap<String, String>,
    unspecced: bool,
) -> Option<String> {
    let Some(name) = CONST.captures(code) else {
        return Some("a fixture exports only `const` values, each with `satisfies <T>`".to_owned());
    };
    let Some(at) = last_top_level(code, SATISFIES) else {
        return Some(format!("`{}` has no `satisfies <T>`", &name[1]));
    };
    let after = at + SATISFIES.len();
    if unspecced || from_sdk(&code[after..], imports) {
        return None;
    }
    let kind = text[after..].trim();
    let kind = kind.strip_suffix(';').unwrap_or(kind);
    Some(format!(
        "`{}` satisfies `{kind}`, which is not imported from `@sdks/*`",
        &name[1]
    ))
}

fn imported_names(m: &regex::Captures<'_>) -> Vec<String> {
    if let Some(name) = m.get(2).or_else(|| m.get(3)) {
        return vec![name.as_str().to_owned()];
    }
    m.get(1)
        .map_or("", |g| g.as_str())
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            let part = part.strip_prefix("type ").unwrap_or(part);
            part.split_whitespace().last().map(str::to_owned)
        })
        .collect()
}

/// Every name in the type is a TypeScript word or an SDK import, and at least one is an SDK import.
fn from_sdk(kind: &str, imports: &BTreeMap<String, String>) -> bool {
    let mut sdk = false;
    for name in IDENT.find_iter(kind).map(|m| m.as_str()) {
        if TYPE_WORDS.contains(&name) {
            continue;
        }
        if !imports.get(name).is_some_and(|i| i.starts_with(SDK_PREFIX)) {
            return false;
        }
        sdk = true;
    }
    sdk
}

/// Blanks comments and the inside of strings, and keeps every offset and newline.
fn mask(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let mut blank = |from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let end = skipped(bytes, i);
        if end == i {
            i += 1;
            continue;
        }
        if matches!(bytes[i], b'"' | b'\'' | b'`') {
            // The closing quote stays, so the scan continues after it.
            blank(i + 1, end);
            i = end + 1;
        } else {
            blank(i, end);
            i = end;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The end of the comment at `i`, the offset of the closing quote of the string at `i`, or `i`.
fn skipped(src: &[u8], i: usize) -> usize {
    let rest = &src[i..];
    if rest.starts_with(b"//") {
        return rest
            .iter()
            .position(|b| *b == b'\n')
            .map_or(src.len(), |n| i + n);
    }
    if rest.starts_with(b"/*") {
        return rest[2..]
            .windows(2)
            .position(|w| w == b"*/")
            .map_or(src.len(), |n| i + n + 4);
    }
    if matches!(src[i], b'"' | b'\'' | b'`') {
        let mut j = i + 1;
        while j < src.len() && src[j] != src[i] {
            if src[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        return j.min(src.len());
    }
    i
}

/// Splits masked source at each `;` and each blank line outside brackets.
fn statements(masked: &str) -> Vec<(usize, usize)> {
    let bytes = masked.as_bytes();
    let mut spans = Vec::new();
    let (mut depth, mut start) = (0_i32, 0);
    let mut cut = |end: usize, start: &mut usize| {
        if !masked[*start..end].trim().is_empty() {
            spans.push((*start, end));
        }
        *start = end;
    };
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => cut(i + 1, &mut start),
            b'\n' if depth == 0 && bytes.get(i + 1) == Some(&b'\n') => cut(i + 1, &mut start),
            _ => {}
        }
    }
    cut(bytes.len(), &mut start);
    spans
}

/// The offset of the last match of `word` outside brackets.
fn last_top_level(code: &str, word: &str) -> Option<usize> {
    let bytes = code.as_bytes();
    let (mut depth, mut at) = (0_i32, None);
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if depth == 0 && bytes[i..].starts_with(word.as_bytes()) => at = Some(i),
            _ => {}
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_swaps() {
        for (input, want) in [
            (
                r#"className="ml-2 pr-4 text-left""#,
                r#"className="ms-2 pe-4 text-start""#,
            ),
            (r#""-right-24 top-10""#, r#""-end-24 top-10""#),
            (
                r#""left-1/2 -translate-x-1/2""#,
                r#""left-1/2 -translate-x-1/2""#,
            ),
            (
                r#""data-[side=left]:slide-in-from-right-2""#,
                r#""data-[side=left]:slide-in-from-right-2""#,
            ),
            (
                r#""data-[side=right]:left-0 data-open:left-0""#,
                r#""data-[side=right]:left-0 data-open:start-0""#,
            ),
            (
                r#""[&_pre]:text-left **:data-[slot=x]:ml-auto""#,
                r#""[&_pre]:text-start **:data-[slot=x]:ms-auto""#,
            ),
        ] {
            assert_eq!(to_logical(input), want);
        }
    }

    const DTS: &str = "export interface paths {\n    \"/members\": {\n        parameters: {\n            query?: never;\n        };\n        get: operations[\"listMembers\"];\n        put?: never;\n        post: operations[\"addMember\"];\n    };\n    \"/members/{id}\": {\n        get?: never;\n        patch: operations[\"updateMember\"];\n    };\n}\nexport interface components {\n    schemas: {\n        get: string;\n    };\n}\n";

    #[test]
    fn endpoints_are_read_from_the_paths_interface() {
        let got: Vec<String> = parse_endpoints(DTS).into_iter().collect();
        assert_eq!(got, ["GET /members", "PATCH /members/{}", "POST /members"]);
    }

    #[test]
    fn fixture_files() {
        let endpoints: BTreeMap<String, String> = [
            ("GET /members".to_owned(), "@sdks/orgs".to_owned()),
            ("PATCH /members/{}".to_owned(), "@sdks/orgs".to_owned()),
        ]
        .into();
        let cases: &[(&str, &[&str])] = &[
            (
                "import type { components } from \"@sdks/orgs\";\n\n// A comment with satisfies in it.\nexport const members = [\n  { name: \"a; b\", id: \"x\" },\n] satisfies components[\"schemas\"][\"Member\"][];\n\nexport const page = {\n  rows: [],\n} as const satisfies Partial<components[\"schemas\"][\"MemberPage\"]>;\n",
                &[],
            ),
            (
                "import { type components as Orgs } from \"@sdks/orgs\";\nexport const one = { id: \"x\" } satisfies Orgs[\"schemas\"][\"Member\"];\n",
                &[],
            ),
            (
                "import type { components } from \"@sdks/orgs\";\n\ntype O = components[\"schemas\"];\nexport const one = { id: \"x\" } satisfies O[\"Member\"];\n",
                &[],
            ),
            (
                "type Member = { id: string };\nexport const one = { id: \"x\" } satisfies Member;\n",
                &["`one` satisfies `Member`, which is not imported from `@sdks/*`"],
            ),
            (
                "import type { components } from \"@sdks/orgs\";\nexport const members: components[\"schemas\"][\"Member\"][] = [];\n",
                &["`members` has no `satisfies <T>`"],
            ),
            (
                "import type { components } from \"@sdks/orgs\";\nexport const members = [({} satisfies components[\"schemas\"][\"Member\"])];\n",
                &["`members` has no `satisfies <T>`"],
            ),
            (
                "import type { Member } from \"@/lib/data/orgs\";\nexport const members = [] satisfies Member[];\n",
                &["`members` satisfies `Member[]`, which is not imported from `@sdks/*`"],
            ),
            (
                "export const names = [\"a\"] satisfies string[];\n",
                &["`names` satisfies `string[]`, which is not imported from `@sdks/*`"],
            ),
            (
                "export function members() {\n  return [];\n}\n",
                &["a fixture exports only `const` values"],
            ),
            ("const members = [];\n", &["exports no fixture"]),
            (
                "// no-spec: GET /invoices\ntype Invoice = { id: string };\nexport const invoices = [] satisfies Invoice[];\n",
                &[],
            ),
            (
                "// no-spec: GET /invoices\nexport const invoices = [];\n",
                &["`invoices` has no `satisfies <T>`"],
            ),
            (
                "// no-spec: PATCH /members/{member_id}\ntype Member = { id: string };\nexport const member = { id: \"x\" } satisfies Member;\n",
                &[
                    "PATCH /members/{member_id} is in @sdks/orgs now. Use its type and remove the no-spec marker",
                ],
            ),
            (
                "// external: kratos GET /sessions/whoami\ntype Whoami = { id: string };\nexport const whoami = { id: \"x\" } satisfies Whoami;\n",
                &[],
            ),
            (
                "// no-spec: members\nimport type { components } from \"@sdks/orgs\";\nexport const members = [] satisfies components[\"schemas\"][\"Member\"][];\n",
                &["the no-spec marker must name one endpoint"],
            ),
        ];
        for (src, want) in cases {
            let (got, _) = check_file(src, &endpoints);
            assert_eq!(got.len(), want.len(), "{src}: {got:?}");
            for ((_, message), expected) in got.iter().zip(want.iter()) {
                assert!(message.contains(expected), "{message} lacks {expected}");
            }
        }
    }

    #[test]
    fn a_seam_needs_its_guard() {
        let pending: BTreeMap<String, Vec<String>> =
            [("invoices".to_owned(), vec!["GET /invoices".to_owned()])].into();
        let seams: BTreeMap<&str, String> = [
            (
                "guarded.ts",
                "import { invoices } from \"@/fixtures/invoices\";\ndesignOnly();\n".to_owned(),
            ),
            (
                "unguarded.ts",
                "import { invoices } from \"@/fixtures/invoices\";\n".to_owned(),
            ),
            (
                "other.ts",
                "import { members } from \"@/fixtures/members\";\n".to_owned(),
            ),
        ]
        .into();
        let got = check_guards(&pending, &seams);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "unguarded.ts");
        assert!(
            got[0]
                .1
                .starts_with("reads @/fixtures/invoices, which waits for GET /invoices")
        );
    }

    #[test]
    fn mask_keeps_offsets() {
        let src = "const a = \"x;y\"; // c;\n/* d\n; */ const b = `e;${f}`;\n";
        let got = mask(src);
        assert_eq!(got.len(), src.len());
        assert_eq!(got.matches('\n').count(), src.matches('\n').count());
        assert_eq!(got.matches(';').count(), 2);
    }

    #[test]
    fn contrast_of_black_on_white_is_21() {
        let got = ratio(Rgb(0.0, 0.0, 0.0), Rgb(1.0, 1.0, 1.0));
        assert!((got - 21.0).abs() < 1e-9);
    }
}
