# Every task, per ADR-0100. A recipe that calls a tool runs only inside the dev shell of flake.nix.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes
default:
    @just --list

# Build the dev shell, download the crates, and install the git hooks
setup:
    nix develop --command cargo fetch --locked

# Build the release binary at target/release/tablint
build: in-shell
    cargo build --release --locked

# Run the unit tests and the fixture tests
test: in-shell
    cargo test --locked

# Write the expected fixture output again after a deliberate change, then review the diff
bless: in-shell
    TABLINT_BLESS=1 cargo test --locked

# Rust format and clippy, with every warning an error
lint-rust: in-shell
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# The repo's own prose, comments, and ADRs, checked by the tablint that the working tree builds
lint-prose: in-shell
    cargo run --quiet --locked -- check

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

# Apply every formatter
format: in-shell
    cargo fmt
    rumdl check --fix .
    nixfmt $(git ls-files '*.nix')
    dprint fmt
    just --fmt

# The fast gate: run it before you finish a change
check: lint test

# Run exactly what CI runs: every check of flake.nix, in the Nix sandbox
ci:
    nix flake check --print-build-logs

# Push what CI built to the binary cache, per ADR-0100. Without NIX_CACHE_URL it does nothing
cache-push: in-shell
    nix-cache-push

[private]
in-shell:
    @test -n "${IN_NIX_SHELL:-}" || { echo "Enter the dev shell first: run nix develop, or allow direnv." >&2; exit 1; }
