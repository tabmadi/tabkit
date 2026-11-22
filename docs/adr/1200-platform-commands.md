# ADR-1200: Platform commands

- **Status:** Accepted
- **Decides:** The platform commands ship as the `tabkit-ops` binary in the `tabkit-platform` package, and a project holds only its data and its own logic.

## Context

A platform template holds scripts that create clusters, promote images, sign releases, and manage secrets. A project made from the template would carry a copy of each script. A fix would then need one manual change for each project.

## Decision

**One binary holds the commands.** `crates/ops` holds every command in Rust. `tabkit-ops <command>` runs one in the repository of the caller.

**A small repo does not pay for it.** The `tabkit-platform` package holds the binary. The default package and the default module do not reference it.

**The boundary is the data.** A script that holds the names or behaviour of one project stays in that project. A shared script reads the project through its files.

**The command name and its arguments are the contract.** A project calls a command by name, and the code behind it can change.

**The layout is the contract.** Every project comes from one template, so the commands read fixed paths and need no config file. The [platform layout](../reference/platform-layout.md) lists them.

**The commands call pinned tools.** A command calls `kubectl`, `helm`, `kind`, `docker`, `cosign`, `syft`, `sops`, and `curl`, and it does not reimplement them. A credential reaches a tool through a file or standard input, and not through its arguments, where the tool allows it.

**A project script has a small shell library.** `tabkit-ops lib` prints the folder of `bootstrap.sh`, `log.sh`, and `cluster.sh`. It holds helpers for project scripts, and no command.

**The rules stay read-only.** `crates/rules` does not depend on `crates/ops`, per [ADR-0900](0900-architecture.md).

## Consequences

A fix to a platform command reaches a project when the project moves its tabkit pin.

The commands and the charts of a project must agree on the layout of the repository. A release that changes this layout can fail the gate of a project, per [ADR-1000](1000-releases.md).

## Rules

- A project calls a shared command through `tabkit-ops`, and it holds no copy of the script.
- A script that names a service of one project stays in that project.
- `crates/rules` has no dependency on `crates/ops`.
- The default package of the flake does not contain `tabkit-ops`.
