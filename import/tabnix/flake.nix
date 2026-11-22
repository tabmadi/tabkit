# tabnix exports the shared toolchain as a flake-parts module, and checks itself with it, per ADR-0200.
{
  description = "A Nix toolchain with opinionated defaults: the dev shell, the git hooks, and the checks";

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
    tabrules = {
      url = "github:tabmadi/tabrules/v0.8.0";
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
        module = flake-parts-lib.importApply ./modules/default.nix inputs;
        bun = flake-parts-lib.importApply ./modules/bun.nix inputs;
        template = ./modules/template.nix;
        go = ./modules/go.nix;
        shell = ./modules/shell.nix;
        deploy = flake-parts-lib.importApply ./modules/deploy.nix inputs;
        microvmHost = flake-parts-lib.importApply ./nixos/microvm-host.nix inputs;
        nixCache = flake-parts-lib.importApply ./nixos/nix-cache.nix inputs;
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
          mtls-gate = ./nixos/mtls-gate.nix;
          server = ./nixos/server.nix;
          microvm-host = microvmHost;
          nix-cache = nixCache;
        };

        # The checks of the modules live here, not in the module that every repo imports. The nixosTests boot VMs.
        perSystem =
          { pkgs, lib, ... }:
          {
            checks = {
              module-template = import ./tests/template.nix {
                inherit pkgs inputs template;
              };
              module-bun = import ./tests/bun.nix {
                inherit
                  pkgs
                  inputs
                  module
                  bun
                  ;
              };
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              module-go = import ./tests/go.nix {
                inherit
                  pkgs
                  inputs
                  module
                  go
                  ;
              };
              module-shell = import ./tests/shell.nix {
                inherit
                  pkgs
                  inputs
                  module
                  shell
                  ;
              };
              module-deploy = import ./tests/deploy.nix {
                inherit pkgs inputs deploy;
              };
              mtls-gate = import ./tests/mtls-gate.nix {
                inherit pkgs;
                module = ./nixos/mtls-gate.nix;
              };
              nix-cache = import ./tests/nix-cache.nix {
                inherit pkgs;
                module = nixCache;
              };
              server = import ./tests/server.nix {
                inherit pkgs;
                module = ./nixos/server.nix;
              };
            };

            # The nested test of ADR-0500 boots guests inside a test VM. It is not a CI check: a GitHub runner is a
            # VM itself, and a guest at that depth does not boot. `just ci` builds it on a Linux machine with KVM.
            packages = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              nested-microvm-host = import ./tests/microvm-host.nix {
                inherit pkgs;
                module = microvmHost;
              };
            };
          };
      }
    );
}
