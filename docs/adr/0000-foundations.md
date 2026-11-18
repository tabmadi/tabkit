# ADR-0000: Foundations

- **Status:** Accepted
- **Decides:** tabnix is the shared Nix toolchain of one owner's repos, it ships as a tagged flake, and every decision is an ADR.

## Context

The owner's repos use one toolchain shape. Each repo held its own copy of the same Nix code: the dprint wrapper, the check helper, the git hooks, and the shared checks. Copied code drifts, and a fix in one copy never reaches the others.

## Decision

tabnix holds the Nix code that every repo shares, per [ADR-0200](0200-module.md). It also ships opt-in NixOS modules for the machines that a repo runs, per [ADR-0300](0300-nixos-modules.md). A repo imports it and adds only its language tools. The docs and the configs stay in each repo, because a reader needs them in place.

**It serves its owner.** The defaults are the owner's opinion. The code is public under the Apache License 2.0, and it makes no promise of support or compatibility.

**Every version is 0.y.z with a plain tag.** A new or stricter shared check bumps the minor version, and a fix bumps the patch version. A repo pins a tag in its flake input, and its gate decides when to move.

**The recipe names are a contract.** The recipes `setup`, `format`, `lint`, `test`, `check`, and `ci` always exist, per [ADR-0100](0100-toolchain.md). Their names do not change.

**Every decision is an ADR.** An ADR states a rule as true now. A gap between an ADR and the code is a defect in the code.

**An ADR has a fixed shape.** It has a `Status`, a one-sentence `Decides` line, and the sections `Context`, `Decision`, `Consequences`, and `Rules`. The `Rules` section is a flat list, so a grep over all ADRs finds every convention.

**Inside this repo, an ADR links only to other ADRs.** A README or a guide moves, and the ADR outlives it. A link to a published standard is allowed.

## Consequences

One fix to a shared check reaches every repo when it moves its input to the new tag.

A release can fail a repo's gate. The repo then keeps its tag until it fixes the findings.

## Rules

- Every ADR has a `Status`, a one-sentence `Decides` line, and a flat `Rules` section. `(CI: lint-prose)`
- Inside this repo, an ADR links only to other ADRs.
- The recipes `setup`, `format`, `lint`, `test`, `check`, and `ci` exist, and their names do not change.
- Every version is `0.y.z`, and a tag is `v0.y.z` with no suffix.
- A new or stricter shared check bumps the minor version.
