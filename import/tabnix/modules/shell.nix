# The shell parts, per ADR-0800: shfmt and shellcheck in the dev shell, and the hook that keeps scripts formatted.
{ lib, ... }:
let
  inherit (lib) mkOption types;
in
{
  perSystem =
    { config, pkgs, ... }:
    let
      cfg = config.tabnix.shell;
    in
    {
      options.tabnix.shell.indent = mkOption {
        type = types.ints.unsigned;
        default = 2;
        description = "The indent of shfmt, in spaces.";
      };

      config.tabnix = {
        packages = [
          pkgs.shfmt
          pkgs.shellcheck
        ];
        hooks.shfmt = {
          name = "shfmt";
          entry = "${pkgs.shfmt}/bin/shfmt --diff -i ${toString cfg.indent}";
          files = "\\.sh$";
        };
      };
    };
}
