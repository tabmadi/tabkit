# tabnix exports the shared toolchain as a flake-parts module, and checks itself with it, per ADR-0200.
{
  description = "The shared Nix toolchain of the owner's repos: the dev shell, the git hooks, and the checks";

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
    tablint = {
      url = "github:tabmadi/tablint/v0.3.0";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.git-hooks.follows = "git-hooks";
    };
    # The bun module of ADR-0400. bun.nix lists every package of bun.lock, so a check installs them without network.
    bun2nix = {
      url = "github:nix-community/bun2nix";
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
      in
      {
        imports = [ module ];
        flake.flakeModules = {
          default = module;
          # The opt-in modules of ADR-0400. A repo imports one only when it has the part.
          inherit bun template;
        };

        # The NixOS modules of ADR-0300. A repo imports one only on a machine that it runs.
        flake.nixosModules.mtls-gate = ./nixos/mtls-gate.nix;

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
              mtls-gate = import ./tests/mtls-gate.nix {
                inherit pkgs;
                module = ./nixos/mtls-gate.nix;
              };
            };
          };
      }
    );
}
