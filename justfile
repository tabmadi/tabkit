# Every task, per ADR-0100. A recipe that calls a tool runs only inside the dev shell of flake.nix.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes
default:
    @just --list

# Build the dev shell and install the git hooks
setup:
    nix develop --command true

# Apply every formatter
format: in-shell
    rumdl check --fix .
    nix fmt flake.nix modules/*.nix nixos/*.nix tests/*.nix
    dprint fmt
    just --fmt

# Prose, comments, and ADRs, per ADR-0001
lint-prose: in-shell
    tablint check

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
    nixfmt --check flake.nix modules/*.nix nixos/*.nix tests/*.nix

# justfile format, per ADR-0100
lint-just:
    just --fmt --check

# Run every linter
lint: lint-prose lint-md lint-secrets lint-data lint-nix lint-just

# Run the tests: every output of this repo's own flake evaluates
test:
    nix flake check --no-build

# The fast gate: run it before you finish a change
check: lint test

# Run exactly what CI runs: every check of flake.nix, in the Nix sandbox. On Linux it boots the nixosTests
ci:
    nix flake check --print-build-logs
    just test-nested

# The nested microVM test of ADR-0500, which GitHub CI cannot run. It needs Linux with KVM
test-nested:
    if [ -n "${CI:-}" ]; then echo "GitHub CI skips the nested microVM test, per ADR-0500."; elif [ -w /dev/kvm ]; then nix build .#nested-microvm-host --no-link --print-build-logs; else echo "The nested microVM test needs KVM. It did not run." >&2; exit 1; fi

[private]
in-shell:
    @test -n "${IN_NIX_SHELL:-}" || { echo "Enter the dev shell first: run nix develop, or allow direnv." >&2; exit 1; }
