# Every task, per ADR-0100. A recipe that calls a tool runs only inside the dev shell of flake.nix.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes
default:
    @just --list

# Build the dev shell, download the crates, and install the git hooks
setup:
    nix develop --command cargo fetch --locked

# Build the release binary at target/release/tabkit
build: in-shell
    cargo build --release --locked --package tabkit

# Apply every formatter
format: in-shell
    cargo fmt
    rumdl check --fix .
    nixfmt $(git ls-files '*.nix')
    dprint fmt
    just --fmt

# Rust format and clippy, with every warning an error
lint-rust: in-shell
    cargo fmt --check
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Prose, comments, and ADRs, checked by the tabkit that the working tree builds, per ADR-0001
lint-prose: in-shell
    cargo run --quiet --locked --package tabkit -- check

# Markdown structure and formatting, per ADR-0001
lint-md: in-shell
    rumdl check .

# Scan the working tree for committed secrets
lint-secrets: in-shell
    gitleaks dir . --no-banner

# JSON, JSONC, TOML, and YAML format, per ADR-0100
lint-data: in-shell
    dprint check

# Nix format, per ADR-0100
lint-nix: in-shell
    nixfmt --check $(git ls-files '*.nix')

# justfile format, per ADR-0100
lint-just:
    just --fmt --check

# Run every linter
lint: lint-rust lint-prose lint-md lint-secrets lint-data lint-nix lint-just

# Run the unit and fixture tests, and evaluate every output of this flake
test: in-shell
    cargo test --workspace --locked
    nix flake check --no-build

# Write the expected fixture output again after a deliberate change, then review the diff
bless: in-shell
    TABKIT_BLESS=1 cargo test --workspace --locked

# The fast gate: run it before you finish a change
check: lint test

# Run exactly what CI runs: every check of flake.nix, in the Nix sandbox. On Linux it boots the nixosTests
ci:
    nix flake check --print-build-logs
    {{ just_executable() }} test-nested

# Push what CI built to the binary cache, per ADR-0600. Without NIX_CACHE_URL it does nothing
cache-push: in-shell
    nix-cache-push

# The nested microVM test of ADR-0500, which GitHub CI cannot run. It needs Linux with KVM
test-nested:
    if [ -n "${CI:-}" ]; then echo "GitHub CI skips the nested microVM test, per ADR-0500."; elif [ -w /dev/kvm ]; then nix build .#nested-microvm-host --no-link --print-build-logs; else echo "The nested microVM test needs KVM. It did not run." >&2; exit 1; fi

# Run Renovate over this repository, per ADR-0102. The renovate workflow calls it
ci-renovate: in-shell
    nix run --inputs-from . nixpkgs#renovate

# Write the files that follow a dependency change, per ADR-0102. This repo has none
deps-sync:
    @true

[private]
in-shell:
    @test -n "${IN_NIX_SHELL:-}" || { echo "Enter the dev shell first: run nix develop, or allow direnv." >&2; exit 1; }
