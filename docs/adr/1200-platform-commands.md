# ADR-1200: Platform commands

- **Status:** Accepted
- **Decides:** The platform commands ship as the `tabkit-ops` binary in the `tabkit-platform` package, and a project holds only its data and its own logic.

## Context

A platform template holds scripts that create clusters, promote images, sign releases, and manage secrets. A project made from the template would carry a copy of each script. A fix would then need one manual change for each project.

## Decision

**One binary holds the commands.** `crates/ops` embeds `crates/ops/scripts/`. `tabkit-ops <command>` writes the scripts once to a cache folder that their content names, then runs the command in the repository of the caller.

**A small repo does not pay for it.** The `tabkit-platform` package holds the binary. The default package and the default module do not reference it.

**The boundary is the data.** A script that holds the names or behaviour of one project stays in that project. A shared script reads the project through its files.

**A command can move to Rust without a change for a project.** The command name and its arguments are the contract. A port replaces the embedded script behind the same name.

**The rules stay read-only.** `crates/rules` does not depend on `crates/ops`, per [ADR-0900](0900-architecture.md).

## Consequences

A fix to a platform command reaches a project when the project moves its tabkit pin.

The scripts and the charts of a project must agree on the layout of the repository. A release that changes this layout can fail the gate of a project, per [ADR-1000](1000-releases.md).

## Rules

- A project calls a shared command through `tabkit-ops`, and it holds no copy of the script.
- A script that names a service of one project stays in that project.
- `crates/rules` has no dependency on `crates/ops`.
- The default package of the flake does not contain `tabkit-ops`.
