# tabrules

Opinionated checks and generators for the files of a repository: prose, code comments, decision records, workflows, and pins. It ships one rule set, the [Simple English profile](docs/reference/profile.md), and checks the structure of an ADR set, in one binary.

tabrules has one maintainer. It is free to use under the Apache License 2.0, and it makes no promise of support or compatibility. Every version is 0.y.z, and any release may break your gate.

## What it does

| Group | Checks |
| --- | --- |
| `simple` | Punctuation, contractions, Latin abbreviations, complex words, emoji, and sentence length in prose, comments, and UI strings |
| `construct` | Intensifiers, hedges, meta-commentary, chronology, status words, planned work, and dated headings |
| `char` | The em dash, en dash, ellipsis, and curly quotes, in every text file |
| `comment` | Block length, paragraphs, commented-out code, author lines, banners, uncited `TODO`s, and doc comments that restate the name |
| `adr` | The header, the one-sentence Decides line, the index, references, Related, the section order, and the adoption, deferral, and tool registers |
| `docs` | Genre directories, the docs root, repeated filenames, single-file directories, links to untracked files, and documents that repeat the ADR set |
| `requirement` | In `requirements.md` and `docs/requirements/`: the prefix, the row form, the EARS keyword, and an ID that a test cites. The rules act only when a requirements file exists |
| `annotation` | A `(CI: <task>)` annotation names a recipe in `justfile` |
| `workflow` | A workflow step calls a `just` recipe and holds no pipeline logic |
| `image` | An image reference or a workflow action does not float |
| `script` | A script prints status through its log library |
| `version` | `flake.nix` and `go.mod` or `pyproject.toml` name the same version line |
| `just` | Every public recipe has a doc comment |

`comment/workaround` checks that a workaround names its pin or a recent check date. Each rule acts only when its input exists. `tabrules rules` lists every rule ID, and `tabrules gen --list` lists every generator. `tabrules affected` prints the services, apps, and libraries that a diff touches, as JSON. Comments are read in Go, TypeScript, JavaScript, Rust, Python, GDScript, shell, TOML, YAML, and SQL.

## Getting started

Install tabrules with cargo, at a release tag:

```bash
cargo install --locked --git https://github.com/tabmadi/tabrules --tag v0.3.0
```

Or add it as a flake input:

```nix
inputs.tabrules = {
  url = "github:tabmadi/tabrules/v0.3.0";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then run `tabrules check` at the root of a repository. Each finding is one line: `path:line: rule: message`.

## Configuration

tabrules reads `.config/tabrules.toml`. An empty or missing file runs every rule with its defaults. [Configuration](docs/reference/configuration.md) lists the keys.

## Decisions

The ADRs in [docs/adr](docs/adr/README.md) record every decision and the reason for it.

## License

Apache License 2.0. See [LICENSE](LICENSE).
