# Architecture Decision Records

Each ADR has a one-sentence **Decides** line and ends with a flat **Rules** section. This table lists the Decides line of every ADR.

| ADR | Decides |
| --- | --- |
| [0000](0000-foundations.md) | tabnix is the shared Nix toolchain of one owner's repos, it ships as a tagged flake, and every decision is an ADR |
| [0001](0001-documentation-conventions.md) | The house style for every text in the repo: Simple English on the ASD-STE100 model |
| [0100](0100-toolchain.md) | Nix pins every tool and runs every check through this repo's own module, `just` runs every task, and Zed is the supported editor |
| [0200](0200-module.md) | tabnix is one flake-parts module that sets the base dev shell, the shared hooks, and the shared checks, and a repo extends it through options |

A new ADR starts from [`_template.md`](_template.md).
