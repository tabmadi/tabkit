# Architecture Decision Records

Each ADR has a one-sentence **Decides** line and ends with a flat **Rules** section. This table lists the Decides line of every ADR.

| ADR | Decides |
| --- | --- |
| [0000](0000-foundations.md) | tabrules is one owner's linter for prose, comments, and decision records, it is public and free to use, and every decision is an ADR |
| [0001](0001-documentation-conventions.md) | The house style for every text in the repo: Simple English on the ASD-STE100 model |
| [0100](0100-toolchain.md) | Nix pins every tool, builds tabrules, and runs every check, `just` runs every task, and Zed is the supported editor |
| [0200](0200-architecture.md) | tabrules is one Rust binary that parses a repository once into a model and runs built-in rules with stable IDs over it |
| [0300](0300-releases.md) | Every version is 0.y.z with a plain tag, any release may break a repository, and a repository pins a tag in its flake input |

A new ADR starts from [`_template.md`](_template.md).
