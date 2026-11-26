# ADR-0200: Module

- **Status:** Accepted
- **Decides:** tabkit is one flake-parts module that sets the base dev shell, the shared hooks, and the shared checks, and a repo extends it through options.

## Context

Each repo needs the same base and its own language parts. The base must merge with those parts without code in the repo that joins them.

| Option | Verdict |
| --- | --- |
| A library of functions | Each repo joins the outputs by hand, and that code is copied again |
| **A flake-parts module** | **Chosen. flake-parts merges the module with the outputs of the repo, and it is the common way to share flake outputs** |

## Decision

**The module sets the base.** It sets the four systems, the dev shell, the git hooks, the checks `lint-prose`, `lint-md`, `lint-secrets`, `lint-data`, `lint-nix`, and `lint-just`, and `formatter`.

**A repo extends it through options under `perSystem`.**

| Option | Does |
| --- | --- |
| `tabkit.packages` | Adds the language tools to the dev shell |
| `tabkit.hooks` | Adds hooks in the git-hooks.nix format |
| `tabkit.compiler` | Puts a C compiler in the dev shell |
| `tabkit.env` | Sets environment variables in the dev shell, such as the library path that a Python wheel with native code needs |
| `tabkit.imageVersions` | Maps a Dockerfile `ARG` to the version of the dev shell, so every image builds with the tested toolchain. The `image-versions` check enforces it |
| `tabkit.rules.enable` | Turns tabkit on or off in the dev shell, the hooks, and the checks |
| `tabkit.rules.package` | Sets the tabkit binary |
| `tabkit.check` | Read only. Makes a check that runs on a copy of the tracked files |

A repo adds its own checks and packages as plain flake-parts outputs, such as `checks.lint-go`.

**The tools read their configs from `.config/` in the repo.** The module passes the path where a tool does not find it: gitleaks gets an environment variable, cog and dprint get a flag.

**tabkit is an independent tool, and a repo can turn it off.** It comes from a tagged input. The tabkit repo uses tabkit too, so it sets `tabkit.rules.package` to the binary that its own commit builds.

**`commit-range` is in the dev shell.** It runs cog over the commits of a branch, against `BASE_REF`, because a rebase can bring a bad message past the commit-msg hook.

**`lint-workflows` runs actionlint.** It finds event text in a `run` script, and a repo lists its extra runner labels in `.config/actionlint.yaml`.

**tabkit checks itself.** Its own flake imports the module, so a broken change fails its own gate first.

## Consequences

A repo learns the flake-parts module system, a small part of Nix.

A change to a shared hook or check reaches a repo only with a new tag.

## Rules

- The module holds only what every repo shares. A language tool stays in the repo.
- Every option has a description.
- A repo extends the module through its options or plain flake-parts outputs, never by a copy of its code.
- The tools read their configs from `.config/` in the repo.
- A Dockerfile `ARG` that names a toolchain version names the version of the dev shell, through `tabkit.imageVersions`.
