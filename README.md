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
| Opt-in flake modules | `template`, for a template repo, and `bun`, for a repo on Bun. Each one has a check |
| NixOS modules | `mtls-gate`, for the machines that a repo runs. Each one has a nixosTest |

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
       url = "github:tabmadi/tabnix/v0.4.0";
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
| `tabnix.env` | Sets environment variables in the dev shell, beside the ones that tabnix sets |
| `tabnix.tablint` | Sets the tablint binary. The tablint repo sets its own build |
| `tabnix.check` | Read only. Makes a check from a name, a list of tools, and a command |

## Opt-in flake modules

A repo imports one of these beside the default module only when it has the part. [ADR-0400](docs/adr/0400-opt-in-flake-modules.md) lists the options of each module.

`template` adds the `lint-scope` check and the `templates` output to a template repo. The template imports it from its `template/` folder, so a project made from it never loads it:

```nix
# template/scope.nix
{ inputs, ... }:
{
  imports = [ inputs.tabnix.flakeModules.template ];
  tabnix.template = {
    description = "A Go application";
    welcomeText = ''
      # A new project

      1. Run `nix develop`.
    '';
  };
}
```

`bun` puts Bun and bun2nix in the dev shell, and adds the hook that keeps `bun.nix` in step with `bun.lock`. Its check helper gives a check the locked `node_modules`:

```nix
imports = [
  tabnix.flakeModules.default
  tabnix.flakeModules.bun
];
perSystem = { pkgs, config, ... }: {
  checks.test = config.tabnix.bun.check "test" [ pkgs.bun ] "bun test";
};
```

## NixOS modules

A repo that runs its own machines on NixOS imports a module in its host configuration. The toolchain module never loads them. [ADR-0300](docs/adr/0300-nixos-modules.md) lists the options of each module.

`mtls-gate` puts nginx in front of a local service. A request reaches the service only with a client certificate that one CA signs, and only from a listed address:

```nix
nixosConfigurations.host = nixpkgs.lib.nixosSystem {
  system = "x86_64-linux";
  modules = [
    tabnix.nixosModules.mtls-gate
    {
      networking.nftables.enable = true;
      tabnix.mtlsGate.signer = {
        address = "10.20.0.30";
        port = 8443;
        upstream = "http://127.0.0.1:8080";
        certificate = ./pki/signer.crt;
        key = "/run/secrets/signer.key";
        clientCa = ./pki/ca.crt;
        allowFrom = [ "10.20.0.10" ];
      };
    }
  ];
};
```

The key is a path at run time, such as a file that sops-nix writes. The module refuses a key path in the Nix store.

## Decisions

The ADRs in [docs/adr](docs/adr/README.md) record every decision and the reason for it.

## License

Apache License 2.0. See [LICENSE](LICENSE).
