# The template scope, per ADR-0400: the lint-scope check and the templates output.
# A template imports it only from its template/ folder, so a project made from the template never loads it.
{
  self,
  lib,
  config,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.tabkit.template;

  # Every file except the template/ folder and the given folders. `nix flake new -t` copies the result.
  projectScope =
    exclude:
    builtins.path {
      name = "project";
      path = self;
      filter =
        path: _type:
        !(lib.any (folder: lib.hasPrefix "${toString self}/${folder}" path) ([ "template" ] ++ exclude));
    };

  variantOptions = {
    options = {
      welcomeText = mkOption {
        type = types.lines;
        description = "The first steps in a new project of this variant, which `nix flake new -t` prints.";
      };
      exclude = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "The top-level folders that this variant leaves out, beside `template/`.";
      };
    };
  };
in
{
  options.tabkit.template = {
    description = mkOption {
      type = types.str;
      description = "The one-line description of the template, which `nix flake show` prints.";
    };
    welcomeText = mkOption {
      type = types.lines;
      description = "The first steps in a new project, which `nix flake new -t` prints.";
    };
    exclude = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "infra" ];
      description = "The top-level folders that the default template leaves out, beside `template/`, such as the folder of a variant.";
    };
    extraVoice = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "github\\.com/example" ];
      description = "More extended regular expressions that lint-scope refuses in `docs` and `AGENTS.md`, such as the name of the source repo.";
    };
    variants = mkOption {
      type = types.attrsOf (types.submodule variantOptions);
      default = { };
      description = "More templates beside the default, by name, such as one that keeps a folder that the default leaves out.";
    };
  };

  config = {
    perSystem =
      { pkgs, ... }:
      {
        # The copied files stay true in every copy.
        checks.lint-scope = pkgs.runCommand "lint-scope" { } ''
          cd ${self}
          voice=${
            lib.escapeShellArg (
              lib.concatStringsSep "|" (
                [
                  "this template"
                  "the template"
                  "made from this"
                  "generated (repo|project)"
                  "TPL-[0-9]"
                ]
                ++ cfg.extraVoice
              )
            )
          }
          if grep -rniE "$voice" docs AGENTS.md; then
            echo "A project doc speaks as the template or names another repo." >&2
            exit 1
          fi
          if grep -rnE --include='*.md' --exclude-dir=template '\]\((\./|\.\./)*template/' .; then
            echo "A copied file links into template/, and the link breaks in every copy." >&2
            exit 1
          fi
          touch $out
        '';
      };

    flake.templates = {
      default = {
        path = projectScope cfg.exclude;
        inherit (cfg) description welcomeText;
      };
    }
    // lib.mapAttrs (_: variant: {
      path = projectScope variant.exclude;
      inherit (cfg) description;
      inherit (variant) welcomeText;
    }) cfg.variants;
  };
}
