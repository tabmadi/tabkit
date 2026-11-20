# ADR-0400: Opt-in flake modules

- **Status:** Accepted
- **Decides:** Nix code that only some repos share ships as an opt-in flake-parts module, with a check that runs it on a fixture.

## Context

The module of [ADR-0200](0200-module.md) holds only what every repo shares. Some Nix code is copied into some repos only:

| Code | Copies |
| --- | --- |
| The template scope: the `lint-scope` check and the `templates` output. Only the description and the welcome text differ | Every template |
| The Bun parts: Bun and bun2nix in the dev shell, the hook for `bun.nix`, and the helper that gives a check the locked `node_modules` | Every repo on Bun |

Copied code drifts, and a fix in one copy never reaches the others, the same as the toolchain of [ADR-0000](0000-foundations.md).

The drivers:

1. **One source.** A fix reaches every repo that has the part with a new tag.
2. **Only what a repo uses.** A repo without the part loads none of its code and none of its inputs.
3. **Small repo code.** A repo states only what differs, such as a description.
4. **Tested.** The shared code fails in tabnix first, not in a repo.

| Option | 1 One source | 2 Only what a repo uses | 3 Small repo code | 4 Tested | Verdict |
| --- | --- | --- | --- | --- | --- |
| A copy in each repo | No | Yes | No | Each copy tests itself, or not at all | Rejected. It is the drift that this ADR removes |
| An option of the default module, off by default | Yes | No. Every repo evaluates the code, and every repo locks bun2nix | Yes | Yes | Rejected. It breaks the rule of ADR-0200 that the default module holds only what every repo shares |
| A library of functions, such as `lib.mkTemplate` | Yes | Yes | No. Each repo joins the outputs by hand | Yes | Rejected. ADR-0200 rejects a library for the same reason |
| A separate flake for each part | Yes | Yes | Yes | Yes | Rejected. Each flake needs its own tags, lock, and gate, for two small modules |
| **A `flakeModules.<name>` output of this flake** | **Yes** | **Yes. flake-parts loads a module only when a repo imports it** | **Yes** | **Yes** | **Chosen** |

Sources:

- A flake exports flake-parts modules through `flakeModules`: [flake.parts, the flakeModules option](https://flake.parts/options/flake-parts-modules.html).
- `importApply` gives a module the inputs of the flake that defines it: [flake.parts, dogfood a reusable module](https://flake.parts/dogfood-a-reusable-module.html).

## Decision

**An opt-in module is a separate output.** It is `flakeModules.<name>` of the tabnix flake, from the file `modules/<name>.nix`. The default module never imports it.

**An opt-in module may need the default module.** Its options extend `tabnix.packages` and `tabnix.hooks`, so a repo imports both.

**The template module is `flakeModules.template`.** A template imports it from `template/scope.nix`, so a project made from the template never loads it. It adds the `lint-scope` check and the `templates.default` output, which holds every file except the `template/` folder and the folders of `exclude`.

| Option, at the top level | Sets |
| --- | --- |
| `tabnix.template.description` | The one-line description that `nix flake show` prints |
| `tabnix.template.welcomeText` | The first steps that `nix flake new -t` prints |
| `tabnix.template.exclude` | The top-level folders that the default template leaves out, beside `template/` |
| `tabnix.template.variants.<name>` | One more template output, with its own welcome text and its own `exclude` list |

A variant serves a template with a mode that adds folders, such as a host for a self-hosted mode. The default leaves the folders out, and the variant keeps them.

**The bun module is `flakeModules.bun`.** It puts Bun and bun2nix in the dev shell, and adds the `bun-nix` hook, which fails when `bun.nix` does not match `bun.lock`. tabnix pins bun2nix, so a repo on Bun has no bun2nix input of its own.

| Option, under `perSystem` | Sets |
| --- | --- |
| `tabnix.bun.package` | The Bun of the dev shell and the checks: the newest release, built with the recipe of nixpkgs, so a repo never waits for nixpkgs |
| `tabnix.bun.tools` | The tools in every Bun derivation, such as the Node.js that runs the app |
| `tabnix.bun.installFlags` | The flags of the install, such as `--linker=hoisted` |
| `tabnix.bun.env` | The environment variables of every Bun derivation |
| `tabnix.bun.install` | Read only. The attributes of a derivation that installs the locked `node_modules`, to extend with its own phases |
| `tabnix.bun.check` | Read only. Makes a check from a name, a list of tools, and a command |

**Every opt-in module has a check.** The check `module-<name>` builds a small flake from a fixture in `tests/fixtures/` with the module, and asserts what it outputs. The check of the template module also asserts that `lint-scope` refuses a doc that speaks as the template.

## Consequences

A template gets a fix of the template scope or the Bun parts only when it moves to a new tag.

A repo on Bun takes the bun2nix version that tabnix pins. A repo that needs another version waits for a tag of tabnix.

## Rules

- Nix code that some repos share is `flakeModules.<name>` from `modules/<name>.nix`, and the default module never imports it.
- Every option of an opt-in module is under `tabnix.` and has a description.
- Every opt-in module has a check `module-<name>` that runs it on a fixture in `tests/fixtures/`. `(CI: ci)`
- A repo states only what differs through the options, never by a copy of the code of the module.
- A new opt-in module bumps the minor version.
