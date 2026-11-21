# The check of the shell module of ADR-0800. A fixture flake must hold both tools and the hook at the set indent.
{
  pkgs,
  inputs,
  module,
  shell,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  fixture = flake {
    tabnix.shell.indent = 4;
  };
  # A repo can turn tabrules off, and then no check or hook names it.
  withoutTabrules = flake { tabnix.tabrules.enable = false; };

  flake =
    settings:
    inputs.flake-parts.lib.mkFlake
      {
        inputs = inputs // {

          self = {
            outPath = ./fixtures/go;
            inherit inputs;
          };

        };
      }
      {
        systems = [ system ];
        imports = [
          module
          shell
        ];
        perSystem =
          { config, ... }:
          {
            imports = [ settings ];
            legacyPackages.fixture = { inherit (config.tabnix) packages hooks; };
          };
      };

  tabnix = fixture.legacyPackages.${system}.fixture;
in
assert builtins.elem pkgs.shfmt tabnix.packages;
assert builtins.elem pkgs.shellcheck tabnix.packages;
assert pkgs.lib.hasSuffix "--diff -i 4" tabnix.hooks.shfmt.entry;
assert fixture.checks.${system} ? lint-prose;
assert !(withoutTabrules.checks.${system} ? lint-prose);
pkgs.runCommand "module-shell" { } "touch $out"
