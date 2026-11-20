# Architecture Decision Records

Each ADR has a one-sentence **Decides** line and ends with a flat **Rules** section. This table lists the Decides line of every ADR.

| ADR | Decides |
| --- | --- |
| [0000](0000-foundations.md) | tabnix is the shared Nix toolchain of one owner's repos, it ships as a tagged flake, and every decision is an ADR |
| [0001](0001-documentation-conventions.md) | The house style for every text in the repo: Simple English on the ASD-STE100 model |
| [0100](0100-toolchain.md) | Nix pins every tool and runs every check through this repo's own module, `just` runs every task, and Zed is the supported editor |
| [0200](0200-module.md) | tabnix is one flake-parts module that sets the base dev shell, the shared hooks, and the shared checks, and a repo extends it through options |
| [0300](0300-nixos-modules.md) | tabnix ships opt-in NixOS modules for the machines that a repo runs, and a nixosTest in CI proves each refusal of each module |
| [0400](0400-opt-in-flake-modules.md) | Nix code that only some repos share ships as an opt-in flake-parts module, with a check that runs it on a fixture |
| [0600](0600-binary-cache.md) | The owner's repos share one binary cache on niks3 and S3, CI pushes with OIDC, and a repo without the variables runs as before |
| [0500](0500-self-hosted-machines.md) | A repo that runs its own machine takes the server base, the microVM host, and the deploy from tabnix, and a test proves each refusal |

A new ADR starts from [`_template.md`](_template.md).
