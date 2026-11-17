# ADR-0000: Foundations

- **Status:** Accepted
- **Decides:** tablint is one owner's linter for prose, comments, and decision records, it is public and free to use, and every decision is an ADR.

## Context

The owner's repositories share one house style and one way to record decisions. Separate copies of the same checks drift apart, and a fix in one copy never reaches the others.

The established linters each own one language or one format. None of them checks the Simple English profile, the comment rules, and the ADR set together.

## Decision

tablint is one binary that checks the rules of [ADR-0001](0001-documentation-conventions.md) and the structure of an ADR set. A repository adds it as a flake input, per [ADR-0300](0300-releases.md), and adds a short `.config/tablint.toml`.

**It serves its owner.** The rules are the owner's opinion, and a default is a decision. A repository can turn a rule off, but it cannot redesign one. The code is public under the Apache License 2.0, and it makes no promise of support or compatibility.

**It checks only what nothing else checks.** Code style, Markdown structure, data formats, and secrets belong to the other linters of the toolchain, such as clippy, golangci-lint, rumdl, dprint, and gitleaks.

**The recipe names are a contract.** The recipes `setup`, `format`, `lint`, `test`, `check`, and `ci` always exist, per [ADR-0100](0100-toolchain.md). Their names do not change.

**Every decision is an ADR.** An ADR states a rule as true now. A gap between an ADR and the code is a defect in the code.

**An ADR has a fixed shape.** It has a `Status`, a one-sentence `Decides` line, and the sections `Context`, `Decision`, `Consequences`, and `Rules`. The `Rules` section is a flat list, so a grep over all ADRs finds every convention.

**Inside this repo, an ADR links only to other ADRs.** A README or a guide moves, and the ADR outlives it. A link to a published standard is allowed.

## Consequences

A rule that suits the owner and not another user stays. Another user forks the repository or turns the rule off.

One fix to a rule reaches every repository when it moves its flake input to the new tag.

## Rules

- Every ADR has a `Status`, a one-sentence `Decides` line, and a flat `Rules` section. `(CI: lint-prose)`
- Inside this repo, an ADR links only to other ADRs.
- The recipes `setup`, `format`, `lint`, `test`, `check`, and `ci` exist, and their names do not change.
- A rule that another linter of the toolchain already checks is not added to tablint.
- A repository changes a rule only by turning it off or by setting a path in `.config/tablint.toml`.
