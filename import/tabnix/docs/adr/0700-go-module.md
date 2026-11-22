# ADR-0700: Go module

- **Status:** Accepted
- **Decides:** The opt-in `go` module ships the ruleguard rules and a golangci-lint wrapper that gives a repo config their store path.

## Context

Two Go rules have no linter of their own: no assignment in the init statement of `if`, and no `==`, `!=`, or `switch` on an error. They are ruleguard rules, and ruleguard runs only inside golangci-lint.

golangci-lint expands only `${base-path}` and `${config-path}` in a config, so a config cannot name a Nix store path through an environment variable.

The drivers:

1. **One source.** A fix to a rule reaches each repo with a new tag.
2. **Readable config.** The repo keeps one golangci-lint config in `.config/`.
3. **No custom build.** The repo runs the golangci-lint of nixpkgs.

| Option | 1 One source | 2 Readable config | 3 No custom build | Verdict |
| --- | --- | --- | --- | --- |
| A copy of the rules in each repo | No | Yes | Yes | Rejected on driver 1 |
| A config that Nix generates | Yes | No. The config leaves the repo | Yes | Rejected on driver 2 |
| A golangci-lint module plugin | Yes | Yes | No | Rejected on driver 3 |
| **A wrapper that replaces a placeholder in a copy of the config** | **Yes** | **Yes** | **Yes** | **Chosen** |

errorlint with `comparison: true` does not replace the error rule. It allows sentinels such as `io.EOF` from the standard library, and the rule forbids them.

## Decision

**The module is `flakeModules.go`.** It adds a `golangci-lint` wrapper to the dev shell. The wrapper copies the file of `--config`, replaces `@tabnix-go-rules@` with the store path of the rules, and runs the real binary.

| Option, under `perSystem` | Sets |
| --- | --- |
| `tabnix.go.package` | The golangci-lint that the wrapper runs |
| `tabnix.go.golangci` | Read only. The wrapper, for a check of the repo |
| `tabnix.go.rules` | Read only. The rules file |

**A repo keeps the ruleguard DSL in its `go.mod`.** ruleguard type-checks the rules through the module of the working directory, so a repo holds a file with the `tools` build tag that imports `github.com/quasilyte/go-ruleguard/dsl`.

**Each repo keeps its whole config.** golangci-lint has no way to merge or extend a config, so the shared part cannot move into this module.

## Consequences

A repo that replaces tabnix copies the rules file and sets its path in the config.

## Rules

- The ruleguard rules live in `modules/go-rules.go`.
- A repo names the rules with `@tabnix-go-rules@` in its golangci-lint config. `(CI: ci)`
- A repo that imports the module keeps the DSL in its `go.mod`.
