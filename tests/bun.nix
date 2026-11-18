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
            checks.test = config.tabnix.bun.check "test" [ pkgs.bun ] "bun test";
          };
      };
in
fixture.checks.${system}.test
