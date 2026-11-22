# Architecture Decision Records

Each ADR has a one-sentence **Decides** line and ends with a flat **Rules** section. This table lists the Decides line of every ADR.

| ADR | Decides |
| --- | --- |
| [0000](0000-foundations.md) | tabkit is one tagged flake for the checks, the generators, and the Nix toolchain, and every decision is an ADR |
| [0001](0001-documentation-conventions.md) | The writing rules for every text in the repo: Simple English on the ASD-STE100 model |
| [0100](0100-toolchain.md) | Nix pins every tool and runs every check through this repo's own module, `just` runs every task, and Zed is the supported editor |
| [0200](0200-module.md) | tabkit is one flake-parts module that sets the base dev shell, the shared hooks, and the shared checks, and a repo extends it through options |
| [0300](0300-nixos-modules.md) | tabkit ships opt-in NixOS modules for the machines that a repo runs, and a nixosTest in CI proves each refusal of each module |
| [0400](0400-opt-in-flake-modules.md) | Nix code that only some repos share ships as an opt-in flake-parts module, with a check that runs it on a fixture |
| [0600](0600-binary-cache.md) | A repo with two variables uses a binary cache on niks3 and S3, CI pushes with OIDC, and other repos run as before |
| [0700](0700-go-module.md) | The opt-in `go` module ships the ruleguard rules and a golangci-lint wrapper that gives a repo config their store path |
| [0800](0800-shell-module.md) | The opt-in `shell` module adds shfmt, shellcheck, and a format hook for a repo that keeps shell scripts |
| [0500](0500-self-hosted-machines.md) | A repo that runs its own machine takes the server base, the microVM host, and the deploy from tabkit, and a test proves each refusal |
| [0900](0900-architecture.md) | tabkit is one Rust binary that parses a repository once into a model and runs built-in rules with stable IDs over it |
| [1000](1000-releases.md) | Every version is 0.y.z with a plain tag, any release may break a repository, and a repository pins a tag in its flake input |
| [1100](1100-scope.md) | tabkit checks and generates from repository files, in rule groups that need their input, with citations and paths from each project |

A new ADR starts from [`_template.md`](_template.md).
