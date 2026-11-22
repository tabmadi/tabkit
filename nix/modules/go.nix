# The Go parts, per ADR-0700: the ruleguard rules, and a golangci-lint that finds them from a config in the repo.
{ lib, ... }:
let
  inherit (lib) mkOption types;
  placeholder = "@tabkit-go-rules@";
in
{
  perSystem =
    { config, pkgs, ... }:
    let
      cfg = config.tabkit.go;
      rules = ./go-rules.go;

      # golangci-lint expands only its own path variables in a config, so the wrapper writes the store path into a copy.
      wrapper = pkgs.writeShellScriptBin "golangci-lint" ''
        args=()
        config=""
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --config | -c)
              config=$2
              shift 2
              ;;
            --config=*)
              config=''${1#--config=}
              shift
              ;;
            *)
              args+=("$1")
              shift
              ;;
          esac
        done
        if [ -z "$config" ]; then
          exec ${lib.getExe cfg.package} "''${args[@]}"
        fi
        copy=$(${pkgs.coreutils}/bin/mktemp --suffix=.yml)
        trap '${pkgs.coreutils}/bin/rm -f "$copy"' EXIT
        ${pkgs.gnused}/bin/sed 's#${placeholder}#${rules}#g' "$config" > "$copy"
        # The subcommand comes first, because a flag of a subcommand is not a flag of the root command.
        ${lib.getExe cfg.package} "''${args[@]}" --config "$copy"
      '';
    in
    {
      options.tabkit.go = {
        package = mkOption {
          type = types.package;
          default = pkgs.golangci-lint;
          defaultText = lib.literalExpression "pkgs.golangci-lint";
          description = "The golangci-lint that the wrapper runs. A repo sets one that is built with its Go line.";
        };
        golangci = mkOption {
          type = types.package;
          readOnly = true;
          default = wrapper;
          description = "Read only. The wrapper, for a check of the repo. It replaces `${placeholder}` in the config.";
        };
        rules = mkOption {
          type = types.path;
          readOnly = true;
          default = rules;
          description = "Read only. The ruleguard rules file.";
        };
      };

      config.tabkit.packages = [ wrapper ];
    };
}
