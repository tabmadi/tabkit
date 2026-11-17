# tabnix

The shared Nix toolchain of one owner's repos, as a [flake-parts](https://flake.parts) module. A repo imports it and adds only its language tools.

tabnix serves its owner. It is free to use under the Apache License 2.0, but it makes no promise of support or compatibility. Every version is 0.y.z, and any release may break your gate.

## What it does

| Part | Holds |
| --- | --- |
| Dev shell | `just`, cocogitto, rumdl, gitleaks, dprint, tablint, nixd, and nixfmt, plus the tools of the repo |
| Git hooks | tablint, rumdl, dprint, nixfmt, `just --fmt`, gitleaks, `cog verify`, `cog check`, and `just ci` |
| Checks | `lint-prose`, `lint-md`, `lint-secrets`, `lint-data`, `lint-nix`, and `lint-just` |
| `formatter` | nixfmt |
| `systems` | Linux and macOS, on x86-64 and ARM |

## Getting started

1. Add flake-parts and tabnix as inputs, with tabnix at a release tag:

   ```nix
   inputs = {
     nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
     flake-parts = {
       url = "github:hercules-ci/flake-parts";
       inputs.nixpkgs-lib.follows = "nixpkgs";
     };
     tabnix = {
       url = "github:tabmadi/tabnix/v0.1.0";
       inputs.nixpkgs.follows = "nixpkgs";
       inputs.flake-parts.follows = "flake-parts";
     };
   };
   ```

2. Build the outputs with flake-parts, and import the module:

   ```nix
   outputs =
     inputs@{ flake-parts, tabnix, ... }:
     flake-parts.lib.mkFlake { inherit inputs; } {
       imports = [ tabnix.flakeModules.default ];
       perSystem = { pkgs, config, ... }: {
         tabnix.packages = [ pkgs.go ];
         checks.lint-go = config.tabnix.check "lint-go" [ pkgs.golangci-lint ] "golangci-lint run";
       };
     };
   ```

3. Keep the configs that the tools read in `.config/`: `cog.toml`, `dprint.json`, `gitleaks.toml`, and `rumdl.toml`.

## Configuration

| Option, under `perSystem` | Does |
| --- | --- |
| `tabnix.packages` | Adds the language tools of the repo to the dev shell |
| `tabnix.hooks` | Adds hooks in the git-hooks.nix format. Each one is enabled |
| `tabnix.compiler` | Puts a C compiler in the dev shell, for a cargo or cgo build |
| `tabnix.tablint` | Sets the tablint binary. The tablint repo sets its own build |
| `tabnix.check` | Read only. Makes a check from a name, a list of tools, and a command |

## Decisions

The ADRs in [docs/adr](docs/adr/README.md) record every decision and the reason for it.

## License

Apache License 2.0. See [LICENSE](LICENSE).
