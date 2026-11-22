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
    tabkit.shell.indent = 4;
  };
  # A repo can turn tabkit off, and then no check or hook names it.
  withoutRules = flake { tabkit.rules.enable = false; };

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
            legacyPackages.fixture = { inherit (config.tabkit) packages hooks; };
          };
      };

  tabkit = fixture.legacyPackages.${system}.fixture;
in
assert builtins.elem pkgs.shfmt tabkit.packages;
assert builtins.elem pkgs.shellcheck tabkit.packages;
assert pkgs.lib.hasSuffix "--diff -i 4" tabkit.hooks.shfmt.entry;
assert fixture.checks.${system} ? lint-prose;
assert !(withoutRules.checks.${system} ? lint-prose);
pkgs.runCommand "module-shell" { } "touch $out"
