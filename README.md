# tabnix

A Nix toolchain with opinionated defaults, as a [flake-parts](https://flake.parts) module. A repo imports it and adds only its language tools.

tabnix has one maintainer. It is free to use under the Apache License 2.0, and it makes no promise of support or compatibility. Every version is 0.y.z, and any release may break your gate.

## What it does

| Part | Holds |
| --- | --- |
| Dev shell | `just`, cocogitto, rumdl, gitleaks, dprint, tabrules, `commit-range`, nixd, and nixfmt, plus the tools of the repo |
| Git hooks | tabrules, rumdl, dprint, nixfmt, `just --fmt`, gitleaks, `cog verify`, `cog check`, and `just ci` |
| Checks | `lint-prose`, `lint-md`, `lint-secrets`, `lint-data`, `lint-nix`, `lint-just`, and `lint-workflows` |
| `formatter` | nixfmt |
| `systems` | Linux, on x86-64 and ARM. Linux is the one supported platform |
| Opt-in flake modules | `template` adds the `templates` output and the `lint-scope` check. `bun`, `go`, and `shell` serve a repo with that part. `deploy` serves a repo that deploys its own machines. Each one has a check |
| NixOS modules | `server`, `microvm-host`, `mtls-gate`, and `nix-cache`, for the machines that a repo runs. Each one has a nixosTest |
| Binary cache | `nix-cache-push` in the dev shell, which `just cache-push` runs after the CI gate. The variables `NIX_CACHE_URL` and `NIX_CACHE_PUBLIC_KEY` of a repo turn it on |

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
       url = "github:tabmadi/tabnix/v0.4.1";
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
| `tabnix.tabrules.enable` | Turns tabrules on or off. The default is on |
| `tabnix.tabrules.package` | Sets the tabrules binary |
| `tabnix.check` | Read only. Makes a check from a name, a list of tools, and a command |

## Opt-in flake modules

A repo imports one of these beside the default module only when it has the part. [ADR-0400](docs/adr/0400-opt-in-flake-modules.md) lists the options of each module.

`template` adds the `templates` output and the `lint-scope` check. A repo imports it from its `template/` folder, so a copy of the repo never loads it:

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

`deploy` deploys each `nixosConfiguration` of the repo with deploy-rs. A deploy that loses SSH rolls back by itself:

```nix
imports = [ tabnix.flakeModules.deploy ];
tabnix.deploy.nodes.host.hostname = "203.0.113.10";
```

## NixOS modules

A repo that runs its own machines on NixOS imports a module in its host configuration. The toolchain module never loads them. [ADR-0300](docs/adr/0300-nixos-modules.md) and [ADR-0500](docs/adr/0500-self-hosted-machines.md) list the options of each module.

`server` sets SSH with keys only, the nftables firewall, and Nix with flakes. `microvm-host` runs each guest as a microVM with one routed tap, and the host drops every connection between guests that no flow declares:

```nix
modules = [
  tabnix.nixosModules.server
  tabnix.nixosModules.microvm-host
  {
    tabnix.server = {
      enable = true;
      adminKeys = [ "ssh-ed25519 AAAA... admin" ];
    };
    tabnix.microvmHost = {
      enable = true;
      gateway = "10.20.0.1";
      uplink = "eth0";
      guests.app = {
        address = "10.20.0.10";
        mac = "02:00:00:00:00:10";
        egress = true;
        publish = [ 443 ];
        config = ./guests/app.nix;
      };
      guests.db = {
        address = "10.20.0.20";
        mac = "02:00:00:00:00:20";
        secrets."db.env" = "/run/secrets/db.env";
        config = ./guests/db.nix;
      };
      flows = [
        {
          from = "app";
          to = "db";
          ports = [ 5432 ];
        }
      ];
    };
  }
];
```

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

## The binary cache

`nix-cache` serves the paths that CI builds, from a bucket on SeaweedFS, and mirrors the public cache for one network. [ADR-0600](docs/adr/0600-binary-cache.md) lists its options:

```nix
modules = [
  tabnix.nixosModules.nix-cache
  {
    tabnix.nixCache = {
      enable = true;
      domain = "cache.example.com";
      githubOwners = [ "example" ];
      signingKeyFile = "/run/secrets/nix-cache.key";
      s3.accessKeyFile = "/run/secrets/s3-access";
      s3.secretKeyFile = "/run/secrets/s3-secret";
      mirror.domain = "mirror.example.com";
      mirror.signingKeyFile = "/run/secrets/nix-mirror.key";
    };
  }
];
```

The host needs DNS names for `domain`, `s3.<domain>`, and the mirror. A repo turns the cache on with the repository variables `NIX_CACHE_URL`, such as `https://cache.example.com`, and `NIX_CACHE_PUBLIC_KEY`. A workstation adds the same two values to `nix.conf`:

```text
extra-substituters = https://cache.example.com
extra-trusted-public-keys = cache.example.com-1:<public key>
```

## Decisions

The ADRs in [docs/adr](docs/adr/README.md) record every decision and the reason for it.

## License

Apache License 2.0. See [LICENSE](LICENSE).
