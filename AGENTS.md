# Agent guide

This guide is for any coding agent that works in this repo. `AGENTS.md` is the one standard, and the repo has no per-tool files such as `CLAUDE.md`.

**Humans are the first developers. The ADRs, `docs/`, and `README.md` rank above this file.** This file holds only operational hints.

## Read first

- [ADR-0900](docs/adr/0900-architecture.md): the crates, the repository model, the rule IDs, and the fixtures.
- [ADR-0200](docs/adr/0200-module.md): what the module sets and the options a repo uses.
- [ADR-0000](docs/adr/0000-foundations.md): what this repo is and the ADR process.
- [ADR-0300](docs/adr/0300-nixos-modules.md): the NixOS modules and their nixosTests.
- [ADR-0400](docs/adr/0400-opt-in-flake-modules.md): the opt-in flake modules and their checks.
- [ADR-0500](docs/adr/0500-self-hosted-machines.md): the server base, the microVM host, and the deploy.
- [ADR-0600](docs/adr/0600-binary-cache.md): the binary cache, its push from CI, and its variables.
- [docs/adr/README.md](docs/adr/README.md): the ADR index. To check a convention, grep the `Rules` sections first.

## Write in Simple English

Every text follows the Simple English profile in [ADR-0001](docs/adr/0001-documentation-conventions.md#simple-english): docs, ADRs, comments, commit titles, and task descriptions.

- Use common words. One idea per sentence, at most 25 words.
- In prose, use only period, comma, colon, and the possessive apostrophe. No em dash, semicolon, parentheses, or `?`.
- No emoji, no contractions, and no `e.g.` or `etc.`
- No chronology, intensifiers, hedges, or planned work. State what is true now.
- `just lint-prose` checks these rules with tabkit.

## Working in the repo

- Work inside the dev shell: run `nix develop`, or allow direnv. Then run every task as `just <recipe>`.
- Keep the recipe names, per [ADR-0000](docs/adr/0000-foundations.md).
- A tool that every repo needs goes into `nix/modules/default.nix`. A language tool stays in the repo that uses it.
- Code that only some repos share goes into `nix/modules/<name>.nix` as `flakeModules.<name>`, with its check in `nix/tests/<name>.nix` and its fixture in `nix/tests/fixtures/`.
- A NixOS module goes into `nix/nixos/<name>.nix`, with its nixosTest in `nix/tests/<name>.nix`. Never import either from `nix/modules/default.nix`.
- A new rule goes into `crates/rules/src/rules/`, with an ID in `RULES`, a case in a fixture in `crates/cli/tests/fixtures/`, and a line in the README table.
- After a deliberate change to the rule output, run `just bless` and review the diff of the `.expected` files.
- `crates/rules` never depends on `crates/ops`, so a check cannot reach a cluster, per [ADR-1200](docs/adr/1200-platform-commands.md).
- Before you finish a change, run `just check`. `just ci` runs the full sandboxed gate that CI runs, and the nested microVM test that CI cannot run.
- Tag a release only after `just ci` passes on a Linux machine with KVM, per ADR-0500.
- Commit messages are a Conventional Commits title only, with no body. Never push unless asked to.
