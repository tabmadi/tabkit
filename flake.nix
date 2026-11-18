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
      url = "github:tabmadi/tablint/v0.2.0";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.git-hooks.follows = "git-hooks";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { flake-parts-lib, ... }:
      let
        module = flake-parts-lib.importApply ./modules/default.nix inputs;
      in
      {
        imports = [ module ];
        flake.flakeModules.default = module;

        # The NixOS modules of ADR-0300. A repo imports one only on a machine that it runs.
        flake.nixosModules.mtls-gate = ./nixos/mtls-gate.nix;

        # The nixosTests boot VMs, so they live here and not in the module that every repo imports.
        perSystem =
          { pkgs, lib, ... }:
          {
            checks = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              mtls-gate = import ./tests/mtls-gate.nix {
                inherit pkgs;
                module = ./nixos/mtls-gate.nix;
              };
            };
          };
      }
    );
}
