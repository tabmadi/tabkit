# tabkit

Opinionated checks, generators, and a Nix toolchain for a repository. A repo imports one [flake-parts](https://flake.parts) module at one tag, and adds only its language tools.

tabkit has one maintainer. It is free to use under the Apache License 2.0, and it makes no promise of support or compatibility. Every version is 0.y.z, and any release may break your gate.

## What it does

| Part | Holds |
| --- | --- |
| Dev shell | `just`, cocogitto, rumdl, gitleaks, dprint, the `tabkit` binary, `commit-range`, nixd, and nixfmt, plus the tools of the repo |
| Git hooks | tabkit, rumdl, dprint, nixfmt, `just --fmt`, gitleaks, `cog verify`, `cog check`, and `just ci` |
| Checks | `lint-prose`, `lint-md`, `lint-secrets`, `lint-data`, `lint-nix`, `lint-just`, and `lint-workflows` |
| `formatter` | nixfmt |
| `systems` | Linux, on x86-64 and ARM. Linux is the one supported platform |
| Opt-in flake modules | `template` adds the `templates` output and the `lint-scope` check. `bun`, `go`, and `shell` serve a repo with that part. `deploy` serves a repo that deploys its own machines. Each one has a check |
| NixOS modules | `server`, `microvm-host`, `mtls-gate`, `nix-cache`, `postgres`, `openbao`, `temporal`, and `monitoring`, for the machines that a repo runs. Each one has a nixosTest |
| Binary cache | `nix-cache-push` in the dev shell, which `just cache-push` runs after the CI gate. The variables `NIX_CACHE_URL` and `NIX_CACHE_PUBLIC_KEY` of a repo turn it on |

## Getting started

1. Add flake-parts and tabkit as inputs, with tabkit at a release tag:

   ```nix
   inputs = {
     nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
     flake-parts = {
       url = "github:hercules-ci/flake-parts";
       inputs.nixpkgs-lib.follows = "nixpkgs";
     };
     tabkit = {
       url = "github:tabmadi/tabkit/v0.16.0";
       inputs.nixpkgs.follows = "nixpkgs";
       inputs.flake-parts.follows = "flake-parts";
     };
   };
   ```

2. Build the outputs with flake-parts, and import the module:

   ```nix
   outputs =
     inputs@{ flake-parts, tabkit, ... }:
     flake-parts.lib.mkFlake { inherit inputs; } {
       imports = [ tabkit.flakeModules.default ];
       perSystem = { pkgs, config, ... }: {
         tabkit.packages = [ pkgs.go ];
         checks.lint-go = config.tabkit.check "lint-go" [ pkgs.golangci-lint ] "golangci-lint run";
       };
     };
   ```

3. Keep the configs that the tools read in `.config/`: `cog.toml`, `dprint.json`, `gitleaks.toml`, and `rumdl.toml`.

## Configuration

| Option, under `perSystem` | Does |
| --- | --- |
| `tabkit.packages` | Adds the language tools of the repo to the dev shell |
| `tabkit.hooks` | Adds hooks in the git-hooks.nix format. Each one is enabled |
| `tabkit.compiler` | Puts a C compiler in the dev shell, for a cargo or cgo build |
| `tabkit.env` | Sets environment variables in the dev shell, beside the ones that tabkit sets |
| `tabkit.rules.enable` | Turns tabkit on or off. The default is on |
| `tabkit.rules.package` | Sets the tabkit binary |
| `tabkit.check` | Read only. Makes a check from a name, a list of tools, and a command |

## Opt-in flake modules

A repo imports one of these beside the default module only when it has the part. [ADR-0400](docs/adr/0400-opt-in-flake-modules.md) lists the options of each module.

`template` adds the `templates` output and the `lint-scope` check. A repo imports it from its `template/` folder, so a copy of the repo never loads it:

```nix
# template/scope.nix
{ inputs, ... }:
{
  imports = [ inputs.tabkit.flakeModules.template ];
  tabkit.template = {
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
  tabkit.flakeModules.default
  tabkit.flakeModules.bun
];
perSystem = { pkgs, config, ... }: {
  checks.test = config.tabkit.bun.check "test" [ pkgs.bun ] "bun test";
};
```

`deploy` deploys each `nixosConfiguration` of the repo with deploy-rs. A deploy that loses SSH rolls back by itself:

```nix
imports = [ tabkit.flakeModules.deploy ];
tabkit.deploy.nodes.host.hostname = "203.0.113.10";
```

## NixOS modules

A repo that runs its own machines on NixOS imports a module in its host configuration. The toolchain module never loads them. [ADR-0300](docs/adr/0300-nixos-modules.md) and [ADR-0500](docs/adr/0500-self-hosted-machines.md) list the options of each module.

`server` sets SSH with keys only, the nftables firewall, and Nix with flakes. `microvm-host` runs each guest as a microVM with one routed tap, and the host drops every connection between guests that no flow declares:

```nix
modules = [
  tabkit.nixosModules.server
  tabkit.nixosModules.microvm-host
  {
    tabkit.server = {
      enable = true;
      adminKeys = [ "ssh-ed25519 AAAA... admin" ];
    };
    tabkit.microvmHost = {
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
    tabkit.nixosModules.mtls-gate
    {
      networking.nftables.enable = true;
      tabkit.mtlsGate.signer = {
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
  tabkit.nixosModules.nix-cache
  {
    tabkit.nixCache = {
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

## Checks and generators

The `tabkit` binary is in the dev shell of every repo that imports the module.

### Rule groups

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
| `frontend` | Message catalogues agree, classes are direction-neutral, tokens meet WCAG contrast, and fixtures are typed against an SDK |
| `sql`, `alert`, `api`, `edge`, `naming` | Personal-data tags, alert severities, API audience and prefixes, edge access rules, and resource names |
| `helm`, `gitops`, `service`, `project` | Chart versions, environment overlays, the artifacts of each service, and the identity of a copied project |
| `go`, `money` | Go call layout, Temporal activity registration, declared service dependencies, and the money type in specs, Go, and TypeScript |
| `kube` | Rendered workloads fit the limit ranges and quotas of their namespaces |
| `workflow` | A workflow step calls a `just` recipe and holds no pipeline logic |
| `image` | An image reference or a workflow action does not float |
| `script` | A script prints status through its log library |
| `version` | `flake.nix` and `go.mod` or `pyproject.toml` name the same version line |
| `just` | Every public recipe has a doc comment |

`comment/workaround` checks that a workaround names its pin or a recent check date. Each rule acts only when its input exists. `tabkit rules` lists every rule ID, and `tabkit gen --list` lists every generator. `tabkit fix` applies the safe fixes. `tabkit affected` prints the services, apps, and libraries that a diff touches, as JSON. Comments are read in Go, TypeScript, JavaScript, Rust, Python, GDScript, shell, TOML, YAML, and SQL.

### Getting started

Install tabkit with cargo, at a release tag:

```bash
cargo install --locked --git https://github.com/tabmadi/tabkit --tag v0.8.0
```

Or add it as a flake input:

```nix
inputs.tabkit = {
  url = "github:tabmadi/tabkit/v0.8.0";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then run `tabkit check` at the root of a repository. Each finding is one line: `path:line: rule: message`.

### Configuration

tabkit reads `.config/tabkit.toml`. An empty or missing file runs every rule with its defaults. [Configuration](docs/reference/configuration.md) lists the keys.

## Platform commands

A platform repo adds the `tabkit-platform` package to its dev shell. `tabkit-ops list` prints the commands, and `tabkit-ops <command>` runs one in the current repository. A small repo never builds this package, per [ADR-1200](docs/adr/1200-platform-commands.md).

## Decisions

The ADRs in [docs/adr](docs/adr/README.md) record every decision and the reason for it.

## License

Apache License 2.0. See [LICENSE](LICENSE).
