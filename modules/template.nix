# The template scope, per ADR-0400: the lint-scope check and the templates output of a template repo.
# A template imports it only from its template/ folder, so a project made from the template never loads it.
{
  self,
  lib,
  config,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.tabnix.template;

  # Every file except the template/ folder. `nix flake new -t` copies the result into a new project.
  projectScope = builtins.path {
    name = "project";
    path = self;
    filter = path: _type: !(lib.hasPrefix "${toString self}/template" path);
  };
in
{
  options.tabnix.template = {
    description = mkOption {
      type = types.str;
      description = "The one-line description of the template, which `nix flake show` prints.";
    };
    welcomeText = mkOption {
      type = types.lines;
      description = "The first steps in a new project, which `nix flake new -t` prints.";
    };
  };

  config = {
    perSystem =
      { pkgs, ... }:
      {
        # The copied files stay true in every copy.
        checks.lint-scope = pkgs.runCommand "lint-scope" { } ''
          cd ${self}
          voice='github\.com/tabmadi|blueprints|this template|the template|made from this|generated (repo|project)|TPL-[0-9]'
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

    flake.templates.default = {
      path = projectScope;
      inherit (cfg) description welcomeText;
    };
  };
}
