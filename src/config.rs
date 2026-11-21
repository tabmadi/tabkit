//! `tabrules.toml`. Every key has a default, and the defaults are the opinion: an empty file runs every rule.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

pub const FILE_NAME: &str = ".config/tabrules.toml";

/// Rules that run only when a `[rules]` table adopts them.
pub const OPT_IN: &[&str] = &[
    "image/floating-tag",
    "image/unpinned-action",
    "script/log-vocabulary",
    "docs/headcount",
    "docs/stack-table",
    "docs/principles",
    "project/identity",
    "service/node-scope",
    "service/contract",
    "helm/app-version",
    "sql/pii-tag",
    "api/audience",
    "api/prefixes",
    "go/call-args",
    "go/activity-register",
    "go/service-deps",
    "money/spec",
    "money/no-float",
    "money/typescript",
    "kube/resource-governance",
];

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Paths that no rule reads, as globs.
    pub exclude: Vec<String>,
    /// Rule IDs or rule groups, such as `adr` or `prose/question`, that do not run.
    pub disable: Vec<String>,
    pub prose: Prose,
    pub comments: Comments,
    pub adr: Adr,
    pub docs: Docs,
    pub messages: Messages,
    pub annotations: Annotations,
    pub requirements: Requirements,
    /// Settings of one rule or one group, by its ID.
    pub rules: BTreeMap<String, RuleConfig>,
}

/// The settings of a rule. Each rule reads its own path keys from `options`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct RuleConfig {
    /// The project's citation, such as an ADR number. A finding prints it.
    pub cite: Option<String>,
    #[serde(flatten)]
    pub options: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Prose {
    /// Text adopted from a third party. No prose rule and no character rule reads it.
    pub verbatim: Vec<String>,
    pub max_words: usize,
    /// Documents that name the banned constructs, such as the ADR that bans them.
    pub constructs_exempt: Vec<String>,
    /// Documents whose subject is live state, so `currently` is the fact.
    pub status_exempt: Vec<String>,
    /// Technical names with a slash, such as `CI/CD`.
    pub slash_terms: Vec<String>,
    /// Regular expressions for spans that are not prose, such as an evidence grade.
    pub ignore: Vec<String>,
}

impl Default for Prose {
    fn default() -> Self {
        Self {
            verbatim: strings(&[
                "LICENSE",
                "**/CHANGELOG.md",
                ".github/CODE_OF_CONDUCT.md",
                "**/*.local.md",
            ]),
            max_words: 25,
            constructs_exempt: strings(&["docs/adr/0001-*.md", "docs/adr/_template.md"]),
            status_exempt: Vec::new(),
            slash_terms: strings(&["ci/cd", "i/o", "tcp/ip"]),
            ignore: strings(&[
                r"\((?:reasoned|documented|measured)\b[^()]*(?:\([^()]*\)[^()]*)*\)",
            ]),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Comments {
    /// The files whose comments the rules read, as globs. Empty means every file.
    pub paths: Vec<String>,
    pub max_lines: usize,
    /// Directory names that hold generated or vendored code.
    pub skip_dirs: Vec<String>,
    /// File name suffixes of generated code.
    pub skip_suffixes: Vec<String>,
    /// Comment prefixes that a tool in the repository reads, beside the built-in list.
    pub directives: Vec<String>,
    /// Pairs of markers. The lines between them are data a generator writes, not comments.
    pub skip_regions: Vec<[String; 2]>,
}

impl Default for Comments {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            max_lines: 3,
            skip_dirs: strings(&[
                "node_modules",
                "vendor",
                "dist",
                "build",
                "target",
                "testdata",
                "_generated",
            ]),
            skip_suffixes: strings(&["_gen.go", ".sql.go", ".pb.go", ".min.js"]),
            directives: Vec::new(),
            skip_regions: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Adr {
    pub dir: String,
    pub index: String,
    pub header_fields: Vec<String>,
    /// The index column that repeats each Decides line must match it word for word.
    pub index_matches_decides: bool,
    /// The section order. A section may be absent, but the ones present keep this order.
    pub sections: Vec<String>,
    /// ADR numbers outside the section rules, such as a foundations ADR with no options to compare.
    pub sections_exempt: Vec<String>,
    /// Words before `ADR-NNNN` that name another repository's set, such as `games`.
    pub foreign: Vec<String>,
    /// Documents where every `ADR-NNNN` must resolve.
    pub reader_paths: Vec<String>,
}

impl Default for Adr {
    fn default() -> Self {
        Self {
            dir: "docs/adr".into(),
            index: "docs/adr/README.md".into(),
            header_fields: strings(&["status", "decides"]),
            index_matches_decides: true,
            sections: strings(&[
                "Context",
                "Decision drivers",
                "Considered options",
                "Decision",
                "Consequences",
                "Rules",
            ]),
            sections_exempt: strings(&["0000"]),
            foreign: Vec::new(),
            reader_paths: strings(&["docs/**", "README.md", "AGENTS.md"]),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Docs {
    /// The genre directories under `docs/`. Empty means any directory.
    pub genre_dirs: Vec<String>,
    /// The file names allowed at the `docs/` root: the entry documents and the registries. Empty means any.
    pub root_files: Vec<String>,
    /// File names that repeat by design, such as the index of each directory.
    pub shared_names: Vec<String>,
    /// Link targets that no document under `from` may point to.
    pub forbidden_links: Vec<ForbiddenLink>,
}

impl Default for Docs {
    fn default() -> Self {
        Self {
            genre_dirs: Vec::new(),
            root_files: Vec::new(),
            shared_names: strings(&["README.md", "_template.md"]),
            forbidden_links: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ForbiddenLink {
    pub from: String,
    pub to: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Messages {
    /// The directory of UI string catalogues, one `<locale>.json` per locale.
    pub dir: Option<String>,
    /// The locale that gets the word rules. Every locale gets the punctuation and length rules.
    pub source_locale: String,
}

impl Default for Messages {
    fn default() -> Self {
        Self {
            dir: None,
            source_locale: "en".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Annotations {
    /// Annotation kinds whose values are `just` recipe names, such as `CI` in `(CI: lint)`.
    pub task_kinds: Vec<String>,
}

impl Default for Annotations {
    fn default() -> Self {
        Self {
            task_kinds: strings(&["CI", "check"]),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Requirements {
    /// The requirements files, as globs. A `README.md` among them is an index, not a requirements file.
    pub files: Vec<String>,
    /// The test files whose IDs count as citations, as globs.
    pub tests: Vec<String>,
    /// The starting files that a new module copies. They use the template prefix, and no test cites them.
    pub template_files: Vec<String>,
    /// The placeholder prefix of the starting files.
    pub template_prefix: String,
}

impl Default for Requirements {
    fn default() -> Self {
        Self {
            files: strings(&["**/requirements.md", "docs/requirements/*.md"]),
            tests: strings(&[
                "**/*_test.go",
                "**/*.test.ts",
                "**/*.test.tsx",
                "**/*.spec.ts",
                "**/*.spec.tsx",
                "**/*.test.js",
                "**/test_*.py",
                "**/*_test.py",
                "**/tests/**/*.rs",
            ]),
            template_files: strings(&["**/_template/**"]),
            template_prefix: "TPL".into(),
        }
    }
}

impl Config {
    /// Reads `.config/tabrules.toml` under the root, or returns the defaults when the file is absent.
    pub fn load(root: &Path, explicit: Option<&Path>) -> Result<Self> {
        let path = explicit.map_or_else(|| root.join(FILE_NAME), Path::to_path_buf);
        if explicit.is_none() && !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// The citation of a rule: its own, or the one of its group.
    pub fn cite(&self, rule: &str) -> Option<&str> {
        let group = rule.split('/').next().unwrap_or(rule);
        [rule, group]
            .iter()
            .find_map(|key| self.rules.get(*key)?.cite.as_deref())
    }

    /// A list of strings from the `[rules."<id>"]` table, or the default when the key is absent.
    pub fn option_strings(&self, rule: &str, key: &str, default: &[&str]) -> Result<Vec<String>> {
        let Some(value) = self.rules.get(rule).and_then(|r| r.options.get(key)) else {
            return Ok(strings(default));
        };
        value
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .with_context(|| format!("`{key}` of `{rule}` is not a list of strings"))
    }

    /// One string from the `[rules."<id>"]` table.
    pub fn option_str(&self, rule: &str, key: &str) -> Result<Option<String>> {
        let Some(value) = self.rules.get(rule).and_then(|r| r.options.get(key)) else {
            return Ok(None);
        };
        value
            .as_str()
            .map(|s| Some(s.to_owned()))
            .with_context(|| format!("`{key}` of `{rule}` is not a string"))
    }

    /// A table of strings from the `[rules."<id>"]` table, as key and value pairs.
    pub fn option_map(&self, rule: &str, key: &str) -> Result<Vec<(String, String)>> {
        let Some(value) = self.rules.get(rule).and_then(|r| r.options.get(key)) else {
            return Ok(Vec::new());
        };
        let table = value
            .as_table()
            .with_context(|| format!("`{key}` of `{rule}` is not a table"))?;
        table
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), s.to_owned()))
                    .with_context(|| format!("`{key}.{k}` of `{rule}` is not a string"))
            })
            .collect()
    }

    /// A rule runs unless its ID, or a group that contains it, is in `disable`. An opt-in rule also needs a
    /// `[rules]` table, for itself or for its group, because its input exists in most repositories.
    pub fn enabled(&self, rule: &str) -> bool {
        let disabled = self.disable.iter().any(|d| {
            rule == d
                || rule
                    .strip_prefix(d.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        });
        let group = rule.split('/').next().unwrap_or(rule);
        let adopted = self.rules.contains_key(rule) || self.rules.contains_key(group);
        !disabled && (adopted || !OPT_IN.contains(&rule))
    }
}

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// A set of path globs. A `*` does not cross a `/`, and `**` does.
pub struct Paths(GlobSet);

impl Paths {
    pub fn new(globs: &[String]) -> Result<Self> {
        let mut set = GlobSetBuilder::new();
        for g in globs {
            let glob = GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .with_context(|| format!("glob {g}"))?;
            set.add(glob);
        }
        Ok(Self(set.build()?))
    }

    pub fn matches(&self, path: &str) -> bool {
        self.0.is_match(path)
    }
}
