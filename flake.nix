# tabkit ships its binary and the shared toolchain as a flake-parts module, and checks itself with both, per ADR-0200.
{
  description = "Opinionated checks, generators, and a Nix toolchain for a repository";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The bun module of ADR-0400. bun.nix lists every package of bun.lock, so a check installs them without network.
    bun2nix = {
      url = "github:nix-community/bun2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The deploy module of ADR-0500. A deploy that loses SSH rolls back by itself.
    deploy-rs = {
      url = "github:serokell/deploy-rs";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The nix-cache module of ADR-0600 imports the NixOS module of niks3.
    niks3 = {
      url = "github:Mic92/niks3";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The microvm-host module of ADR-0500 runs each guest with microvm.nix.
    microvm = {
      url = "github:microvm-nix/microvm.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { flake-parts-lib, ... }:
      let
        module = flake-parts-lib.importApply ./nix/modules/default.nix inputs;
        bun = flake-parts-lib.importApply ./nix/modules/bun.nix inputs;
        template = ./nix/modules/template.nix;
        go = ./nix/modules/go.nix;
        shell = ./nix/modules/shell.nix;
        deploy = flake-parts-lib.importApply ./nix/modules/deploy.nix inputs;
        microvmHost = flake-parts-lib.importApply ./nix/nixos/microvm-host.nix inputs;
        nixCache = flake-parts-lib.importApply ./nix/nixos/nix-cache.nix inputs;
      in
      {
        imports = [ module ];
        flake.flakeModules = {
          default = module;
          # The opt-in modules of ADR-0400. A repo imports one only when it has the part.
          inherit
            bun
            deploy
            go
            shell
            template
            ;
        };

        # The NixOS modules of ADR-0300 and ADR-0500. A repo imports one only on a machine that it runs.
        flake.nixosModules = {
          mtls-gate = ./nix/nixos/mtls-gate.nix;
          server = ./nix/nixos/server.nix;
          microvm-host = microvmHost;
          nix-cache = nixCache;
          openbao = ./nix/nixos/openbao.nix;
          cloudflare-origin = ./nix/nixos/cloudflare-origin.nix;
          monitoring = ./nix/nixos/monitoring.nix;
          sops-guests = ./nix/nixos/sops-guests.nix;
          postgres = ./nix/nixos/postgres.nix;
          temporal = ./nix/nixos/temporal.nix;
        };

        # The checks of the modules live here, not in the module that every repo imports. The nixosTests boot VMs.
        perSystem =
          { pkgs, lib, ... }:
          let
            cargoToml = fromTOML (builtins.readFile ./Cargo.toml);

            # The binary. Building it also runs `cargo test`, with the fixture tests.
            tabkitPackage = pkgs.rustPlatform.buildRustPackage {
              pname = "tabkit";
              inherit (cargoToml.workspace.package) version;
              src = lib.fileset.toSource {
                root = ./.;
                fileset = lib.fileset.unions [
                  ./Cargo.toml
                  ./Cargo.lock
                  ./crates
                ];
              };
              cargoLock.lockFile = ./Cargo.lock;
              cargoBuildFlags = [
                "--package"
                "tabkit"
              ];
              # The just/doc-comment rule reads the recipes through `just`, and a fixture covers it.
              nativeCheckInputs = [ pkgs.just ];
              meta.mainProgram = "tabkit";
            };

            # The platform commands, in their own package, so a small repo never builds or downloads them.
            tabkitPlatform = tabkitPackage.overrideAttrs (_: {
              pname = "tabkit-platform";
              cargoBuildFlags = [
                "--package"
                "tabkit-ops"
              ];
              cargoCheckFlags = [
                "--package"
                "tabkit-ops"
              ];
              # A CI gate shell builds this package on a cold runner, so the tests run in the `build-platform` check.
              doCheck = false;
              meta.mainProgram = "tabkit-ops";
            });
          in
          {
            tabkit = {
              packages = with pkgs; [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
              ];
              compiler = true;
              hooks.rust = {
                name = "cargo fmt";
                entry = "${pkgs.writeShellScript "cargo-fmt-check" ''
                  export PATH=${pkgs.cargo}/bin:${pkgs.rustfmt}/bin:$PATH
                  exec cargo fmt --check
                ''}";
                files = "\\.rs$";
                pass_filenames = false;
              };
            };

            checks = {
              build = tabkitPackage;
              build-platform = tabkitPlatform.overrideAttrs (_: {
                doCheck = true;
              });
              # The embedded scripts follow the same shell rules as a repo that keeps its own.
              lint-ops =
                pkgs.runCommand "lint-ops"
                  {
                    nativeBuildInputs = [
                      pkgs.shellcheck
                      pkgs.shfmt
                    ];
                  }
                  ''
                    cd ${./crates/ops/scripts}
                    shellcheck --external-sources --source-path=SCRIPTDIR --exclude=SC1091 -x --severity=warning lib/*.sh
                    shfmt -d -i 2 .
                    touch $out
                  '';
              lint-rust = tabkitPackage.overrideAttrs (old: {
                pname = "lint-rust";
                nativeBuildInputs = old.nativeBuildInputs ++ [
                  pkgs.clippy
                  pkgs.rustfmt
                ];
                buildPhase = ''
                  cargo fmt --check
                  cargo clippy --workspace --all-targets --locked --offline -- -D warnings
                '';
                doCheck = false;
                installPhase = "touch $out";
              });
              module-template = import ./nix/tests/template.nix {
                inherit pkgs inputs template;
              };
              module-bun = import ./nix/tests/bun.nix {
                inherit
                  pkgs
                  inputs
                  module
                  bun
                  ;
              };
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              module-go = import ./nix/tests/go.nix {
                inherit
                  pkgs
                  inputs
                  module
                  go
                  ;
              };
              module-shell = import ./nix/tests/shell.nix {
                inherit
                  pkgs
                  inputs
                  module
                  shell
                  ;
              };
              module-deploy = import ./nix/tests/deploy.nix {
                inherit pkgs inputs deploy;
              };
              mtls-gate = import ./nix/tests/mtls-gate.nix {
                inherit pkgs;
                module = ./nix/nixos/mtls-gate.nix;
              };
              nix-cache = import ./nix/tests/nix-cache.nix {
                inherit pkgs;
                module = nixCache;
              };
              cloudflare-origin = import ./nix/tests/cloudflare-origin.nix {
                inherit pkgs;
                module = ./nix/nixos/cloudflare-origin.nix;
              };
              sops-guests = import ./nix/tests/sops-guests.nix {
                inherit pkgs;
                module = ./nix/nixos/sops-guests.nix;
              };
              monitoring = import ./nix/tests/monitoring.nix {
                inherit pkgs;
                module = ./nix/nixos/monitoring.nix;
              };
              temporal = import ./nix/tests/temporal.nix {
                inherit pkgs;
                module = ./nix/nixos/temporal.nix;
                postgres = ./nix/nixos/postgres.nix;
              };
              openbao = import ./nix/tests/openbao.nix {
                inherit pkgs;
                module = ./nix/nixos/openbao.nix;
              };
              postgres = import ./nix/tests/postgres.nix {
                inherit pkgs;
                module = ./nix/nixos/postgres.nix;
              };
              server = import ./nix/tests/server.nix {
                inherit pkgs;
                module = ./nix/nixos/server.nix;
              };
            };

            # The nested test of ADR-0500 boots guests inside a test VM. It is not a CI check: a GitHub runner is a
            # VM itself, and a guest at that depth does not boot. `just ci` builds it on a Linux machine with KVM.
            packages = {
              default = tabkitPackage;
              tabkit-platform = tabkitPlatform;
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              nested-microvm-host = import ./nix/tests/microvm-host.nix {
                inherit pkgs;
                module = microvmHost;
              };
            };
          };
      }
    );
}
