# ADR-0200: Architecture

- **Status:** Accepted
- **Decides:** tabrules is one Rust binary that parses a repository once into a model and runs built-in rules with stable IDs over it.

## Context

The rules come from [ADR-0001](0001-documentation-conventions.md). They read three kinds of text: Markdown prose, code comments in many languages, and the structure of an ADR set across files.

A trial ran Vale for the prose and Conftest with Rego for the cross-document rules, against a set of custom Go linters. It matched every finding. It also added two tools and two rule languages, and the rules did not shrink. The comment parser stayed custom, because Vale does not read shell or GDScript comments.

Three needs decide the shape:

1. One language for every rule, so a person or an agent changes a rule in one place.
2. One parse for many rules, so a rule reads a model, not raw bytes.
3. One binary with no runtime, which Nix builds from the lock file.

| Option | Verdict |
| --- | --- |
| **One Rust binary with built-in rules** | **Chosen. Rust and Go both meet the three needs, and this choice has no technical driver** |
| One Go binary with built-in rules | Runner-up. It meets the same three needs |
| Vale and Conftest, with a Go extractor | Rejected after the trial. Its rules span YAML, Tengo, Rego, and Go |

## Decision

**The repository model is built once.** `git ls-files` gives the files: everything tracked, and anything new that git does not ignore. Outside a git work tree, such as the Nix sandbox, tabrules walks the directory.

Each Markdown file has two views. The prose view comes from pulldown-cmark: one unit per paragraph, heading, list item, and table cell, with a code span as one word. The fact view comes from a line scan: header fields, headings, tables, links, annotations, and `ADR-NNNN` mentions.

Each source file in a known language becomes comment blocks. Strings, heredocs, and generated regions are data, not comments.

**A rule is a function with a stable ID.** The ID is `<group>/<name>`, such as `simple/semicolon` or `adr/index`. `tabrules rules` lists them. A rule with a new meaning takes a new ID.

**The defaults are the opinion.** A missing or empty `.config/tabrules.toml` runs every rule. A key sets a path or turns a rule off, per [ADR-0000](0000-foundations.md).

**A line opts out with `tabrules:allow`.** In Markdown it is an HTML comment, and in code it is a comment on the same line.

**The output is one finding per line**, as `path:line: rule: message`. The exit code is 0 for no finding, 1 for a finding, and 2 for an error.

**Each rule has a fixture.** `tests/fixtures/<name>/` is a small repository that breaks rules on purpose, and `<name>.expected` holds the exact output. `just bless` writes it again after a deliberate change.

## Consequences

A rule that only one repository needs is still code in tabrules. When it is too narrow for a built-in rule, the repository keeps its own check.

## Rules

- Every rule has an ID of the form `<group>/<name>` and a row in `rules::RULES`.
- A rule reads the repository model. It never runs another program.
- A rule never repeats a check of another linter of the toolchain.
- Every rule has a case in a fixture, and the fixture output is exact. `(CI: test)`
