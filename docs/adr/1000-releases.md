# ADR-1000: Releases

- **Status:** Accepted
- **Decides:** Every version is 0.y.z with a plain tag, any release may break a repository, and a repository pins a tag in its flake input.

## Context

tabkit has one maintainer and no promise of compatibility, per [ADR-0000](0000-foundations.md). A version number that claims stability would be false.

A repository gets every tool through Nix, per [ADR-0100](0100-toolchain.md). So tabkit ships as a flake package, not as an archive per platform.

## Decision

**The major version is 0, always.** Under Semantic Versioning, a 0.y.z release may change anything. A new or stricter rule bumps the minor version, and a fix bumps the patch version.

**The tag is plain.** A release is the tag `v0.y.z`, with no suffix such as `-beta`. The version in `Cargo.toml` matches the tag.

**A repository pins a tag.** Its flake input is `github:tabmadi/tabkit/v0.y.z`, and its `flake.lock` pins the commit. A new version reaches the repository only when it moves the input to a new tag, and its gate decides.

## Consequences

A release can fail a repository's gate. The repository then keeps its tag until it fixes the findings.

The crate is not published to crates.io. `publish = false` in `Cargo.toml` prevents it.

## Rules

- Every version is `0.y.z`. A tag is `v0.y.z`, with no suffix.
- The version in `Cargo.toml` matches the tag.
- A new or stricter rule bumps the minor version.
- A repository pins tabkit by a tag in its flake input.
- The crate is never published to crates.io.
