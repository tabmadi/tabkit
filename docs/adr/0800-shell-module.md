# ADR-0800: Shell module

- **Status:** Accepted
- **Decides:** The opt-in `shell` module adds shfmt, shellcheck, and a format hook for a repo that keeps shell scripts.

## Context

A repo with many shell scripts needs a formatter and a linter. A repo without scripts needs neither, so the default module of [ADR-0200](0200-module.md) does not hold them.

| Option | Verdict |
| --- | --- |
| The tools and the hook in each repo | Rejected. The copies drift |
| The tools in the default module | Rejected. A repo without scripts pays for them |
| **An opt-in module** | **Chosen, per [ADR-0400](0400-opt-in-flake-modules.md)** |

## Decision

**The module is `flakeModules.shell`.** It adds shfmt and shellcheck to the dev shell, and a hook that fails on a script that shfmt would change. The option `tabkit.shell.indent` sets the indent, and its default is 2.

The file list and the shellcheck flags stay in the repo, because they depend on the layout of its scripts.

## Consequences

A repo with its own shell recipes keeps them, and they call the tools of this module.

## Rules

- The shfmt hook uses the indent of `tabkit.shell.indent`. `(CI: ci)`
