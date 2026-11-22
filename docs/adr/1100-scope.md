# ADR-1100: Scope

- **Status:** Accepted
- **Decides:** tabkit checks and generates from repository files, in rule groups that need their input, with citations and paths from each project.

## Context

A repository states rules in its decision records, and many of those rules need a custom check or a generated file. Without a shared tool, each repository keeps its own programs for them.

The drivers for a YAML parser:

1. **Source lines.** A finding names the line of the value.
2. **Maintained.** The crate has recent releases and no open advisory.
3. **Small.** It adds few dependencies.

| Crate | 1 Source lines | 2 Maintained | 3 Small | Verdict |
| --- | --- | --- | --- | --- |
| **saphyr** | **Yes, on every node** | **Yes** | **Yes** | **Chosen** |
| yaml-rust2 | Through its parser events only | Yes | Yes | Rejected on driver 1 |
| serde-saphyr | Not on an untyped tree | Yes | Yes | Rejected on driver 1 |
| serde_yaml | No | No. The crate is archived | Yes | Rejected on drivers 1 and 2 |
| serde_norway, serde_yaml_ng | No | No recent release | Yes | Rejected on drivers 1 and 2 |
| serde_yml | No | No. It has the advisory RUSTSEC-2025-0068 | Yes | Rejected on drivers 1 and 2 |

## Decision

**tabkit has two jobs.** `tabkit check` reports findings. `tabkit gen` writes generated files, and `tabkit gen --check` fails when a file differs from its generated form.

**A rule names a topic.** Its ID never names a decision record of a project, because each project numbers its own.

**A project supplies the citation.** The key `cite` in `[rules."<id>"]`, or in the table of a group, adds that text to the end of each finding.

**Every path is a config key.** A rule reads its paths from its `[rules."<id>"]` table, and each key has a default.

**A rule acts only when its input exists.** A repository without the input of a rule gets no finding from it, so a small repository needs no long `disable` list.

**A rule may run a program that reads its input.** Such a program is `git`, `just`, `helm`, or `go`. A rule that needs a missing optional program has no input and reports nothing.

**A rule whose input exists in most repositories is opt-in.** It runs only when a `[rules]` table adopts it, because a repository enforces a rule only after its own decision states it.

**A slow rule is on demand.** A rule that renders charts or lists packages runs only when `--only` names it, so a commit hook stays fast.

**A key in `[rules]` must name a rule, a generator, or a group.** Any other key is an error, so a typo never drops a citation.

## Consequences

The tool grows with each rule group. A user who needs only part of it turns the other groups off.

A repository that runs tabkit in a sandbox adds each optional program to that sandbox, or the rules that need it stay silent.

## Rules

- A rule ID has a topic for its group and never names a decision record of a project.
- Every rule and generator can be turned off by its ID or by its group.
- Every path that a rule reads is a config key with a default.
- A rule reports nothing when its input does not exist.
- `tabkit gen --check` writes nothing. `(CI: test)`
- A key in `[rules]` names a known rule, generator, or group. `(CI: test)`
