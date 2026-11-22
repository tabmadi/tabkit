# The check of the bun module of ADR-0400. A fixture flake runs its tests with the locked node_modules, offline.
{
  pkgs,
  inputs,
  module,
  bun,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  fixture =
    inputs.flake-parts.lib.mkFlake
      {
        inputs = inputs // {

          self = {
            outPath = ./fixtures/bun;
            inherit inputs;
          };

        };
      }
      {
        systems = [ system ];
        imports = [
          module
          bun
        ];
        perSystem =
          { config, pkgs, ... }:
          {
            # The check runs the Bun of tabkit.bun.package, and not the one that the hook of bun2nix brings.
            checks.test = config.tabkit.bun.check "test" [ ] ''
              test "$(bun --version)" = "${config.tabkit.bun.package.version}"
              bun test
            '';
          };
      };
in
fixture.checks.${system}.test
